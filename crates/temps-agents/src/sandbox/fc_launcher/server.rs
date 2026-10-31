// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The privileged half of the launcher: runs as root, accepts requests from
//! the server user only, and acts on nothing it has not first verified by
//! file descriptor (the server user controls the VM directories, so a path
//! checked and then used would be a race).

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, error, info, warn};

use super::plan::{
    self, identity_for_slot, jailer_args, parse_vm_config, render_jail_config, validate_vm_name,
    SlotTable, VmSpec, CONFIG_FILE, KERNEL_FILE, ROOTFS_FILE, VSOCK_FILE,
};
use super::{
    jail_id, jail_root, read_frame, write_frame, FrameError, LauncherConfig, LauncherRequest,
    LauncherResponse,
};

/// The server's `vm.json` is a few hundred bytes; anything big is not one.
const MAX_VM_CONFIG_BYTES: u64 = 64 * 1024;
const SOCKET_WAIT: Duration = Duration::from_secs(15);
const KILL_WAIT: Duration = Duration::from_secs(5);
const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const STATE_FILE: &str = "launcher-state.json";

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("VM '{vm}': {step}: {reason}")]
    Step {
        vm: String,
        step: &'static str,
        reason: String,
    },
    #[error("{0}")]
    Plan(#[from] plan::PlanError),
    #[error("launcher setup at '{path}': {reason}")]
    Setup { path: PathBuf, reason: String },
}

fn step(vm: &str, step: &'static str, reason: impl std::fmt::Display) -> ServerError {
    ServerError::Step {
        vm: vm.to_string(),
        step,
        reason: reason.to_string(),
    }
}

/// A VMM the launcher started, remembered across launcher restarts so the
/// VM can still be killed (`KillMode=process` keeps VMMs alive when the
/// launcher restarts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RunningVm {
    pid: u32,
    uid: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    slots: SlotTable,
    running: BTreeMap<String, RunningVm>,
    /// TAP devices handed to a VM's identity, to give back on removal.
    taps: BTreeMap<String, String>,
}

pub struct Launcher {
    config: LauncherConfig,
    /// Serializes operations: launches are rare and short, and one lock
    /// removes every interleaving of kill/remove with a launch in flight.
    state: tokio::sync::Mutex<PersistedState>,
}

/// Whether a connecting peer may use the launcher: only the server user.
pub fn peer_allowed(config: &LauncherConfig, peer_uid: u32) -> bool {
    peer_uid == config.server_uid
}

/// The real uid in a `/proc/<pid>/status` body, and whether the process is a
/// zombie.
fn parse_proc_status(status: &str) -> (Option<u32>, bool) {
    let mut uid = None;
    let mut zombie = false;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            uid = rest.split_whitespace().next().and_then(|u| u.parse().ok());
        } else if let Some(rest) = line.strip_prefix("State:") {
            zombie = rest.trim_start().starts_with('Z');
        }
    }
    (uid, zombie)
}

/// Whether `pid` is still the live VMM the launcher started: a recycled pid
/// belongs to some other process and must never be signalled.
fn is_our_vmm(proc_root: &Path, pid: u32, uid: u32) -> bool {
    let dir = proc_root.join(pid.to_string());
    let comm_ok = std::fs::read_to_string(dir.join("comm"))
        .map(|c| c.trim().starts_with("firecracker"))
        .unwrap_or(false);
    if !comm_ok {
        return false;
    }
    match std::fs::read_to_string(dir.join("status")) {
        Ok(status) => {
            let (found, zombie) = parse_proc_status(&status);
            found == Some(uid) && !zombie
        }
        Err(_) => false,
    }
}

// ── descriptor-based file access ────────────────────────────────────────

/// Open a directory without following a symlink in its last component.
fn open_dir(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn c_name(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL"))
}

/// `openat(dir, name)` refusing symlinks, so nothing the server user plants
/// in its own VM directory redirects a root-owned write.
fn openat_nofollow(dir: &File, name: &str, flags: i32, mode: u32) -> std::io::Result<File> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// A regular file owned by `owner`, not hard-linked elsewhere.
fn check_owned_regular(file: &File, owner: u32, what: &str) -> Result<std::fs::Metadata, String> {
    let meta = file.metadata().map_err(|e| format!("{what}: stat: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{what} is not a regular file"));
    }
    if meta.uid() != owner {
        return Err(format!(
            "{what} is owned by uid {}, not the server user (uid {owner})",
            meta.uid()
        ));
    }
    Ok(meta)
}

/// The VM's disk: a regular file the server user owns, or, once the VM has
/// run, its own jail identity, which the launcher handed it to and which
/// keeps it across restarts. No one else's file is ever linked into a jail
/// or handed to a VM.
fn check_disk(file: &File, server_uid: u32, vm_uid: u32) -> Result<std::fs::Metadata, String> {
    let meta = file
        .metadata()
        .map_err(|e| format!("{ROOTFS_FILE}: stat: {e}"))?;
    if meta.uid() == vm_uid {
        if !meta.is_file() {
            return Err(format!("{ROOTFS_FILE} is not a regular file"));
        }
        return Ok(meta);
    }
    check_owned_regular(file, server_uid, ROOTFS_FILE)
        .map_err(|e| format!("{e} or the VM's own jail identity (uid {vm_uid})"))
}

