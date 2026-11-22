// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The non-root user a sandbox's commands, harnesses and terminals run as.
//!
//! A Docker sandbox runs everything as `temps` (uid 1000), the user its
//! image creates. A Firecracker sandbox boots an arbitrary image, so the
//! host names the user on the kernel command line
//! ([`SandboxUser::boot_arg`]) and the guest agent, still root as PID 1,
//! prepares it at boot before it starts anything that runs as that user:
//!
//! * the `passwd` and `group` entries, added to this VM's own rootfs copy
//!   when the image lacks them. The converted rootfs is cached per image and
//!   shared with deployment replicas, which must keep the image's own
//!   identity, so the entry is written per VM rather than at conversion;
//! * the home directory;
//! * ownership of the home and the workspace. The host extracts the rootfs
//!   without privileges, so every file carries the host user's uid until
//!   the guest normalises it.
//!
//! A conflict the agent cannot resolve safely (the name taken by another
//! uid, the gid taken by another group name) is a [`SandboxUserError`]: the
//! guest reports it through [`crate::Request::SandboxUser`] and the host
//! fails the create with it, rather than running the sandbox as root or as
//! someone else.
//!
//! Workload (deployment) VMs never get the boot argument: they run as the
//! image's own `USER`.

use std::io::Read;
use std::os::unix::fs::{lchown, MetadataExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Kernel command-line key carrying the sandbox user. The kernel keeps
/// unknown dotted parameters out of init's environment, and PID 1 reads it
/// back from `/proc/cmdline`.
pub const SANDBOX_USER_BOOT_ARG: &str = "temps.sandbox_user";

/// Most bytes of `/etc/passwd` or `/etc/group` the agent edits. Real files
/// are a few KiB; a larger one is refused rather than read into memory.
pub const MAX_ACCOUNT_FILE_BYTES: u64 = 1024 * 1024;

/// Login shell given to a user the agent creates. Terminals start their own
/// shell, so this only matters to tools that read it from `passwd`.
const CREATED_USER_SHELL: &str = "/bin/sh";

/// The sandbox user: its numeric identity, name and home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: String,
}

/// Why a boot argument or an account file could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxUserError {
    #[error("boot argument {SANDBOX_USER_BOOT_ARG}='{value}' is not uid:gid:name:/home: {reason}")]
    BadBootArg { value: String, reason: &'static str },

    #[error(
        "the image already has a user '{name}' with uid {existing_uid}; the sandbox needs \
         '{name}' to be uid {wanted_uid}"
    )]
    NameTakenByOtherUid {
        name: String,
        existing_uid: u32,
        wanted_uid: u32,
    },

    #[error(
        "the image already has a group '{name}' with gid {existing_gid}; the sandbox needs \
         '{name}' to be gid {wanted_gid}"
    )]
    GroupNameTakenByOtherGid {
        name: String,
        existing_gid: u32,
        wanted_gid: u32,
    },

    #[error("{path} is larger than {MAX_ACCOUNT_FILE_BYTES} bytes; refusing to edit it")]
    AccountFileTooLarge { path: String },

    #[error("{path} is not a regular file; refusing to read it as an account file")]
    AccountFileNotRegular { path: String },

    #[error("{path} does not record a rootfs owner normalisation: '{contents}'")]
    BadNormalizationRecord { path: String, contents: String },

    #[error("{operation} {path}: {reason}")]
    Io {
        operation: &'static str,
        path: String,
        reason: String,
    },
}

impl SandboxUserError {
    fn io(operation: &'static str, path: &Path, error: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }
}

impl SandboxUser {
    /// The kernel command-line parameter naming this user.
    pub fn boot_arg(&self) -> String {
        format!(
            "{SANDBOX_USER_BOOT_ARG}={}:{}:{}:{}",
            self.uid, self.gid, self.name, self.home
        )
    }

    /// The user named on a kernel command line, if any.
    pub fn from_cmdline(cmdline: &str) -> Option<Result<Self, SandboxUserError>> {
        let prefix = format!("{SANDBOX_USER_BOOT_ARG}=");
        cmdline
            .split_ascii_whitespace()
            .filter_map(|token| token.strip_prefix(&prefix))
            .next_back()
            .map(Self::parse)
    }

