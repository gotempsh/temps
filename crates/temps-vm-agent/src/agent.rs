//! In-guest agent for Firecracker sandboxes (ADR-029 §5).
//!
//! Runs as PID 1 inside the microVM: mounts the pseudo-filesystems, then
//! serves exec/fs RPCs over vsock. Injected into every rootfs by the
//! host-side conversion pipeline as a static musl binary, so it works on
//! glibc, musl, and distroless images alike.
//!
//! Not yet a full init: orphaned grandchildren that outlive their exec
//! session accumulate as zombies until proper subreaping lands with the
//! PTY integration. Sandbox exec workloads don't daemonize in practice.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use temps_vm_agent::{read_frame, write_frame, Request, Response, AGENT_PORT, WORK_DIR};

pub fn run() {
    let is_init = std::process::id() == 1;
    if is_init {
        setup_system();
    }
    log(&format!(
        "temps-vm-agent starting (pid {}, vsock port {})",
        std::process::id(),
        AGENT_PORT
    ));
    serve();
}

/// Minimal boot-time setup when running as PID 1.
fn setup_system() {
    for dir in ["/proc", "/sys", "/dev", "/tmp", "/run", WORK_DIR] {
        let _ = std::fs::create_dir_all(dir);
    }
    mount("proc", "/proc", "proc");
    mount("sysfs", "/sys", "sysfs");
    mount("devtmpfs", "/dev", "devtmpfs");
    mount("tmpfs", "/tmp", "tmpfs");
    mount("tmpfs", "/run", "tmpfs");
    unsafe {
        let name = b"sandbox";
        libc::sethostname(name.as_ptr() as *const libc::c_char, name.len());
    }
    interface_up("lo");
    // eth0's address comes from the kernel's built-in IP autoconfig (the
    // host passes `ip=...` boot args); userspace only needs resolver config.
    if Path::new("/sys/class/net/eth0").exists() {
        let _ = std::fs::write(
            "/etc/resolv.conf",
            "nameserver 1.1.1.1\nnameserver 8.8.8.8\n",
        );
        let _ = std::fs::write("/etc/hosts", "127.0.0.1 localhost sandbox\n");
    }
}

/// `ip link set <name> up` without shelling out — guest images can't be
/// assumed to ship iproute2.
fn interface_up(name: &str) {
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return;
        }
        let mut ifr: libc::ifreq = std::mem::zeroed();
        for (i, b) in name.bytes().take(libc::IFNAMSIZ - 1).enumerate() {
            ifr.ifr_name[i] = b as libc::c_char;
        }
        // Cast: musl declares ioctl's request as c_int, glibc as c_ulong —
        // `as _` compiles against both.
        if libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut ifr) == 0 {
            ifr.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &ifr);
        }
        libc::close(fd);
    }
}

fn mount(src: &str, target: &str, fstype: &str) {
    let src = std::ffi::CString::new(src).unwrap();
    let target_c = std::ffi::CString::new(target).unwrap();
    let fstype = std::ffi::CString::new(fstype).unwrap();
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            target_c.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        // EBUSY = already mounted (e.g. kernel automounted devtmpfs) — fine.
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EBUSY) {
            log(&format!("mount {} failed: {}", target, err));
        }
    }
}

/// Serial console is our only log sink; failures to write are ignorable.
fn log(msg: &str) {
    println!("[temps-vm-agent] {}", msg);
}

// ── Vsock server ────────────────────────────────────────────────────────

fn serve() {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        log(&format!(
            "fatal: vsock socket: {}",
            std::io::Error::last_os_error()
        ));
        halt_forever();
    }
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    addr.svm_port = AGENT_PORT;
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        log(&format!(
            "fatal: vsock bind: {}",
            std::io::Error::last_os_error()
        ));
        halt_forever();
    }
    if unsafe { libc::listen(fd, 16) } != 0 {
        log(&format!(
            "fatal: vsock listen: {}",
            std::io::Error::last_os_error()
        ));
        halt_forever();
    }
    log("ready");

    loop {
        let conn: RawFd = unsafe { libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if conn < 0 {
            continue;
        }
        // A vsock stream fd behaves like any stream socket; UnixStream is
        // just an fd wrapper and gives us Read/Write + Drop-closes.
        let stream = unsafe { UnixStream::from_raw_fd(conn) };
        std::thread::spawn(move || handle_connection(stream));
    }
}