/// Read a small file the server wrote.
fn read_server_file(dir: &File, name: &str, owner: u32, max: u64) -> Result<Vec<u8>, String> {
    let mut file =
        openat_nofollow(dir, name, libc::O_RDONLY, 0).map_err(|e| format!("open {name}: {e}"))?;
    let meta = check_owned_regular(&file, owner, name)?;
    if meta.len() > max {
        return Err(format!("{name} is {} bytes (limit {max})", meta.len()));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    Read::take(&mut file, max)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {name}: {e}"))?;
    Ok(bytes)
}

/// Write a file into the server's VM directory as root without letting the
/// server redirect it: no symlinks, a file the server owns (or one we
/// create), a single link.
fn write_server_file(
    dir: &File,
    name: &str,
    bytes: &[u8],
    server_uid: u32,
    server_gid: u32,
) -> Result<(), String> {
    let mut file = open_server_output(dir, name, server_uid, server_gid)?;
    file.write_all(bytes)
        .map_err(|e| format!("write {name}: {e}"))
}

/// Open (creating if needed) an output file in the server's VM directory,
/// truncated, owned by the server user.
fn open_server_output(
    dir: &File,
    name: &str,
    server_uid: u32,
    server_gid: u32,
) -> Result<File, String> {
    let file = openat_nofollow(dir, name, libc::O_WRONLY | libc::O_CREAT, 0o640)
        .map_err(|e| format!("open {name}: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("{name}: stat: {e}"))?;
    if !meta.is_file() || meta.nlink() != 1 || (meta.uid() != server_uid && meta.uid() != 0) {
        return Err(format!(
            "{name} is not a plain single-link file owned by the server user"
        ));
    }
    std::os::unix::fs::fchown(&file, Some(server_uid), Some(server_gid))
        .map_err(|e| format!("chown {name}: {e}"))?;
    file.set_len(0)
        .map_err(|e| format!("truncate {name}: {e}"))?;
    Ok(file)
}

/// Hard-link the file behind `file` to `dst`, going through the descriptor
/// so the link is to the inode that was checked.
#[cfg(target_os = "linux")]
fn link_open_file(file: &File, dst: &Path) -> std::io::Result<()> {
    let src = c_name(&format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    let dst = c_name(&dst.to_string_lossy())?;
    let rc = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn link_open_file(_file: &File, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the jailer launcher needs Linux",
    ))
}

/// Make the VMM's `v.sock` reachable by the server user. Goes through an
/// `O_PATH` descriptor and confirms it is a socket first, so a symlink the
/// VMM swapped in cannot turn the chown into one on another file.
#[cfg(target_os = "linux")]
fn share_socket(path: &Path, server_uid: u32, server_gid: u32) -> Result<bool, String> {
    let c = c_name(&path.to_string_lossy()).map_err(|e| e.to_string())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return if e.kind() == std::io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(format!("open {}: {e}", path.display()))
        };
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(owned.as_raw_fd(), &mut stat) } != 0 {
        return Err(format!(
            "stat {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(format!("{} is not a socket", path.display()));
    }
    let empty = c_name("").map_err(|e| e.to_string())?;
    let rc = unsafe {
        libc::fchownat(
            owned.as_raw_fd(),
            empty.as_ptr(),
            server_uid,
            server_gid,
            libc::AT_EMPTY_PATH,
        )
    };
    if rc != 0 {
        return Err(format!(
            "chown {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let proc_path =
        c_name(&format!("/proc/self/fd/{}", owned.as_raw_fd())).map_err(|e| e.to_string())?;
    if unsafe { libc::chmod(proc_path.as_ptr(), 0o660) } != 0 {
        return Err(format!(
            "chmod {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(true)
}

/// Let the server user reach the vsock socket inside the jail. The jailer
/// leaves the chroot directory `0700` and owned by the VM's identity, which
/// shuts the server out of the socket it was just given; hand the directory's
/// group to the server and allow search only (`0710`), so it can open entries
/// it knows the name of but cannot list or change the jail. The VM keeps full
/// access as the owner. Opened without following links: the jail's parent is
/// root-owned, so the VM cannot swap the directory for something else.
#[cfg(target_os = "linux")]
fn let_server_search_jail(root: &Path, vm_uid: u32, server_gid: u32) -> Result<(), String> {
    let c = c_name(&root.to_string_lossy()).map_err(|e| e.to_string())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "open {}: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let dir = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::fchown(dir.as_raw_fd(), vm_uid, server_gid) } != 0 {
        return Err(format!(
            "chown {}: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchmod(dir.as_raw_fd(), 0o710) } != 0 {
        return Err(format!(
            "chmod {}: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn let_server_search_jail(_root: &Path, _vm_uid: u32, _server_gid: u32) -> Result<(), String> {
    Err("the jailer launcher needs Linux".to_string())
}

#[cfg(not(target_os = "linux"))]
fn share_socket(_path: &Path, _uid: u32, _gid: u32) -> Result<bool, String> {
    Err("the jailer launcher needs Linux".to_string())
}

async fn host_command(program: &str, args: &[&str]) -> Result<(), String> {
    let run = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(std::process::Stdio::null())
        .output();
    match tokio::time::timeout(HOST_COMMAND_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => Err(format!(
            "`{program} {}` failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Ok(Err(e)) => Err(format!("`{program}`: {e}")),
        Err(_) => Err(format!(
            "`{program} {}` timed out after {}s",
            args.join(" "),
            HOST_COMMAND_TIMEOUT.as_secs()
        )),
    }
}

/// Refuse an executable the server user could have replaced. The launcher
/// runs as root, so a `jailer` (or `firecracker`) that the server user can
/// write is a way from a compromised server to root, which is the very thing
/// the launcher exists to prevent. The file and every directory above it must
/// belong to `trusted_uid` (or root) and not be writable by group or others;
/// a world-writable directory is accepted only with the sticky bit, which
/// stops others replacing entries they do not own (`/tmp`).
pub fn check_trusted_binary(path: &Path, trusted_uid: u32) -> Result<(), String> {
    let real = std::fs::canonicalize(path)
        .map_err(|e| format!("{}: cannot be resolved: {e}", path.display()))?;
    let meta = std::fs::metadata(&real).map_err(|e| format!("{}: stat: {e}", real.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", real.display()));
    }
    let owned_ok = |uid: u32| uid == trusted_uid || uid == 0;
    if !owned_ok(meta.uid()) || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} (owner uid {}, mode {:o}) could be replaced by a user other than root; \
             the launcher runs it as root. Install it root-owned and not group/world \
             writable (`sudo temps firecracker install-launcher` does)",
            real.display(),
            meta.uid(),
            meta.mode() & 0o7777
        ));
    }
    for dir in real.ancestors().skip(1) {
        let meta = std::fs::metadata(dir).map_err(|e| format!("{}: stat: {e}", dir.display()))?;
        let others_can_write = meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0;
        if !owned_ok(meta.uid()) || others_can_write {
            return Err(format!(
                "{}, which holds {}, (owner uid {}, mode {:o}) lets a user other than root \
                 replace the executable; install it under a root-owned directory",
                dir.display(),
                real.display(),
                meta.uid(),
                meta.mode() & 0o7777
            ));
        }
    }
    Ok(())
}

impl Launcher {
    pub fn new(config: LauncherConfig) -> Result<Arc<Self>, ServerError> {
        let state = Self::load_state(&config);
        Ok(Arc::new(Self {
            config,
            state: tokio::sync::Mutex::new(state),
        }))
    }

    fn state_path(&self) -> PathBuf {
        self.config.jail_base.join(STATE_FILE)
    }

    fn load_state(config: &LauncherConfig) -> PersistedState {
        let path = config.jail_base.join(STATE_FILE);
        let mut state: PersistedState = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // A VMM that did not survive the restart, or whose pid now belongs
        // to something else, is no longer ours to signal.
        state
            .running
            .retain(|_, vm| is_our_vmm(Path::new("/proc"), vm.pid, vm.uid));
        state
    }

    fn save_state(&self, state: &PersistedState) -> Result<(), ServerError> {
        let path = self.state_path();
        let tmp = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)
        };
        write().map_err(|e| ServerError::Setup {
            path,
            reason: format!("write launcher state: {e}"),
        })
    }

    /// Create the jail base if needed and refuse a symlinked or foreign-owned
    /// one; the launcher deletes and populates it as root.
    pub fn prepare_jail_base(&self) -> Result<(), ServerError> {
        let base = &self.config.jail_base;
        let setup = |reason: String| ServerError::Setup {
            path: base.clone(),
            reason,
        };
        std::fs::create_dir_all(base).map_err(|e| setup(format!("create: {e}")))?;
        let meta = std::fs::symlink_metadata(base).map_err(|e| setup(format!("stat: {e}")))?;
        if !meta.is_dir() {
            return Err(setup("is not a directory (a symlink?)".to_string()));
        }
        if meta.uid() != 0 {
            return Err(setup(format!(
                "is owned by uid {}; the jail directory must be owned by root",
                meta.uid()
            )));
        }
        std::fs::set_permissions(base, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| setup(format!("chmod: {e}")))?;
        for ancestor in base.ancestors().skip(1) {
            if let Ok(m) = std::fs::symlink_metadata(ancestor) {
                if m.uid() != 0 {
                    warn!(
                        path = %ancestor.display(),
                        owner_uid = m.uid(),
                        "jail directory ancestor is not owned by root: the server user \
                         could redirect it; place the jail under a root-owned directory \
                         on the same filesystem"
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    /// Serve until the process ends.
    pub async fn serve(self: Arc<Self>) -> Result<(), ServerError> {
        for binary in [&self.config.jailer_bin, &self.config.firecracker_bin] {
            check_trusted_binary(binary, 0).map_err(|reason| ServerError::Setup {
                path: binary.clone(),
                reason,
            })?;
        }
        self.prepare_jail_base()?;
        let socket = &self.config.socket;
        let setup = |reason: String| ServerError::Setup {
            path: socket.clone(),
            reason,
        };
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).map_err(|e| setup(format!("create dir: {e}")))?;
        }
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).map_err(|e| setup(format!("bind: {e}")))?;
        std::os::unix::fs::chown(
            socket,
            Some(self.config.server_uid),
            Some(self.config.server_gid),
        )
        .map_err(|e| setup(format!("chown: {e}")))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| setup(format!("chmod: {e}")))?;
        info!(socket = %socket.display(), "firecracker launcher listening");
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| setup(format!("accept: {e}")))?;
            let launcher = Arc::clone(&self);
            tokio::spawn(async move { launcher.connection(stream).await });
        }
    }

    async fn connection(self: Arc<Self>, mut stream: UnixStream) {
        let peer = match stream.peer_cred() {
            Ok(cred) => cred.uid(),
            Err(e) => {
                warn!("launcher: cannot read peer credentials: {e}");
                return;
            }
        };
        if !peer_allowed(&self.config, peer) {
            warn!(
                peer_uid = peer,
                "launcher: refused a peer that is not the server user"
            );
            return;
        }
        let request = match read_frame::<LauncherRequest>(&mut stream).await {
            Ok(request) => request,
            Err(FrameError::TooLarge(len)) => {
                warn!(len, "launcher: oversized request frame");
                return;
            }
            Err(FrameError::Io(e)) => {
                debug!("launcher: unreadable request: {e}");
                return;
            }
        };
        let response = self.handle(request).await;
        if let Err(e) = write_frame(&mut stream, &response).await {
            debug!("launcher: cannot send the answer: {e}");
        }
    }

    pub async fn handle(self: &Arc<Self>, request: LauncherRequest) -> LauncherResponse {
        let result = match &request {
            LauncherRequest::Ping => Ok(LauncherResponse::Pong),
            LauncherRequest::Launch { vm } => self
                .launch(vm)
                .await
                .map(|pid| LauncherResponse::Launched { pid }),
            LauncherRequest::Kill { vm } => self
                .kill(vm)
                .await
                .map(|was_running| LauncherResponse::Killed { was_running }),
            LauncherRequest::Remove { vm } => {
                self.remove(vm).await.map(|()| LauncherResponse::Removed)
            }
        };
        result.unwrap_or_else(|e| {
            error!(?request, "launcher request failed: {e}");
            LauncherResponse::Err {
                message: e.to_string(),
            }
        })
    }

    async fn launch(self: &Arc<Self>, vm: &str) -> Result<u32, ServerError> {
        validate_vm_name(vm)?;
        let mut state = self.state.lock().await;
        if let Some(running) = state.running.get(vm) {
            if is_our_vmm(Path::new("/proc"), running.pid, running.uid) {
                return Err(step(
                    vm,
                    "launch",
                    format!("already running as pid {}", running.pid),
                ));
            }
        }
        state.running.remove(vm);

        // Everything about the VM comes from the directory the server owns,
        // read through one verified descriptor.
        let vm_path = self.config.vms_dir().join(vm);
        let vm_dir = open_dir(&vm_path).map_err(|e| step(vm, "open VM directory", e))?;
        let meta = vm_dir
            .metadata()
            .map_err(|e| step(vm, "stat VM directory", e))?;
        if meta.uid() != self.config.server_uid {
            return Err(step(
                vm,
                "open VM directory",
                format!("owned by uid {}, not the server user", meta.uid()),
            ));
        }
        let raw = read_server_file(
            &vm_dir,
            "vm.json",
            self.config.server_uid,
            MAX_VM_CONFIG_BYTES,
        )
        .map_err(|e| step(vm, "read vm.json", e))?;
        let spec = parse_vm_config(vm, &raw)?;

        let slot = state
            .slots
            .slot_for(vm, self.config.uid_count)
            .ok_or_else(|| {
                step(
                    vm,
                    "allocate identity",
                    format!(
                        "all {} jail identities are in use; raise --uid-count",
                        self.config.uid_count
                    ),
                )
            })?;
        let (uid, gid) = identity_for_slot(&self.config, slot).ok_or_else(|| {
            step(
                vm,
                "allocate identity",
                format!("slot {slot} is out of range"),
            )
        })?;

        self.build_jail(vm, &vm_dir, &spec, uid, gid)?;
        let tap = spec.interfaces.first().map(|i| i.tap.clone());
        if let Some(tap) = &tap {
            self.own_tap(vm, tap, uid, gid).await?;
            state.taps.insert(vm.to_string(), tap.clone());
        }

        let console = open_server_output(
            &vm_dir,
            "console.log",
            self.config.server_uid,
            self.config.server_gid,
        )
        .map_err(|e| step(vm, "open console.log", e))?;
        let mut child = tokio::process::Command::new(&self.config.jailer_bin)
            .args(jailer_args(&self.config, &jail_id(vm), uid, gid))
            .current_dir("/")
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(
                console
                    .try_clone()
                    .map_err(|e| step(vm, "console.log", e))?,
            )
            .stderr(console)
            .spawn()
            .map_err(|e| {
                step(
                    vm,
                    "spawn jailer",
                    format!("{}: {e}", self.config.jailer_bin.display()),
                )
            })?;
        let pid = child
            .id()
            .ok_or_else(|| step(vm, "spawn jailer", "exited immediately"))?;

        let root = jail_root(&self.config.jail_base, vm);
        let socket = root.join(VSOCK_FILE);
        let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let tail = console_tail(&vm_dir);
                return Err(step(
                    vm,
                    "start VMM",
                    format!("the jailer exited ({status}) before the VMM was up; console: {tail}"),
                ));
            }
            match share_socket(&socket, self.config.server_uid, self.config.server_gid) {
                // The jailer has chrooted by the time the VMM made the socket,
                // so its `0700` on the jail is final and safe to relax now.
                Ok(true) => match let_server_search_jail(&root, uid, self.config.server_gid) {
                    Ok(()) => break,
                    Err(reason) => {
                        kill_pid(pid);
                        return Err(step(vm, "open jail to the server", reason));
                    }
                },
                Ok(false) => {}
                Err(reason) => {
                    kill_pid(pid);
                    return Err(step(vm, "share vsock socket", reason));
                }
            }
            if tokio::time::Instant::now() > deadline {
                kill_pid(pid);
                return Err(step(
                    vm,
                    "start VMM",
                    format!(
                        "no vsock socket at {} after {}s; console: {}",
                        socket.display(),
                        SOCKET_WAIT.as_secs(),
                        console_tail(&vm_dir)
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        write_server_file(
            &vm_dir,
            "fc.pid",
            pid.to_string().as_bytes(),
            self.config.server_uid,
            self.config.server_gid,
        )
        .map_err(|e| step(vm, "write fc.pid", e))?;
        state.running.insert(vm.to_string(), RunningVm { pid, uid });
        self.save_state(&state)?;

        // Reap the VMM when it ends so it never lingers as a zombie.
        let launcher = Arc::clone(self);
        let name = vm.to_string();
        tokio::spawn(async move {
            let _ = child.wait().await;
            let mut state = launcher.state.lock().await;
            if state.running.get(&name).is_some_and(|r| r.pid == pid) {
                state.running.remove(&name);
                let _ = launcher.save_state(&state);
            }
        });
        info!(vm, pid, uid, "microVM started under the jailer");
        Ok(pid)
    }

    /// Build the chroot: the VM's disk (hard-linked, never copied), the
    /// pinned kernel, and a config rewritten from the validated fields.
    fn build_jail(
        &self,
        vm: &str,
        vm_dir: &File,
        spec: &VmSpec,
        uid: u32,
        gid: u32,
    ) -> Result<(), ServerError> {
        let root = jail_root(&self.config.jail_base, vm);
        let id_dir = self.config.jail_base.join("firecracker").join(jail_id(vm));
        if id_dir.exists() {
            std::fs::remove_dir_all(&id_dir)
                .map_err(|e| step(vm, "clear stale jail", format!("{}: {e}", id_dir.display())))?;
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| step(vm, "create jail", format!("{}: {e}", root.display())))?;
        std::os::unix::fs::chown(&root, Some(uid), Some(gid))
            .map_err(|e| step(vm, "create jail", format!("chown {}: {e}", root.display())))?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| step(vm, "create jail", e))?;

        // The disk: link the inode we verified, then hand it to the VM's
        // identity with group access kept for the server user.
        let disk = openat_nofollow(vm_dir, ROOTFS_FILE, libc::O_RDONLY, 0)
            .map_err(|e| step(vm, "open disk", format!("{ROOTFS_FILE}: {e}")))?;
        check_disk(&disk, self.config.server_uid, uid).map_err(|e| step(vm, "open disk", e))?;
        let disk_dst = root.join(ROOTFS_FILE);
        link_open_file(&disk, &disk_dst).map_err(|e| {
            step(
                vm,
                "link disk into jail",
                format!(
                    "{e} (the jail at {} must be on the same filesystem as the VM directory)",
                    self.config.jail_base.display()
                ),
            )
        })?;
        std::os::unix::fs::fchown(&disk, Some(uid), Some(self.config.server_gid))
            .map_err(|e| step(vm, "own disk", e))?;
        disk.set_permissions(std::fs::Permissions::from_mode(0o660))
            .map_err(|e| step(vm, "own disk", e))?;

        // The kernel: only from the launcher's own kernel directory.
        let kernel_src = self.vet_kernel(vm, &spec.kernel_path)?;
        let kernel_dst = root.join(KERNEL_FILE);
        if std::fs::hard_link(&kernel_src, &kernel_dst).is_err() {
            std::fs::copy(&kernel_src, &kernel_dst)
                .map_err(|e| step(vm, "place kernel", format!("{}: {e}", kernel_src.display())))?;
        }

        let config_dst = root.join(CONFIG_FILE);
        let rendered = serde_json::to_vec_pretty(&render_jail_config(spec))
            .map_err(|e| step(vm, "render config", e))?;
        std::fs::write(&config_dst, rendered).map_err(|e| {
            step(
                vm,
                "write jail config",
                format!("{}: {e}", config_dst.display()),
            )
        })?;
        std::fs::set_permissions(&config_dst, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| step(vm, "write jail config", e))?;
        Ok(())
    }

    fn vet_kernel(&self, vm: &str, requested: &Path) -> Result<PathBuf, ServerError> {
        let real = std::fs::canonicalize(requested)
            .map_err(|e| step(vm, "vet kernel", format!("{}: {e}", requested.display())))?;
        let dir = std::fs::canonicalize(&self.config.kernel_dir).map_err(|e| {
            step(
                vm,
                "vet kernel",
                format!("kernel dir {}: {e}", self.config.kernel_dir.display()),
            )
        })?;
        let named_like_a_kernel = real
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("vmlinux"));
        if !plan::is_within(&dir, &real) || !named_like_a_kernel || !real.is_file() {
            return Err(step(
                vm,
                "vet kernel",
                format!(
                    "{} is not a vmlinux file in the launcher's kernel directory {}",
                    requested.display(),
                    dir.display()
                ),
            ));
        }
        Ok(real)
    }

    /// Give the pool TAP to the VM's identity (the VMM opens it after
    /// dropping privileges) and wire it back onto the bridge.
    async fn own_tap(&self, vm: &str, tap: &str, uid: u32, gid: u32) -> Result<(), ServerError> {
        self.set_tap_owner(tap, uid, gid)
            .await
            .map_err(|reason| step(vm, "assign TAP device", reason))
    }

    async fn set_tap_owner(&self, tap: &str, uid: u32, gid: u32) -> Result<(), String> {
        if !Path::new("/sys/class/net").join(tap).exists() {
            return Err(format!(
                "pool device '{tap}' does not exist (run `temps firecracker setup --network-only`)"
            ));
        }
        let (uid, gid) = (uid.to_string(), gid.to_string());
        host_command("ip", &["tuntap", "del", "dev", tap, "mode", "tap"]).await?;
        host_command(
            "ip",
            &[
                "tuntap", "add", "dev", tap, "mode", "tap", "user", &uid, "group", &gid,
            ],
        )
        .await?;
        host_command("ip", &["link", "set", tap, "master", &self.config.bridge]).await?;
        // Best effort, as in setup: the host and metadata drop rules are the
        // load-bearing controls.
        let _ = host_command("bridge", &["link", "set", "dev", tap, "isolated", "on"]).await;
        host_command("ip", &["link", "set", tap, "up"]).await
    }

    async fn kill(&self, vm: &str) -> Result<bool, ServerError> {
        validate_vm_name(vm)?;
        let mut state = self.state.lock().await;
        self.kill_locked(vm, &mut state).await
    }

    async fn kill_locked(&self, vm: &str, state: &mut PersistedState) -> Result<bool, ServerError> {
        let Some(running) = state.running.get(vm).cloned() else {
            return Ok(false);
        };
        let proc_root = Path::new("/proc");
        if !is_our_vmm(proc_root, running.pid, running.uid) {
            state.running.remove(vm);
            self.save_state(state)?;
            return Ok(false);
        }
        kill_pid(running.pid);
        let deadline = tokio::time::Instant::now() + KILL_WAIT;
        while is_our_vmm(proc_root, running.pid, running.uid) {
            if tokio::time::Instant::now() > deadline {
                return Err(step(
                    vm,
                    "kill VMM",
                    format!(
                        "pid {} did not exit within {}s",
                        running.pid,
                        KILL_WAIT.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        state.running.remove(vm);
        self.save_state(state)?;
        Ok(true)
    }

    async fn remove(&self, vm: &str) -> Result<(), ServerError> {
        validate_vm_name(vm)?;
        let mut state = self.state.lock().await;
        self.kill_locked(vm, &mut state).await?;
        let id_dir = self.config.jail_base.join("firecracker").join(jail_id(vm));
        if id_dir.exists() {
            std::fs::remove_dir_all(&id_dir)
                .map_err(|e| step(vm, "remove jail", format!("{}: {e}", id_dir.display())))?;
        }
        if let Some(tap) = state.taps.remove(vm) {
            // Back to the server user, whose pool it is.
            self.set_tap_owner(&tap, self.config.server_uid, self.config.server_gid)
                .await
                .map_err(|reason| step(vm, "return TAP device", reason))?;
        }
        state.slots.release(vm);
        self.save_state(&state)?;
        info!(vm, "microVM jail removed");
        Ok(())
    }
}

fn kill_pid(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

fn console_tail(vm_dir: &File) -> String {
    read_server_file(vm_dir, "console.log", 0, 1 << 20)
        .or_else(|_| {
            // Owner is the server user once chowned.
            let uid = vm_dir.metadata().map(|m| m.uid()).unwrap_or(0);
            read_server_file(vm_dir, "console.log", uid, 1 << 20)
        })
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn config() -> LauncherConfig {
        LauncherConfig {
            fc_root: PathBuf::from("/data/firecracker"),
            jail_base: PathBuf::from("/data/firecracker/jail"),
            firecracker_bin: PathBuf::from("/bin/firecracker"),
            jailer_bin: PathBuf::from("/bin/jailer"),
            kernel_dir: PathBuf::from("/data/firecracker/kernel"),
            server_uid: 1000,
            server_gid: 1000,
            uid_base: 52000,
            uid_count: 4,
            bridge: "temps-fc-br0".into(),
            socket: PathBuf::from("/run/temps-fc/launcher.sock"),
        }
    }

    fn me() -> u32 {
        unsafe { libc::geteuid() }
    }

    #[test]
    fn only_the_server_user_may_use_the_launcher() {
        let config = config();
        assert!(peer_allowed(&config, 1000));
        assert!(!peer_allowed(&config, 0), "not even root: it has no need");
        assert!(!peer_allowed(&config, 52001), "a jailed VMM is refused");
        assert!(!peer_allowed(&config, 1001));
    }

    #[test]
    fn a_vmm_is_recognised_by_name_owner_and_liveness() {
        let proc = tempfile::tempdir().unwrap();
        let make = |pid: u32, comm: &str, status: &str| {
            let dir = proc.path().join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(dir.join("status"), status).unwrap();
        };
        let live = "Name:\tfirecracker\nState:\tS (sleeping)\nUid:\t52001\t52001\t52001\t52001\n";
        make(10, "firecracker", live);
        assert!(is_our_vmm(proc.path(), 10, 52001));
        assert!(!is_our_vmm(proc.path(), 10, 52002), "another VM's uid");
        make(11, "bash", live);
        assert!(!is_our_vmm(proc.path(), 11, 52001), "a recycled pid");
        make(
            12,
            "firecracker",
            "State:\tZ (zombie)\nUid:\t52001\t52001\t52001\t52001\n",
        );
        assert!(!is_our_vmm(proc.path(), 12, 52001), "a zombie is gone");
        assert!(!is_our_vmm(proc.path(), 99, 52001), "no such process");
    }

    #[test]
    fn a_symlinked_file_in_the_vm_directory_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret");
        std::fs::write(&target, "root-only").unwrap();
        let vm = dir.path().join("vm");
        std::fs::create_dir(&vm).unwrap();
        symlink(&target, vm.join("vm.json")).unwrap();
        symlink(&target, vm.join("console.log")).unwrap();
        let handle = open_dir(&vm).unwrap();

        let read = read_server_file(&handle, "vm.json", me(), 1024);
        assert!(
            read.is_err(),
            "reading through a symlink must fail: {read:?}"
        );
        let write = write_server_file(&handle, "console.log", b"x", me(), 0);
        assert!(write.is_err(), "writing through a symlink must fail");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "root-only");
    }

    #[test]
    fn a_symlinked_vm_directory_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(open_dir(&link).is_err());
        assert!(open_dir(&real).is_ok());
    }

    #[test]
    fn files_of_the_wrong_owner_or_size_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("vm.json"), vec![b'x'; 2048]).unwrap();
        let handle = open_dir(dir.path()).unwrap();
        let too_big = read_server_file(&handle, "vm.json", me(), 1024).unwrap_err();
        assert!(too_big.contains("limit 1024"), "{too_big}");
        let wrong_owner = read_server_file(&handle, "vm.json", me() + 1, 4096).unwrap_err();
        assert!(wrong_owner.contains("not the server user"), "{wrong_owner}");
        assert_eq!(
            read_server_file(&handle, "vm.json", me(), 4096)
                .unwrap()
                .len(),
            2048
        );
    }

    #[test]
    fn a_disk_the_vm_already_ran_on_is_launched_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ROOTFS_FILE), b"ext4").unwrap();
        let handle = open_dir(dir.path()).unwrap();
        let disk = openat_nofollow(&handle, ROOTFS_FILE, libc::O_RDONLY, 0).unwrap();
        // Never run: the server user's.
        assert!(check_disk(&disk, me(), me() + 52000).is_ok());
        // Run before: handed to this VM's identity, which it keeps.
        assert!(check_disk(&disk, me() + 1, me()).is_ok());
        // Anyone else's file is refused, naming both owners it may have.
        let other = check_disk(&disk, me() + 1, me() + 2).unwrap_err();
        assert!(other.contains("not the server user"), "{other}");
        assert!(
            other.contains(&format!("jail identity (uid {})", me() + 2)),
            "{other}"
        );

        std::fs::create_dir(dir.path().join("dir")).unwrap();
        let not_a_file = openat_nofollow(&handle, "dir", libc::O_RDONLY, 0).unwrap();
        assert!(check_disk(&not_a_file, me() + 1, me()).is_err());
    }

    #[test]
    fn a_hard_linked_output_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::fs::hard_link(&victim, dir.path().join("console.log")).unwrap();
        let handle = open_dir(dir.path()).unwrap();
        let error = write_server_file(&handle, "console.log", b"x", me(), 0).unwrap_err();
        assert!(error.contains("single-link"), "{error}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn state_survives_a_restart_but_forgets_a_vmm_that_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.jail_base = dir.path().to_path_buf();
        let launcher = Launcher::new(config.clone()).unwrap();
        let mut state = PersistedState::default();
        state.slots.slot_for("vm1", 4);
        state.running.insert(
            "vm1".into(),
            RunningVm {
                pid: 4_000_000,
                uid: 52000,
            },
        );
        state.taps.insert("vm1".into(), "temps-fc-tap0".into());
        launcher.save_state(&state).unwrap();

        let reloaded = Launcher::load_state(&config);
        assert_eq!(reloaded.slots.len(), 1, "the identity is kept");
        assert_eq!(
            reloaded.taps.get("vm1").map(String::as_str),
            Some("temps-fc-tap0")
        );
        assert!(reloaded.running.is_empty(), "no such process any more");
    }

    #[tokio::test]
    async fn requests_with_unsafe_names_are_answered_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.jail_base = dir.path().to_path_buf();
        let launcher = Launcher::new(config).unwrap();
        for request in [
            LauncherRequest::Launch {
                vm: "../etc".into(),
            },
            LauncherRequest::Kill { vm: "a/b".into() },
            LauncherRequest::Remove { vm: "".into() },
        ] {
            match launcher.handle(request).await {
                LauncherResponse::Err { message } => {
                    assert!(message.contains("not allowed"), "{message}")
                }
                other => panic!("expected an error, got {other:?}"),
            }
        }
        assert_eq!(
            launcher.handle(LauncherRequest::Ping).await,
            LauncherResponse::Pong
        );
        assert_eq!(
            launcher
                .handle(LauncherRequest::Kill {
                    vm: "never-started".into()
                })
                .await,
            LauncherResponse::Killed { was_running: false }
        );
    }

    #[tokio::test]
    async fn a_launch_for_a_missing_vm_names_the_vm_and_the_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.fc_root = dir.path().to_path_buf();
        config.jail_base = dir.path().join("jail");
        let launcher = Launcher::new(config).unwrap();
        match launcher
            .handle(LauncherRequest::Launch { vm: "ghost".into() })
            .await
        {
            LauncherResponse::Err { message } => {
                assert!(message.contains("ghost"), "{message}");
                assert!(message.contains("open VM directory"), "{message}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_server_can_reach_into_a_jail_but_not_list_it() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // The jailer leaves the chroot 0700, which locks the server out of
        // the vsock socket inside it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };

        let_server_search_jail(&root, uid, gid).unwrap();
        let meta = std::fs::metadata(&root).unwrap();
        assert_eq!(
            meta.mode() & 0o7777,
            0o710,
            "search only for the server group"
        );
        assert_eq!((meta.uid(), meta.gid()), (uid, gid));

        // A link in the jail's place is never followed.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(let_server_search_jail(&link, uid, gid).is_err());
    }

    #[test]
    fn an_executable_the_server_user_could_replace_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("jailer");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Owned by "us" and tight: accepted when "us" is the trusted owner.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(check_trusted_binary(&bin, me()), Ok(()));

        // Owned by someone else than the trusted user (and not root).
        if me() != 0 {
            let error = check_trusted_binary(&bin, me() + 1).unwrap_err();
            assert!(error.contains("could be replaced"), "{error}");
        }

        // Group- or world-writable file.
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o775)).unwrap();
        let error = check_trusted_binary(&bin, me()).unwrap_err();
        assert!(error.contains("could be replaced"), "{error}");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A writable parent directory lets the file be swapped for another.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        let error = check_trusted_binary(&bin, me()).unwrap_err();
        assert!(error.contains("replace the executable"), "{error}");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        // A symlink is judged by what it points at.
        let link = dir.path().join("link");
        symlink(&bin, &link).unwrap();
        assert_eq!(check_trusted_binary(&link, me()), Ok(()));
        assert!(check_trusted_binary(&dir.path().join("missing"), me()).is_err());
    }
}