    fn parse(value: &str) -> Result<Self, SandboxUserError> {
        let bad = |reason| SandboxUserError::BadBootArg {
            value: value.to_string(),
            reason,
        };
        let mut parts = value.splitn(4, ':');
        let (Some(uid), Some(gid), Some(name), Some(home)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("expected four ':'-separated fields"));
        };
        let uid = uid.parse::<u32>().map_err(|_| bad("uid is not a number"))?;
        let gid = gid.parse::<u32>().map_err(|_| bad("gid is not a number"))?;
        if uid == 0 {
            return Err(bad("uid 0 is root, not a sandbox user"));
        }
        if !is_account_name(name) {
            return Err(bad("name must be lowercase letters, digits, '-' or '_'"));
        }
        if !home.starts_with('/') || home.contains(':') || home.split('/').any(|c| c == "..") {
            return Err(bad("home must be an absolute path without '..' or ':'"));
        }
        Ok(Self {
            uid,
            gid,
            name: name.to_string(),
            home: home.to_string(),
        })
    }
}

/// What preparing the user changed, for the boot log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrepareReport {
    /// A `passwd` entry was added (the image had none for the uid).
    pub added_user: bool,
    /// A `group` entry was added (the image had none for the gid).
    pub added_group: bool,
    /// An existing account already had the uid under another name, which
    /// the sandbox then runs as.
    pub existing_name: Option<String>,
    /// Paths whose owner was changed to the user.
    pub chowned: u64,
}

/// Prepare `user` in the filesystem rooted at `root` (`/` in the guest):
/// its accounts, its home, and ownership of the home and of `work_dir`.
/// Idempotent, so a restarted VM runs it again at every boot.
pub fn prepare(
    root: &Path,
    user: &SandboxUser,
    work_dir: &str,
) -> Result<PrepareReport, SandboxUserError> {
    let mut report = PrepareReport::default();

    // Both files are checked before either is written, so a conflict in
    // one never leaves a half-made account on the VM's persistent disk.
    let group_path = root.join("etc/group");
    let group_update = ensure_group_entry(&read_account_file(&group_path)?, &user.name, user.gid)?;
    let passwd_path = root.join("etc/passwd");
    let passwd_update = ensure_passwd_entry(&read_account_file(&passwd_path)?, user)?;

    if let Some(updated) = group_update {
        write_account_file(&group_path, &updated)?;
        report.added_group = true;
    }
    match passwd_update {
        PasswdEntry::Present => {}
        PasswdEntry::PresentAs(name) => report.existing_name = Some(name),
        PasswdEntry::Added(updated) => {
            write_account_file(&passwd_path, &updated)?;
            report.added_user = true;
        }
    }

    let home = root.join(user.home.trim_start_matches('/'));
    std::fs::create_dir_all(&home).map_err(|e| SandboxUserError::io("create", &home, e))?;
    let work = root.join(work_dir.trim_start_matches('/'));
    std::fs::create_dir_all(&work).map_err(|e| SandboxUserError::io("create", &work, e))?;
    for tree in [&home, &work] {
        report.chowned += chown_tree(tree, user.uid, user.gid)?;
    }
    Ok(report)
}

/// The outcome of looking for the user's `passwd` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswdEntry {
    /// The image already has the user.
    Present,
    /// The image has the uid under another name (`node` at uid 1000).
    PresentAs(String),
    /// The entry was missing; the updated file contents.
    Added(String),
}

/// Find or add `user` in `passwd` (the file's contents).
pub fn ensure_passwd_entry(
    passwd: &str,
    user: &SandboxUser,
) -> Result<PasswdEntry, SandboxUserError> {
    let mut same_uid = None;
    for (name, uid) in passwd.lines().filter_map(account_name_and_id) {
        if name == user.name {
            return if uid == user.uid {
                Ok(PasswdEntry::Present)
            } else {
                Err(SandboxUserError::NameTakenByOtherUid {
                    name: user.name.clone(),
                    existing_uid: uid,
                    wanted_uid: user.uid,
                })
            };
        }
        if uid == user.uid && same_uid.is_none() {
            same_uid = Some(name.to_string());
        }
    }
    if let Some(name) = same_uid {
        return Ok(PasswdEntry::PresentAs(name));
    }
    Ok(PasswdEntry::Added(append_line(
        passwd,
        &format!(
            "{}:x:{}:{}::{}:{CREATED_USER_SHELL}",
            user.name, user.uid, user.gid, user.home
        ),
    )))
}

