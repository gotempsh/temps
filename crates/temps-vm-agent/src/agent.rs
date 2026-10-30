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

use serde_json::{json, Value};
use temps_vm_agent::logging::{self, Level};
use temps_vm_agent::mounts::{MountArgs, MountError};
use temps_vm_agent::restart::RestartBackoff;
use temps_vm_agent::stats;
use temps_vm_agent::supervisor::{ProcessSpec, Supervision, DEFAULT_PATH};
use temps_vm_agent::workload_log::{self, read_tail, LogLimits, LogPipeline, RotatingLog};
use temps_vm_agent::{
    read_frame, relay, write_frame, Request, Response, SpawnRestartPolicy, AGENT_PORT,
    DEFAULT_SPAWN_LOG, MAX_PTY_RELAYS, MAX_READ_LOG_LINES, MAX_SPAWNED_PROCESSES, PTY_AGENT_BIN,
    PTY_AGENT_SOCKET, PTY_PORT, WORK_DIR,
};

/// Pause between PTY agent restarts, so a binary that dies at once cannot
/// turn PID 1 into a hot loop.
const PTY_RESTART_DELAY: Duration = Duration::from_secs(1);

/// Relays currently being served; bounded by [`MAX_PTY_RELAYS`].
static ACTIVE_PTY_RELAYS: AtomicUsize = AtomicUsize::new(0);

pub fn run() {
    let is_init = std::process::id() == 1;
    if is_init {
        setup_system();
    }
    info(
        "starting",
        &[
            ("pid", json!(std::process::id())),
            ("vsock_port", json!(AGENT_PORT)),
            ("pty_port", json!(PTY_PORT)),
        ],
    );
    start_terminal_support();
    serve();
}

/// One pseudo-filesystem mounted at boot: `(source, target, fstype, data)`.
type MountSpec = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// Pseudo-filesystems mounted when running as PID 1, in order.
///
/// `devpts` must follow `devtmpfs`: the guest boots straight into this
/// binary (no distro init), and without a devpts instance `openpty(3)` — and
/// therefore every terminal tab — fails. `/dev/ptmx` comes from devtmpfs and
/// resolves its slaves through the sibling `/dev/pts`; `ptmxmode=0666` lets
/// non-root users open it.
const BOOT_MOUNTS: &[MountSpec] = &[
    ("proc", "/proc", "proc", None),
    ("sysfs", "/sys", "sysfs", None),
    ("devtmpfs", "/dev", "devtmpfs", None),
    (
        "devpts",
        "/dev/pts",
        "devpts",
        Some("mode=0620,ptmxmode=0666"),
    ),
    ("tmpfs", "/tmp", "tmpfs", None),
    ("tmpfs", "/run", "tmpfs", None),
];