/// PID 1 must never exit — the kernel panics. Park instead of returning.
fn halt_forever() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn handle_connection(mut stream: UnixStream) {
    let payload = match read_frame(&mut stream) {
        Ok(p) => p,
        Err(_) => return,
    };
    let request: Request = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => {
            respond(
                &mut stream,
                &Response::Err {
                    message: format!("bad request: {}", e),
                },
            );
            return;
        }
    };

    let shutdown = matches!(request, Request::Shutdown);
    let response = handle_request(request);
    respond(&mut stream, &response);

    if shutdown {
        // Ack is on the wire; now bring the VM down. Guest-initiated reboot
        // exits the VMM cleanly — that's Firecracker's shutdown story.
        let _ = stream.flush();
        unsafe {
            libc::sync();
            libc::reboot(libc::LINUX_REBOOT_CMD_RESTART);
        }
    }
}

fn respond(stream: &mut UnixStream, response: &Response) {
    if let Ok(json) = serde_json::to_vec(response) {
        let _ = write_frame(stream, &json);
    }
}

fn handle_request(request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::Shutdown => Response::Ok,
        Request::Exec {
            cmd,
            env,
            cwd,
            user,
            timeout_secs,
        } => exec(cmd, env, cwd, user, timeout_secs),
        Request::WriteFile {
            path,
            data_hex,
            mode,
        } => write_file(&path, &data_hex, mode),
        Request::ReadFile { path } => match std::fs::read(&path) {
            Ok(data) => Response::File {
                data_hex: hex::encode(data),
            },
            Err(e) => Response::Err {
                message: format!("read {}: {}", path, e),
            },
        },
        Request::Mkdir { path, mode } => {
            match std::fs::create_dir_all(&path).and_then(|_| {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            }) {
                Ok(_) => Response::Ok,
                Err(e) => Response::Err {
                    message: format!("mkdir {}: {}", path, e),
                },
            }
        }
        Request::Kill { pattern, signal } => kill_matching(&pattern, signal),
    }
}

// ── Exec ────────────────────────────────────────────────────────────────

fn exec(
    cmd: Vec<String>,
    env: HashMap<String, String>,
    cwd: Option<String>,
    user: Option<u32>,
    timeout_secs: Option<u64>,
) -> Response {
    if cmd.is_empty() {
        return Response::Err {
            message: "empty command".to_string(),
        };
    }
    let cwd = cwd.unwrap_or_else(|| WORK_DIR.to_string());
    let _ = std::fs::create_dir_all(&cwd);

    let mut command = std::process::Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A sane default PATH: guest images vary, and PID 1's env is empty.
    if !env.contains_key("PATH") {
        command.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        );
    }
    command.env("HOME", "/root");
    for (k, v) in &env {
        command.env(k, v);
    }
    if let Some(uid) = user {
        use std::os::unix::process::CommandExt;
        command.uid(uid).gid(uid);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Response::Err {
                message: format!("spawn {}: {}", cmd[0], e),
            }
        }
    };

    // Drain both pipes on threads so neither side can fill and deadlock.
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if deadline.is_some_and(|d| Instant::now() > d) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break -1;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break -1,
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Response::Exec {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

// ── Files ───────────────────────────────────────────────────────────────

fn write_file(path: &str, data_hex: &str, mode: u32) -> Response {
    let data = match hex::decode(data_hex) {
        Ok(d) => d,
        Err(e) => {
            return Response::Err {
                message: format!("bad hex payload: {}", e),
            }
        }
    };
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, &data)
        .and_then(|_| std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)))
    {
        Ok(_) => Response::Ok,
        Err(e) => Response::Err {
            message: format!("write {}: {}", path, e),
        },
    }
}

// ── Kill by pattern ─────────────────────────────────────────────────────

/// pkill-alike over /proc: substring match against the full command line.
/// Guest images (distroless!) can't be assumed to ship pkill.
fn kill_matching(pattern: &str, signal: i32) -> Response {
    let own_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Response::Err {
            message: "no /proc".to_string(),
        };
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == 1 || pid == own_pid {
            continue;
        }
        let Ok(cmdline) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if cmdline.contains(pattern) {
            unsafe {
                libc::kill(pid as i32, signal);
            }
        }
    }
    Response::Ok
}