/// Find or add the group `name` with `gid` in `group` (the file's
/// contents). `None` when nothing needs to change.
pub fn ensure_group_entry(
    group: &str,
    name: &str,
    gid: u32,
) -> Result<Option<String>, SandboxUserError> {
    let mut gid_taken = false;
    for (existing, existing_gid) in group.lines().filter_map(account_name_and_id) {
        if existing == name {
            return if existing_gid == gid {
                Ok(None)
            } else {
                Err(SandboxUserError::GroupNameTakenByOtherGid {
                    name: name.to_string(),
                    existing_gid,
                    wanted_gid: gid,
                })
            };
        }
        gid_taken |= existing_gid == gid;
    }
    if gid_taken {
        // The gid already has a group under another name; the user runs
        // with that gid all the same.
        return Ok(None);
    }
    Ok(Some(append_line(group, &format!("{name}:x:{gid}:"))))
}

/// The uid, gid and `HOME` an exec request for `uid` runs with.
///
/// Root (or no uid) keeps `/root`. The sandbox user gets its own gid and
/// home. Any other uid runs with gid = uid and the home `passwd` lists for
/// it, or `/` when it has none.
pub fn exec_identity(
    uid: Option<u32>,
    sandbox_user: Option<&SandboxUser>,
    passwd: &str,
) -> (Option<(u32, u32)>, String) {
    match uid {
        None | Some(0) => (uid.map(|_| (0, 0)), "/root".to_string()),
        Some(uid) => match sandbox_user.filter(|user| user.uid == uid) {
            Some(user) => (Some((user.uid, user.gid)), user.home.clone()),
            None => (
                Some((uid, uid)),
                passwd_home(passwd, uid).unwrap_or("/").to_string(),
            ),
        },
    }
}

/// Argument that makes the agent binary run one file operation as the
/// user it was started as, then exit (see [`run_file_op`]). PID 1 starts it
/// with the sandbox user's uid and gid for every write into that user's
/// files.
pub const AS_USER_FLAG: &str = "--as-user";

/// A file operation PID 1 delegates to a child running as the sandbox user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Write stdin to the file, creating missing parent directories.
    Write { path: String, mode: u32 },
    /// Create the directory and its missing parents.
    Mkdir { path: String, mode: u32 },
}

impl FileOp {
    /// The arguments after [`AS_USER_FLAG`] that describe this operation.
    pub fn args(&self) -> Vec<String> {
        let (op, path, mode) = match self {
            Self::Write { path, mode } => ("write", path, mode),
            Self::Mkdir { path, mode } => ("mkdir", path, mode),
        };
        vec![op.to_string(), path.clone(), format!("{mode:o}")]
    }

    /// Parse the arguments after [`AS_USER_FLAG`].
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let [op, path, mode] = args else {
            return Err(format!(
                "expected <write|mkdir> <path> <octal mode>, got {args:?}"
            ));
        };
        let mode = u32::from_str_radix(mode, 8).map_err(|_| format!("bad mode '{mode}'"))?;
        let path = path.clone();
        match op.as_str() {
            "write" => Ok(Self::Write { path, mode }),
            "mkdir" => Ok(Self::Mkdir { path, mode }),
            other => Err(format!("unknown operation '{other}'")),
        }
    }
}

