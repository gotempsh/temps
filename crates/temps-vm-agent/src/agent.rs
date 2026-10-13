// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-guest agent for Firecracker sandboxes (ADR-029 §5).
//!
//! Runs as PID 1 inside the microVM: mounts the pseudo-filesystems, then
//! serves exec/fs RPCs over vsock. Injected into every rootfs by the
//! host-side conversion pipeline as a static musl binary, so it works on
//! glibc, musl, and distroless images alike.
//!
//! Interactive terminals (ADR-029 §5): at boot it also starts
//! `temps-pty-agent` under a supervisor and serves a second vsock listener
//! ([`PTY_PORT`]) that relays bytes to that agent's unix socket. Everything
//! about terminals — tabs, replay, reattach — lives in the PTY agent, so it
//! behaves identically to the Docker backend.
//!
//! Not yet a full init: orphaned grandchildren that outlive their exec
//! session accumulate as zombies until proper subreaping lands. The
//! supervised PTY agent reaps its own children; sandbox exec workloads don't
//! daemonize in practice.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use temps_vm_agent::{
    read_frame, relay, write_frame, Request, Response, AGENT_PORT, MAX_PTY_RELAYS, PTY_AGENT_BIN,
    PTY_AGENT_SOCKET, PTY_PORT, WORK_DIR,
};

/// `PATH` for processes this agent starts: PID 1's own environment is empty
/// and guest images vary, so every child gets the same sane default.
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Relays currently being served; bounded by [`MAX_PTY_RELAYS`].
static ACTIVE_PTY_RELAYS: AtomicUsize = AtomicUsize::new(0);

pub fn run() {
    let is_init = std::process::id() == 1;
    if is_init {
        setup_system();
    }
    log(&format!(
        "temps-vm-agent starting (pid {}, vsock port {}, pty port {})",
        std::process::id(),
        AGENT_PORT,
        PTY_PORT
    ));
    start_terminal_support();
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

/// Bind and listen on a vsock `port`, accepting connections from any CID.
fn listen_vsock(port: u32) -> Result<RawFd, String> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!("vsock socket: {}", std::io::Error::last_os_error()));
    }
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    addr.svm_port = port;
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("vsock bind on port {}: {}", port, err));
    }
    if unsafe { libc::listen(fd, 16) } != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("vsock listen on port {}: {}", port, err));
    }
    Ok(fd)
}

fn serve() {
    let fd = match listen_vsock(AGENT_PORT) {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("fatal: {}", e));
            halt_forever();
        }
    };
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

// ── Interactive terminals (ADR-029 §5) ──────────────────────────────────