/// Minimal boot-time setup when running as PID 1.
fn setup_system() {
    for dir in ["/proc", "/sys", "/dev", "/tmp", "/run", WORK_DIR] {
        let _ = std::fs::create_dir_all(dir);
    }
    for (src, target, fstype, data) in BOOT_MOUNTS {
        // `/dev/pts` only exists once devtmpfs is mounted over `/dev`.
        let _ = std::fs::create_dir_all(target);
        if let Err(e) = mount(src, target, fstype, *data) {
            warn(
                "mount_failed",
                &[
                    ("target", json!(target)),
                    ("fstype", json!(fstype)),
                    ("error", json!(e.to_string())),
                ],
            );
        }
    }
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

/// Mount one pseudo-filesystem. `EBUSY` (already mounted, e.g. a kernel that
/// automounted devtmpfs) is success.
fn mount(src: &str, target: &str, fstype: &str, data: Option<&str>) -> Result<(), MountError> {
    let args = MountArgs::new(src, target, fstype, data)?;
    let rc = unsafe {
        libc::mount(
            args.source.as_ptr(),
            args.target.as_ptr(),
            args.fstype.as_ptr(),
            0,
            args.data
                .as_ref()
                .map_or(std::ptr::null(), |d| d.as_ptr() as *const libc::c_void),
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EBUSY) {
        return Ok(());
    }
    Err(MountError::Syscall {
        target: target.to_string(),
        fstype: fstype.to_string(),
        source: err,
    })
}

/// Structured JSONL record on the serial console, our only log sink.
fn log_event(level: Level, event: &str, fields: &[(&str, Value)]) {
    logging::emit(level, event, fields);
}

fn info(event: &str, fields: &[(&str, Value)]) {
    log_event(Level::Info, event, fields);
}

fn warn(event: &str, fields: &[(&str, Value)]) {
    log_event(Level::Warn, event, fields);
}

fn error(event: &str, fields: &[(&str, Value)]) {
    log_event(Level::Error, event, fields);
}

// ── Vsock server ────────────────────────────────────────────────────────

/// Bind and listen on a vsock `port`, accepting connections from any CID.
fn listen_vsock(port: u32) -> Result<RawFd, String> {
    // CLOEXEC: this agent starts every sandbox command and restarts the PTY
    // agent. Without it each child would inherit the listener, and could
    // hold or reuse it independently of the agent that owns it.
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
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

/// Accept one connection with `SOCK_CLOEXEC` set atomically, so the stream is
/// never inherited by a command started on another thread (a plain `accept`
/// followed by `fcntl` would leave a window).
///
/// `None` means no connection was produced. Anything other than `EINTR`
/// pauses briefly first: an `EMFILE`/`ENOMEM` failure would otherwise spin
/// this PID 1 thread at full speed.
fn accept_cloexec(listener: RawFd) -> Option<RawFd> {
    let conn = unsafe {
        libc::accept4(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    };
    if conn >= 0 {
        return Some(conn);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EINTR) {
        warn(
            "accept_failed",
            &[
                ("listener_fd", json!(listener)),
                ("error", json!(err.to_string())),
            ],
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn serve() {
    let fd = match listen_vsock(AGENT_PORT) {
        Ok(fd) => fd,
        Err(e) => {
            error(
                "listen_failed",
                &[("port", json!(AGENT_PORT)), ("error", json!(e))],
            );
            halt_forever();
        }
    };
    info("ready", &[("port", json!(AGENT_PORT))]);

    loop {
        let Some(conn) = accept_cloexec(fd) else {
            continue;
        };
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
        warn(
            "terminals_disabled",
            &[
                ("path", json!(PTY_AGENT_BIN)),
                ("reason", json!("PTY agent binary not present")),
            ],
        );
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("pty-supervisor".to_string())
        .spawn(supervise_pty_agent)
    {
        error(
            "pty_supervisor_spawn_failed",
            &[("error", json!(e.to_string()))],
        );
        return;
    }
    match listen_vsock(PTY_PORT) {
        Ok(fd) => {
            let spawned = std::thread::Builder::new()
                .name("pty-listener".to_string())
                .spawn(move || serve_pty(fd));
            if let Err(e) = spawned {
                error(
                    "pty_listener_spawn_failed",
                    &[("port", json!(PTY_PORT)), ("error", json!(e.to_string()))],
                );
            }
        }
        Err(e) => error(
            "pty_listen_failed",
            &[("port", json!(PTY_PORT)), ("error", json!(e))],
        ),
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
                let pid = child.id();
                info("pty_agent_started", &[("pid", json!(pid))]);
                match child.wait() {
                    Ok(status) => warn(
                        "pty_agent_exited",
                        &[
                            ("pid", json!(pid)),
                            ("status", json!(status.to_string())),
                            ("uptime_ms", json!(started.elapsed().as_millis() as u64)),
                            ("restart_in_ms", json!(PTY_RESTART_DELAY.as_millis() as u64)),
                        ],
                    ),
                    Err(e) => error(
                        "pty_agent_wait_failed",
                        &[("pid", json!(pid)), ("error", json!(e.to_string()))],
                    ),
                }
            }
            Err(e) => error(
                "pty_agent_spawn_failed",
                &[
                    ("path", json!(PTY_AGENT_BIN)),
                    ("error", json!(e.to_string())),
                ],
            ),
        }
        std::thread::sleep(PTY_RESTART_DELAY);
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
            warn(
                "pty_agent_chown_failed",
                &[
                    ("path", json!(path.display().to_string())),
                    ("error", json!(e.to_string())),
                ],
            );
        }
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)) {
            warn(
                "pty_agent_chmod_failed",
                &[
                    ("path", json!(path.display().to_string())),
                    ("error", json!(e.to_string())),
                ],
            );
        }
    }
}

/// Accept terminal connections and relay each to the PTY agent's socket.
fn serve_pty(fd: RawFd) {
    info("pty_relay_ready", &[("port", json!(PTY_PORT))]);
    loop {
        let Some(conn) = accept_cloexec(fd) else {
            continue;
        };
        let stream = unsafe { UnixStream::from_raw_fd(conn) };
        let Some(slot) = RelaySlot::acquire() else {
            warn(
                "pty_relay_refused",
                &[
                    ("port", json!(PTY_PORT)),
                    ("max_relays", json!(MAX_PTY_RELAYS)),
                ],
            );
            continue;
        };
        let spawned = std::thread::Builder::new()
            .name("pty-relay".to_string())
            .spawn(move || {
                let _slot = slot;
                relay_to_pty_agent(stream);
            });
        if let Err(e) = spawned {
            error(
                "pty_relay_spawn_failed",
                &[("port", json!(PTY_PORT)), ("error", json!(e.to_string()))],
            );
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
                warn(
                    "pty_relay_connect_failed",
                    &[
                        ("socket", json!(PTY_AGENT_SOCKET)),
                        ("attempts", json!(attempts + 1)),
                        ("error", json!(e.to_string())),
                    ],
                );
                return;
            }
        }
    };
    if let Err(e) = relay(host, agent) {
        warn(
            "pty_relay_ended_with_error",
            &[
                ("socket", json!(PTY_AGENT_SOCKET)),
                ("error", json!(e.to_string())),
            ],
        );
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
    if shutdown {
        // A deliberate stop: nothing is restarted on its way down.
        stop_all_supervised();
    }
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
        Request::Spawn {
            cmd,
            env,
            cwd,
            uid,
            gid,
            log_path,
            restart,
            log_max_bytes,
            log_max_files,
        } => spawn(SpawnArgs {
            cmd,
            env,
            cwd,
            uid,
            gid,
            log_path,
            restart,
            log_limits: LogLimits::from_request(log_max_bytes, log_max_files),
        }),
        Request::ProcessStatus { pid } => process_status(pid),
        Request::ReadLog { path, tail_lines } => read_log(path, tail_lines),
        Request::ReadLogSince {
            path,
            cursor,
            max_bytes,
            include_partial,
        } => read_log_since(path, cursor, max_bytes, include_partial),
        Request::ReadStats { pid } => read_stats(pid),
    }
}

// ── Spawn (a workload's main process) ───────────────────────────────────

/// Processes started by [`spawn`], each under a [`Supervision`] that reaps
/// it, restarts it per its policy and bounds its log. Bounded by
/// [`MAX_SPAWNED_PROCESSES`].
static SPAWNED: std::sync::Mutex<Vec<Supervision>> = std::sync::Mutex::new(Vec::new());

/// The process table; plain data, usable whatever a panicking holder did.
fn spawned_table() -> std::sync::MutexGuard<'static, Vec<Supervision>> {
    SPAWNED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A deliberate shutdown: no supervised process may be restarted between
/// the acknowledgement and the reboot.
fn stop_all_supervised() {
    for supervision in spawned_table().iter() {
        supervision.mark_stopped();
    }
}

struct SpawnArgs {
    cmd: Vec<String>,
    env: HashMap<String, String>,
    cwd: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    log_path: Option<String>,
    restart: SpawnRestartPolicy,
    log_limits: LogLimits,
}

fn spawn(args: SpawnArgs) -> Response {
    let Some(program) = args.cmd.first().cloned() else {
        return Response::Err {
            message: "spawn: empty command".to_string(),
        };
    };
    let mut spawned = spawned_table();
    // Forget supervisions that ended: nothing of theirs runs any more.
    spawned.retain(|supervision| !supervision.is_finished());
    if spawned.len() >= MAX_SPAWNED_PROCESSES {
        return Response::Err {
            message: format!(
                "spawn {program}: {} processes already run (limit {MAX_SPAWNED_PROCESSES})",
                spawned.len(),
            ),
        };
    }
    let log_path = args
        .log_path
        .unwrap_or_else(|| DEFAULT_SPAWN_LOG.to_string());
    let log = match RotatingLog::open(&log_path, args.log_limits) {
        Ok(log) => log,
        Err(e) => {
            return Response::Err {
                message: format!("spawn {program}: open log {log_path}: {e}"),
            }
        }
    };
    let error_path = log_path.clone();
    let pipeline = match LogPipeline::start(log, move |e: &std::io::Error| {
        warn(
            "workload_log_write_failed",
            &[("path", json!(error_path)), ("error", json!(e.to_string()))],
        );
    }) {
        Ok(pipeline) => pipeline,
        Err(e) => {
            return Response::Err {
                message: format!("spawn {program}: start the log writer for {log_path}: {e}"),
            }
        }
    };
    let spec = ProcessSpec {
        cmd: args.cmd,
        env: args.env,
        cwd: args.cwd.unwrap_or_else(|| "/".to_string()),
        uid: args.uid,
        gid: args.gid,
    };
    match Supervision::start(spec, args.restart, RestartBackoff::default(), pipeline) {
        Ok(supervision) => {
            let pid = supervision.id();
            spawned.push(supervision);
            info(
                "spawned",
                &[
                    ("pid", json!(pid)),
                    ("cmd", json!(program)),
                    ("restart", json!(args.restart)),
                    ("log_path", json!(log_path)),
                    ("log_max_bytes", json!(args.log_limits.max_bytes)),
                    ("log_max_files", json!(args.log_limits.max_files)),
                ],
            );
            Response::Spawned { pid }
        }
        Err(e) => Response::Err {
            message: e.to_string(),
        },
    }
}

fn process_status(pid: u32) -> Response {
    let spawned = spawned_table();
    // Newest first: a pid number can be reused by a later spawn.
    let Some(supervision) = spawned.iter().rev().find(|s| s.matches(pid)) else {
        return Response::Err {
            message: format!("process status: pid {pid} was not started by spawn"),
        };
    };
    let status = supervision.status();
    Response::ProcessStatus {
        running: status.running(),
        exit_code: if status.running() {
            None
        } else {
            status.last_exit
        },
        restart_count: status.restart_count,
        restarting: status.restarting,
    }
}

fn read_log_since(
    path: Option<String>,
    cursor: Option<temps_vm_agent::LogCursor>,
    max_bytes: Option<u32>,
    include_partial: bool,
) -> Response {
    let path = path.unwrap_or_else(|| DEFAULT_SPAWN_LOG.to_string());
    let limit = max_bytes
        .unwrap_or(temps_vm_agent::DEFAULT_LOG_CHUNK_BYTES)
        .clamp(1, temps_vm_agent::MAX_LOG_CHUNK_BYTES) as usize;
    match workload_log::read_since(Path::new(&path), cursor, limit, include_partial) {
        Ok(read) => Response::LogChunk {
            data: String::from_utf8_lossy(&read.data).into_owned(),
            cursor: read.cursor,
            skipped: read.skipped,
            more: read.more,
        },
        Err(e) => Response::Err {
            message: format!("read log {path}: {e}"),
        },
    }
}

/// Sample the guest. With a `pid`, CPU and memory are those of the tree of
/// the process spawn started as that pid, rooted at its *current* process (the
/// supervised pid changes when it restarts); a process that is down between
/// restarts reports an empty tree rather than the whole VM.
fn read_stats(pid: Option<u32>) -> Response {
    let root = match pid {
        None => None,
        Some(pid) => {
            let spawned = spawned_table();
            let Some(supervision) = spawned.iter().rev().find(|s| s.matches(pid)) else {
                return Response::Err {
                    message: format!("read stats: pid {pid} was not started by spawn"),
                };
            };
            // No live pid: a root no process has, so the tree is empty.
            Some(supervision.status().current_pid.unwrap_or(0))
        }
    };
    Response::Stats(stats::collect(
        Path::new("/proc"),
        root,
        stats::ProcUnits::from_system(),
    ))
}

fn read_log(path: Option<String>, tail_lines: u32) -> Response {
    let path = path.unwrap_or_else(|| DEFAULT_SPAWN_LOG.to_string());
    let lines = tail_lines.min(MAX_READ_LOG_LINES) as usize;
    match read_tail(Path::new(&path), lines) {
        Ok(bytes) => Response::Log {
            data: String::from_utf8_lossy(&bytes).into_owned(),
        },
        Err(e) => Response::Err {
            message: format!("read log {path}: {e}"),
        },
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
    fn boot_mounts_provide_devpts_after_devtmpfs() {
        // Without devpts every `openpty` — and so every terminal tab — fails
        // in a guest that boots straight into this binary.
        let position = |fstype: &str| {
            BOOT_MOUNTS
                .iter()
                .position(|(_, _, t, _)| *t == fstype)
                .unwrap_or_else(|| panic!("{fstype} is not mounted at boot"))
        };
        assert!(position("devpts") > position("devtmpfs"));
        let (_, target, _, data) = BOOT_MOUNTS[position("devpts")];
        assert_eq!(target, "/dev/pts");
        assert!(data.is_some_and(|d| d.contains("ptmxmode=")), "{data:?}");
    }

    fn is_cloexec(fd: RawFd) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "fcntl(F_GETFD) failed on fd {fd}");
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn accepted_connections_are_close_on_exec() {
        // A plain `accept` would hand every sandbox command started by this
        // PID 1 a copy of the stream; accept4(SOCK_CLOEXEC) must not.
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("temps-vm-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("accept.sock");
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let _client = UnixStream::connect(&path).expect("connect test socket");

        let conn = accept_cloexec(listener.as_raw_fd()).expect("a pending connection is accepted");
        let cloexec = is_cloexec(conn);
        unsafe { libc::close(conn) };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(cloexec, "accepted fd {conn} must carry FD_CLOEXEC");
    }

    #[test]
    fn mount_rejects_an_argument_with_a_nul_byte_without_panicking() {
        let err = mount("tmpfs", "/run\0x", "tmpfs", None).unwrap_err();
        assert!(matches!(err, MountError::InvalidArgument { .. }), "{err}");
    }

    fn spawn_args(cmd: &[&str], log: &Path, restart: SpawnRestartPolicy) -> SpawnArgs {
        SpawnArgs {
            cmd: cmd.iter().map(|part| part.to_string()).collect(),
            env: HashMap::new(),
            cwd: None,
            uid: None,
            gid: None,
            log_path: Some(log.display().to_string()),
            restart,
            log_limits: LogLimits::default(),
        }
    }

    fn wait_for_exit(pid: u32) -> (Option<i32>, u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match process_status(pid) {
                Response::ProcessStatus {
                    running: false,
                    restarting: false,
                    exit_code,
                    restart_count,
                } => return (exit_code, restart_count),
                _ if Instant::now() > deadline => panic!("process {pid} did not exit"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    #[test]
    fn spawned_process_status_reports_running_then_its_exit_code() {
        let dir = std::env::temp_dir().join(format!("temps-vm-agent-spawn-{}", std::process::id()));
        let log = dir.join("out.log");
        let response = spawn(spawn_args(
            &["/bin/sh", "-c", "echo started; sleep 0.3; exit 7"],
            &log,
            SpawnRestartPolicy::Never,
        ));
        let Response::Spawned { pid } = response else {
            panic!("unexpected {response:?}");
        };
        assert!(matches!(
            process_status(pid),
            Response::ProcessStatus {
                running: true,
                exit_code: None,
                ..
            }
        ));
        assert_eq!(wait_for_exit(pid), (Some(7), 0));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match read_log(Some(log.display().to_string()), 10) {
                Response::Log { data } if data.contains("started") => break,
                _ if Instant::now() > deadline => panic!("the log never showed the output"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(process_status(999_999), Response::Err { .. }));
        assert!(matches!(
            spawn(spawn_args(&[], &log, SpawnRestartPolicy::Never)),
            Response::Err { .. }
        ));
    }

    #[test]
    fn a_spawned_process_is_restarted_per_its_policy_under_the_same_pid_handle() {
        let dir =
            std::env::temp_dir().join(format!("temps-vm-agent-restart-{}", std::process::id()));
        let marker = dir.join("ran-once");
        let _ = std::fs::create_dir_all(&dir);
        let script = format!(
            "if [ -e {m} ]; then exit 0; else touch {m}; exit 1; fi",
            m = marker.display()
        );
        let response = spawn(spawn_args(
            &["/bin/sh", "-c", &script],
            &dir.join("out.log"),
            SpawnRestartPolicy::OnFailure { max_retries: None },
        ));
        let Response::Spawned { pid } = response else {
            panic!("unexpected {response:?}");
        };
        // The first run fails, the restart (after the 1s backoff) succeeds.
        assert_eq!(wait_for_exit(pid), (Some(0), 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

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