/// Run `op` as the current process's user, reading a write's contents from
/// `input` (at most `max_bytes`).
pub fn run_file_op(op: &FileOp, input: impl std::io::Read, max_bytes: u64) -> std::io::Result<()> {
    match op {
        FileOp::Write { path, mode } => {
            let mut data = Vec::new();
            input
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut data)?;
            if data.len() as u64 > max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("contents for {path} exceed {max_bytes} bytes"),
                ));
            }
            let target = Path::new(path);
            let parent = target.parent().unwrap_or(Path::new("/"));
            std::fs::create_dir_all(parent)?;
            // Staged beside the target and renamed over it, so a file the
            // user left read-only (a `0400` upload) can still be replaced,
            // and a planted symlink at the target is replaced, not followed.
            if target.file_name().is_none() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{path} names no file"),
                ));
            }
            // A short sibling name, independent of the target's (a valid
            // 255-byte name plus a prefix would not fit), unique per write.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let staged = parent.join(format!(
                ".temps-write-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // Created exclusively: a name already taken (a planted file or
            // symlink) fails here and is never touched.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staged)?;
            let written = file
                .write_all(&data)
                .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(*mode)))
                .and_then(|()| std::fs::rename(&staged, target));
            if written.is_err() {
                // Only the file this write created.
                let _ = std::fs::remove_file(&staged);
            }
            written
        }
        FileOp::Mkdir { path, mode } => {
            std::fs::create_dir_all(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))
        }
    }
}

/// Why a host write into the sandbox user's files was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OwnedWriteError {
    #[error("refusing to write {path}: it contains '..'")]
    ParentComponent { path: String },
}

/// Whether a host write at `path` goes into the sandbox user's files (its
/// home, or the workspace, which in a microVM lives outside the home), and
/// so runs as that user rather than as root. Files the Docker backend
/// uploads under the user's home belong to the user the same way.
///
/// Running the write as the user is what makes it safe: the kernel then
/// refuses anything the user could not do itself, so a symlink the user
/// planted in its own files cannot redirect a root write elsewhere.
pub fn is_user_file(
    path: &str,
    user: &SandboxUser,
    work_dir: &str,
) -> Result<bool, OwnedWriteError> {
    // A relative path is resolved from `/`, the writers' working directory.
    let absolute = Path::new("/").join(path);
    let has_parent_dir = absolute
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir));
    let in_user_area =
        |candidate: &Path| candidate.starts_with(&user.home) || candidate.starts_with(work_dir);
    if !has_parent_dir {
        return Ok(in_user_area(&absolute));
    }
    // A `..` is refused when the path is spelled inside the user's area or
    // lands there (`/tmp/../workspace/x` is the workspace too, and must not
    // reach the root writer).
    if in_user_area(&absolute) || in_user_area(&lexically_normal(&absolute)) {
        return Err(OwnedWriteError::ParentComponent {
            path: path.to_string(),
        });
    }
    Ok(false)
}

/// `path` with `.` and `..` applied by name (`..` stops at `/`).
fn lexically_normal(path: &Path) -> std::path::PathBuf {
    let mut normal = std::path::PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => normal.push(name),
            std::path::Component::ParentDir => {
                normal.pop();
            }
            _ => {}
        }
    }
    normal
}

fn passwd_home(passwd: &str, uid: u32) -> Option<&str> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.as_slice() {
            [_, _, id, _, _, home, ..]
                if id.parse::<u32>().ok() == Some(uid) && home.starts_with('/') =>
            {
                Some(*home)
            }
            _ => None,
        }
    })
}

/// `name` and numeric id of a `passwd` or `group` line.
fn account_name_and_id(line: &str) -> Option<(&str, u32)> {
    let mut fields = line.split(':');
    let name = fields.next()?.trim();
    let _password = fields.next()?;
    let id = fields.next()?.trim().parse::<u32>().ok()?;
    (!name.is_empty() && !name.starts_with('#')).then_some((name, id))
}

fn append_line(contents: &str, line: &str) -> String {
    let mut updated = contents.to_string();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated.push('\n');
    updated
}

fn is_account_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !name.starts_with('-')
}