/// Start the PTY agent supervisor and the terminal relay listener. Both are
/// best effort: a guest without the PTY agent (an older rootfs, or a host
/// that never staged the binary) still serves exec/fs RPCs normally, it just
/// has no terminals.
fn start_terminal_support() {
    if !Path::new(PTY_AGENT_BIN).exists() {
        log(&format!(
            "{} not present; interactive terminals are disabled in this guest",
            PTY_AGENT_BIN
        ));
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("pty-supervisor".to_string())
        .spawn(supervise_pty_agent)
    {
        log(&format!("could not start the PTY agent supervisor: {}", e));
        return;
    }
    match listen_vsock(PTY_PORT) {
        Ok(fd) => {
            let spawned = std::thread::Builder::new()
                .name("pty-listener".to_string())
                .spawn(move || serve_pty(fd));
            if let Err(e) = spawned {
                log(&format!("could not start the PTY relay listener: {}", e));
            }
        }
        Err(e) => log(&format!("terminal relay unavailable: {}", e)),
    }
}

/// Keep `temps-pty-agent` running. It owns every PTY in the guest, so the
/// terminal feature is dead without it: a crash is retried after a short
/// pause rather than leaving the guest with a listener that has nobody to
/// talk to.
fn supervise_pty_agent() {
    let socket_dir = Path::new(PTY_AGENT_SOCKET)
        .parent()
        .unwrap_or_else(|| Path::new("/run"));
    loop {
        secure_pty_agent_binary();
        // 0700 root: the agent's socket is the trust boundary (ADR-008), and
        // /run is tmpfs so this directory has to be re-created each boot.
        let _ = std::fs::create_dir_all(socket_dir);
        let _ = std::fs::set_permissions(socket_dir, std::fs::Permissions::from_mode(0o700));

        let started = Instant::now();
        let spawned = std::process::Command::new(PTY_AGENT_BIN)
            .args(["--socket", PTY_AGENT_SOCKET])
            .env_clear()
            .env("PATH", DEFAULT_PATH)
            .env("HOME", "/root")
            .current_dir("/")
            .stdin(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                log(&format!("temps-pty-agent started (pid {})", child.id()));
                match child.wait() {
                    Ok(status) => log(&format!(
                        "temps-pty-agent exited ({}) after {:?}; restarting",
                        status,
                        started.elapsed()
                    )),
                    Err(e) => log(&format!("waiting on temps-pty-agent failed: {}", e)),
                }
            }
            Err(e) => log(&format!("could not start {}: {}", PTY_AGENT_BIN, e)),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Make the PTY agent binary and its directory root-owned and not writable
/// by anyone else, before every start.
///
/// The host builds the rootfs without privileges, so injected files carry the
/// *host* user's uid. Left like that, a guest account that happens to share
/// the uid could replace the binary this PID 1 then runs as root. Running as
/// root here, normalizing ownership closes that gap on every boot.
fn secure_pty_agent_binary() {
    let bin = Path::new(PTY_AGENT_BIN);
    for path in [Some(bin), bin.parent()].into_iter().flatten() {
        if let Err(e) = std::os::unix::fs::chown(path, Some(0), Some(0)) {
            log(&format!("chown {}: {}", path.display(), e));
        }
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)) {
            log(&format!("chmod {}: {}", path.display(), e));
        }
    }
}

/// Accept terminal connections and relay each to the PTY agent's socket.
fn serve_pty(fd: RawFd) {
    log(&format!("terminal relay ready on vsock port {}", PTY_PORT));
    loop {
        let conn: RawFd = unsafe { libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if conn < 0 {
            continue;
        }
        let stream = unsafe { UnixStream::from_raw_fd(conn) };
        let Some(slot) = RelaySlot::acquire() else {
            log(&format!(
                "terminal relay refused: {} relays already active",
                MAX_PTY_RELAYS
            ));
            continue;
        };
        let spawned = std::thread::Builder::new()
            .name("pty-relay".to_string())
            .spawn(move || {
                let _slot = slot;
                relay_to_pty_agent(stream);
            });
        if let Err(e) = spawned {
            log(&format!("could not start a terminal relay thread: {}", e));
        }
    }
}

/// RAII claim on one of the [`MAX_PTY_RELAYS`] slots.
struct RelaySlot;

impl RelaySlot {
    fn acquire() -> Option<Self> {
        let mut current = ACTIVE_PTY_RELAYS.load(Ordering::Relaxed);
        loop {
            if current >= MAX_PTY_RELAYS {
                return None;
            }
            match ACTIVE_PTY_RELAYS.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(RelaySlot),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for RelaySlot {
    fn drop(&mut self) {
        ACTIVE_PTY_RELAYS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn relay_to_pty_agent(host: UnixStream) {
    // The agent may still be binding its socket when the very first terminal
    // is opened right after boot, so allow a short window before giving up.
    let mut attempts = 0;
    let agent = loop {
        match UnixStream::connect(PTY_AGENT_SOCKET) {
            Ok(stream) => break stream,
            Err(_) if attempts < 20 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                // Closing the vsock connection is the only signal the host
                // gets; the cause belongs on the serial console.
                log(&format!(
                    "terminal relay: cannot connect to {}: {}",
                    PTY_AGENT_SOCKET, e
                ));
                return;
            }
        }
    };
    if let Err(e) = relay(host, agent) {
        log(&format!("terminal relay ended with an error: {}", e));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_slots_are_bounded_and_returned_on_drop() {
        let held: Vec<RelaySlot> = (0..MAX_PTY_RELAYS)
            .map(|_| RelaySlot::acquire().expect("slot below the cap"))
            .collect();
        assert!(
            RelaySlot::acquire().is_none(),
            "the {}th relay must be refused",
            MAX_PTY_RELAYS + 1
        );
        drop(held);
        assert!(
            RelaySlot::acquire().is_some(),
            "slots must be returned when relays end"
        );
    }
}