/// Read an account file (`passwd`, `group`) of at most
/// [`MAX_ACCOUNT_FILE_BYTES`]. The file is opened once and measured as
/// opened, so a symlink to something huge (or to a device) is refused
/// instead of read. A missing file reads as empty: a distroless image may
/// ship no account files at all.
pub fn read_account_file(path: &Path) -> Result<String, SandboxUserError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(SandboxUserError::io("open", path, error)),
    };
    let meta = file
        .metadata()
        .map_err(|e| SandboxUserError::io("inspect", path, e))?;
    if !meta.is_file() {
        return Err(SandboxUserError::AccountFileNotRegular {
            path: path.display().to_string(),
        });
    }
    let mut contents = String::new();
    file.take(MAX_ACCOUNT_FILE_BYTES + 1)
        .read_to_string(&mut contents)
        .map_err(|e| SandboxUserError::io("read", path, e))?;
    if contents.len() as u64 > MAX_ACCOUNT_FILE_BYTES {
        return Err(SandboxUserError::AccountFileTooLarge {
            path: path.display().to_string(),
        });
    }
    Ok(contents)
}

/// Replace an account file through a sibling temporary file, so a crash
/// mid-write never leaves the image without a `passwd`. A symlinked file is
/// replaced by a regular one rather than written through.
fn write_account_file(path: &Path, contents: &str) -> Result<(), SandboxUserError> {
    replace_file(path, contents, 0o644)
}

/// Write `contents` to `path` (mode `mode`) through a sibling temporary file
/// renamed over it, creating missing parents.
fn replace_file(path: &Path, contents: &str, mode: u32) -> Result<(), SandboxUserError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        require_real_dirs(parent)?;
        std::fs::create_dir_all(parent).map_err(|e| SandboxUserError::io("create", parent, e))?;
    }
    let temporary = path.with_extension("temps-new");
    // A stale entry at the staging name (an image may ship one, a symlink
    // included) is unlinked, never followed; the file is then created
    // exclusively, its mode set on the open handle before it is written.
    match std::fs::remove_file(&temporary) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(SandboxUserError::io("remove", &temporary, e))
        }
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| SandboxUserError::io("create", &temporary, e))?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .and_then(|()| file.write_all(contents.as_bytes()))
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temporary);
            SandboxUserError::io("write", path, e)
        })
}

/// Refuse a directory that is, or sits under, a symlink root does not own:
/// exclusive creation protects only the last component of what root is
/// about to write. Root's own links (a merged `/usr`'s `/lib`) are the
/// system's layout; one the sandbox user, or the unconverted image, owns
/// could aim root's write anywhere.
fn require_real_dirs(dir: &Path) -> Result<(), SandboxUserError> {
    let mut current = std::path::PathBuf::new();
    for component in dir.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() && meta.uid() != 0 => {
                return Err(SandboxUserError::Io {
                    operation: "write under",
                    path: current.display().to_string(),
                    reason: format!("it is a symbolic link owned by uid {}", meta.uid()),
                })
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(SandboxUserError::io("inspect", &current, e)),
        }
    }
    Ok(())
}

/// Settle the sandbox user in the order the host relies on: `prepare` it,
/// hand the result to `start_terminals` (which registers the terminal
/// listener, or decides terminals are unavailable), and only then `publish`
/// it. The host treats a published `ready` as "the sandbox is usable",
/// terminals included.
pub fn settle(
    prepare: impl FnOnce() -> crate::SandboxUserStatus,
    start_terminals: impl FnOnce(&crate::SandboxUserStatus),
    publish: impl FnOnce(crate::SandboxUserStatus),
) {
    let status = prepare();
    start_terminals(&status);
    publish(status);
}

/// Where the guest records how far handing the converter's files back to
/// root got (see [`normalize_rootfs_owner`]).
pub const NORMALIZATION_RECORD: &str = "/var/lib/temps-vm-agent/rootfs-owner";

/// How far [`normalize_converter_owner`] got on this rootfs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalization {
    /// Never started: the rootfs has no record.
    NotStarted,
    /// Started for the converter `uid:gid` and not known to have finished;
    /// the next boot runs it again for the same owner.
    Pending { uid: u32, gid: u32 },
    /// Finished.
    Done,
}

/// The longest record [`read_normalization`] reads (`pending <u32> <u32>`).
const MAX_NORMALIZATION_RECORD_BYTES: u64 = 64;

/// Read the record at `path`.
///
/// Only a record this guest wrote counts: it is root's, while anything the
/// image shipped at that path was extracted with the converter's uid (the
/// host converts unprivileged). An image cannot therefore mark the walk
/// done; a record it ships reads as [`Normalization::NotStarted`] and is
/// overwritten. The file is opened without following a symlink or blocking
/// on a FIFO, must be regular, and is read only up to a few bytes.
pub fn read_normalization(path: &Path) -> Result<Normalization, SandboxUserError> {
    read_normalization_owned_by(path, 0)
}

fn read_normalization_owned_by(
    path: &Path,
    trusted_owner: u32,
) -> Result<Normalization, SandboxUserError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Normalization::NotStarted)
        }
        Err(error) => return Err(SandboxUserError::io("open", path, error)),
    };
    let meta = file
        .metadata()
        .map_err(|e| SandboxUserError::io("inspect", path, e))?;
    if !meta.is_file() {
        return Err(SandboxUserError::BadNormalizationRecord {
            path: path.display().to_string(),
            contents: "not a regular file".to_string(),
        });
    }
    if meta.uid() != trusted_owner {
        return Ok(Normalization::NotStarted);
    }
    let mut raw = Vec::new();
    file.take(MAX_NORMALIZATION_RECORD_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| SandboxUserError::io("read", path, e))?;
    let contents = String::from_utf8_lossy(&raw);
    if raw.len() as u64 > MAX_NORMALIZATION_RECORD_BYTES {
        return Err(SandboxUserError::BadNormalizationRecord {
            path: path.display().to_string(),
            contents: format!("over {MAX_NORMALIZATION_RECORD_BYTES} bytes"),
        });
    }
    let bad = || SandboxUserError::BadNormalizationRecord {
        path: path.display().to_string(),
        contents: contents.chars().take(64).collect(),
    };
    let mut words = contents.split_ascii_whitespace();
    match (words.next(), words.next(), words.next(), words.next()) {
        (Some("done"), None, None, None) => Ok(Normalization::Done),
        (Some("pending"), Some(uid), Some(gid), None) => Ok(Normalization::Pending {
            uid: uid.parse().map_err(|_| bad())?,
            gid: gid.parse().map_err(|_| bad())?,
        }),
        _ => Err(bad()),
    }
}

fn write_normalization(path: &Path, state: Normalization) -> Result<(), SandboxUserError> {
    let contents = match state {
        Normalization::NotStarted => return Ok(()),
        Normalization::Pending { uid, gid } => format!("pending {uid} {gid}\n"),
        Normalization::Done => "done\n".to_string(),
    };
    replace_file(path, &contents, 0o600)
}

/// Hand the converter's files back to root, once per rootfs, resumably.
///
/// `exe_owner` is the owner of the agent binary, which the host injected
/// with the converted files, so it names the converter until the walk
/// reaches it. Which converter to hand back is therefore recorded at
/// `record` *before* the walk changes anything, and `done` only after it
/// returns `Ok`: a walk that fails half-way runs again at the next boot for
/// the same owner, even though the binary is root's by then. `normalize`
/// does the walk ([`normalize_converter_owner`] in the guest). Returns the
/// entries it changed, `None` when nothing needed doing.
pub fn normalize_rootfs_owner(
    record: &Path,
    exe_owner: (u32, u32),
    normalize: impl FnOnce((u32, u32)) -> Result<u64, SandboxUserError>,
) -> Result<Option<u64>, SandboxUserError> {
    normalize_rootfs_owner_as(record, 0, exe_owner, normalize)
}

/// [`normalize_rootfs_owner`] for a guest whose own records are owned by
/// `trusted_owner` (root in a VM; the test runner in tests).
fn normalize_rootfs_owner_as(
    record: &Path,
    trusted_owner: u32,
    exe_owner: (u32, u32),
    normalize: impl FnOnce((u32, u32)) -> Result<u64, SandboxUserError>,
) -> Result<Option<u64>, SandboxUserError> {
    let converter = match read_normalization_owned_by(record, trusted_owner)? {
        Normalization::Done => return Ok(None),
        Normalization::Pending { uid, gid } => (uid, gid),
        Normalization::NotStarted if exe_owner.0 == 0 => return Ok(None),
        Normalization::NotStarted => exe_owner,
    };
    write_normalization(
        record,
        Normalization::Pending {
            uid: converter.0,
            gid: converter.1,
        },
    )?;
    let changed = normalize(converter)?;
    write_normalization(record, Normalization::Done)?;
    Ok(Some(changed))
}

/// Hand back to root every entry of the filesystem at `root` still owned by
/// `converter` (`uid:gid`), the host account that extracted the rootfs, so
/// the guest's system files are not the sandbox user's when the two uids
/// match. The `keep` trees (the user's home and the workspace) are skipped:
/// [`prepare`] gives them to the user. Other filesystems mounted under
/// `root` (`/proc`, `/dev`, `/run`, `/tmp`) are not entered, and symlinks
/// are never followed. Returns how many entries changed.
pub fn normalize_converter_owner(
    root: &Path,
    converter: (u32, u32),
    keep: &[&Path],
) -> Result<u64, SandboxUserError> {
    let mut changed = 0;
    walk_owned_by(root, converter.0, keep, |path, meta| {
        let gid = (meta.gid() == converter.1).then_some(0);
        lchown(path, Some(0), gid).map_err(|e| SandboxUserError::io("chown", path, e))?;
        changed += 1;
        Ok(())
    })?;
    Ok(changed)
}

/// Call `visit` for every entry under `root` (same filesystem, symlinks not
/// followed, `keep` trees skipped) whose owner is `uid`.
pub fn walk_owned_by(
    root: &Path,
    uid: u32,
    keep: &[&Path],
    mut visit: impl FnMut(&Path, &std::fs::Metadata) -> Result<(), SandboxUserError>,
) -> Result<(), SandboxUserError> {
    let root_meta =
        std::fs::symlink_metadata(root).map_err(|e| SandboxUserError::io("inspect", root, e))?;
    let device = root_meta.dev();
    let mut pending = vec![(root.to_path_buf(), root_meta)];
    while let Some((path, meta)) = pending.pop() {
        if meta.dev() != device || keep.iter().any(|kept| path == *kept) {
            continue;
        }
        if meta.uid() == uid {
            visit(&path, &meta)?;
        }
        if meta.file_type().is_dir() {
            let entries =
                std::fs::read_dir(&path).map_err(|e| SandboxUserError::io("list", &path, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| SandboxUserError::io("list", &path, e))?;
                let child = entry.path();
                let child_meta = std::fs::symlink_metadata(&child)
                    .map_err(|e| SandboxUserError::io("inspect", &child, e))?;
                pending.push((child, child_meta));
            }
        }
    }
    Ok(())
}

/// Give every entry under `root` (and `root`) to `uid:gid`, without
/// following symlinks or crossing into another filesystem. Entries already
/// owned are left alone, so a second boot touches nothing.
pub fn chown_tree(root: &Path, uid: u32, gid: u32) -> Result<u64, SandboxUserError> {
    let root_meta =
        std::fs::symlink_metadata(root).map_err(|e| SandboxUserError::io("inspect", root, e))?;
    let device = root_meta.dev();
    let mut changed = 0;
    let mut pending = vec![(root.to_path_buf(), root_meta)];
    while let Some((path, meta)) = pending.pop() {
        if meta.dev() != device {
            continue;
        }
        if meta.uid() != uid || meta.gid() != gid {
            lchown(&path, Some(uid), Some(gid))
                .map_err(|e| SandboxUserError::io("chown", &path, e))?;
            changed += 1;
        }
        if meta.file_type().is_dir() {
            let entries =
                std::fs::read_dir(&path).map_err(|e| SandboxUserError::io("list", &path, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| SandboxUserError::io("list", &path, e))?;
                let child = entry.path();
                let child_meta = std::fs::symlink_metadata(&child)
                    .map_err(|e| SandboxUserError::io("inspect", &child, e))?;
                pending.push((child, child_meta));
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
