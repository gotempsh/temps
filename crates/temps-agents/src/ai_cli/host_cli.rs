// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One identity for every host-side provider CLI invocation.
//!
//! Status probes, model discovery, turns and the sign-in flow must all run as
//! the same OS user with the same `HOME`, otherwise "signed in" is a statement
//! about one user's credential store while turns read another's. Everything
//! that launches a provider CLI on the Temps host goes through
//! [`host_command`].
//!
//! Layout. When an operator signs in through Temps the CLI owns a private
//! directory below the Temps data directory:
//!
//! ```text
//! <state root>/                          owned by the Temps server, 0711
//!     <provider id>/                     owned by the Temps server, 0711
//!         home/                          HOME of the CLI process; owned by the run user
//!             .claude/                   CLAUDE_CONFIG_DIR (or .codex/ for CODEX_HOME)
//!                 .credentials.json      written and refreshed by the CLI only
//!         login-<id>/                    scratch home for an in-flight sign-in
//! ```
//!
//! Trust boundary. When the server is root the CLI runs as an unprivileged
//! user that owns `home/` and everything below it. That user can therefore
//! rewrite anything under `home/`, so the server never `chown`s, `chmod`s or
//! reads through a path inside it with a symlink-following call: the shared
//! parents stay server-owned (the run user cannot replace them with a
//! symlink), new directories are created by the server and handed over with
//! `lchown`, and credential files are read with `O_NOFOLLOW` component by
//! component ([`read_regular_file_below`]).
//!
//! Hosts that were authenticated out of band keep working: the managed
//! location is only used once a credential file exists in it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use tokio::process::Command;

use super::login::LoginError;
use super::sanitize_command_environment;
use crate::sandbox::user::SANDBOX_USER;

static STATE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Record the directory that holds Temps-owned provider CLI state. Called once
/// during plugin start-up; later calls are ignored so a running server cannot
/// have its CLI home swapped underneath an active sign-in.
pub fn set_state_root(root: PathBuf) {
    let _ = STATE_ROOT.set(root);
}

pub fn state_root() -> Option<&'static Path> {
    STATE_ROOT.get().map(PathBuf::as_path)
}

/// Where one provider CLI keeps its credential store for a given `HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLocation {
    /// `HOME` of the CLI process.
    pub home: PathBuf,
    /// Value of the provider's config-dir variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`).
    pub config_dir: PathBuf,
    /// Name of that variable.
    pub env_var: &'static str,
    /// File the CLI writes its credential to inside `config_dir`.
    pub credential_file: PathBuf,
}

impl ProviderLocation {
    /// Location of `provider_id`'s credential store below `home`, or `None`
    /// for providers whose store Temps does not manage.
    pub fn below_home(provider_id: &str, home: &Path) -> Option<Self> {
        let (dir_name, env_var, credential_name) = match provider_id {
            "claude_cli" => (".claude", "CLAUDE_CONFIG_DIR", ".credentials.json"),
            "codex_cli" => (".codex", "CODEX_HOME", "auth.json"),
            _ => return None,
        };
        let config_dir = home.join(dir_name);
        Some(Self {
            home: home.to_path_buf(),
            credential_file: config_dir.join(credential_name),
            config_dir,
            env_var,
        })
    }

    /// Path components of the credential file below `home`, for reads that
    /// must not follow symlinks (see [`read_regular_file_below`]).
    pub fn credential_relative(&self) -> Vec<String> {
        self.credential_file
            .strip_prefix(&self.home)
            .map(|relative| {
                relative
                    .components()
                    .filter_map(|component| component.as_os_str().to_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Inverse of [`Self::below_home`] for a caller that was handed the
    /// config directory: `HOME` is its parent.
    pub fn from_config_dir(provider_id: &str, config_dir: &Path) -> Option<Self> {
        let home = config_dir.parent()?;
        let location = Self::below_home(provider_id, home)?;
        (location.config_dir == config_dir).then_some(location)
    }
}

/// The canonical Temps-managed location for `provider_id`, whether or not a
/// credential has been written to it yet.
pub fn managed_location(provider_id: &str) -> Option<ProviderLocation> {
    ProviderLocation::below_home(provider_id, &managed_home(state_root()?, provider_id))
}

/// `HOME` of the promoted sign-in for `provider_id` below `state_root`.
pub fn managed_home(state_root: &Path, provider_id: &str) -> PathBuf {
    state_root.join(provider_id).join("home")
}

/// The managed location, but only when a sign-in through Temps has produced a
/// credential there. Status and turns use this so that a host authenticated
/// out of band is left exactly as it was.
pub fn active_managed_location(provider_id: &str) -> Option<ProviderLocation> {
    managed_location(provider_id).filter(|location| location.credential_file.is_file())
}

/// OS user a provider CLI executes as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    /// `Some(user)` when the server is root and must drop privileges.
    pub run_user: Option<String>,
}

impl HostIdentity {
    /// Run as the server's own user.
    pub const fn current() -> Self {
        Self { run_user: None }
    }

    /// Resolve the identity every host-side CLI invocation uses.
    ///
    /// Claude Code refuses its bypass-permissions mode as root, so a root
    /// server drops to the sandbox user when it exists and to `nobody`
    /// otherwise. Status, sign-in and turns all use this answer.
    pub fn resolve() -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            return Self::current();
        }
        let user = if user_exists(SANDBOX_USER) {
            SANDBOX_USER
        } else {
            "nobody"
        };
        Self {
            run_user: Some(user.to_string()),
        }
    }
}

/// The identity a provider's CLI runs as. Claude Code drops privileges (it
/// refuses its bypass mode as root); Codex has always run as the server user,
/// so its directories must stay owned by the server user too — handing them to
/// an unprivileged user while a root process executes out of them would let
/// that user plant configuration root then loads.
pub fn identity_for(provider_id: &str) -> HostIdentity {
    match provider_id {
        "codex_cli" => HostIdentity::current(),
        _ => HostIdentity::resolve(),
    }
}

/// Check if a Unix user exists by name.
pub fn user_exists(name: &str) -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Build a provider CLI command with the host identity and, when given, the
/// managed credential location applied.
///
/// Every argument stays in its own argv slot; `runuser -u` has no shell
/// string parameter. The environment starts empty and receives only the
/// runtime allowlist, so provider credentials are never inherited from the
/// server process; callers add what they need explicitly.
pub fn host_command(
    program: impl AsRef<std::ffi::OsStr>,
    identity: &HostIdentity,
    location: Option<&ProviderLocation>,
) -> Command {
    let mut command = match identity.run_user.as_deref() {
        Some(user) => {
            let mut wrapped = Command::new("runuser");
            wrapped.arg("-u").arg(user).arg("--").arg(program);
            wrapped
        }
        None => Command::new(program),
    };
    sanitize_command_environment(&mut command);
    if let Some(location) = location {
        // Both are set so the result does not depend on whether `runuser -u`
        // resets HOME; an explicit config-dir variable wins over HOME.
        command.env("HOME", &location.home);
        command.env(location.env_var, &location.config_dir);
    }
    command.kill_on_drop(true);
    command
}

/// Make `path` a server-owned directory that the run user may traverse but
/// not modify. Used for the shared parents of every sign-in; because the run
/// user cannot write to them it cannot replace them with a symlink, and
/// because they are verified to be real directories owned by the server
/// before any permission change, a symlink planted earlier is refused rather
/// than followed.
pub fn ensure_shared_dir(path: &Path, identity: &HostIdentity) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is not a directory (symlinks are refused)",
                path.display()
            ),
        ));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let server_uid = unsafe { libc::geteuid() };
    if metadata.uid() != server_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {}, expected the server user (uid {})",
                path.display(),
                metadata.uid(),
                server_uid
            ),
        ));
    }
    let mode = if identity.run_user.is_some() {
        0o711
    } else {
        0o700
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Create a brand-new private directory and hand it to the run user.
///
/// `path` must not exist: the leaf is made by the server inside a shared
/// parent the run user cannot write to, locked to `0700`, and only then given
/// to the run user with `lchown`, so there is no window in which the run user
/// can substitute it.
pub fn create_private_leaf(path: &Path, identity: &HostIdentity) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    if let Some(user) = identity.run_user.as_deref() {
        let uid = lookup_id(user, "-u")?;
        let gid = lookup_id(user, "-g")?;
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    }
    Ok(())
}

/// Read a regular file at `relative` below `home` without following a symlink
/// at any component after `home` itself (which is opened with `O_NOFOLLOW`).
///
/// Used when the server reads a credential out of a directory the run user
/// owns: a plain `File::open` would follow a planted `.claude -> /root/.claude`
/// or `.credentials.json -> <other secret>` and import someone else's token.
pub fn read_regular_file_below(
    home: &Path,
    relative: &[String],
    max_bytes: u64,
) -> std::io::Result<Vec<u8>> {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let Some((file_name, directories)) = relative.split_last() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no file name given",
        ));
    };
    let invalid = |what: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid path component {what:?}"),
        )
    };

    let open_dir = |dir: Option<&OwnedFd>, name: &std::ffi::OsStr| -> std::io::Result<OwnedFd> {
        let name = CString::new(name.as_bytes()).map_err(|_| invalid("(nul byte)"))?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `name` is a valid NUL-terminated string that outlives the
        // call, and `dir` (when present) is a live descriptor we own.
        let fd = unsafe {
            match dir {
                Some(dir) => libc::openat(dir.as_raw_fd(), name.as_ptr(), flags),
                None => libc::open(name.as_ptr(), flags),
            }
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned by open(2) and is owned by no one else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    };

    let mut current = open_dir(None, home.as_os_str())?;
    for component in directories {
        if component.is_empty() || component.contains('/') || component == ".." || component == "."
        {
            return Err(invalid(component));
        }
        current = open_dir(Some(&current), std::ffi::OsStr::new(component))?;
    }
    if file_name.is_empty() || file_name.contains('/') || file_name == ".." || file_name == "." {
        return Err(invalid(file_name));
    }
    let name = CString::new(file_name.as_bytes()).map_err(|_| invalid("(nul byte)"))?;
    // SAFETY: as above.
    let fd = unsafe {
        libc::openat(
            current.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by openat(2) and is owned by no one else.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential path is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file).take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file is larger than {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

fn lookup_id(user: &str, flag: &str) -> std::io::Result<u32> {
    let output = std::process::Command::new("id")
        .arg(flag)
        .arg(user)
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("user '{user}' does not exist"),
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("id {flag} {user} returned a non-numeric value: {error}"),
            )
        })
}

/// Create the home and config directories a sign-in writes to and confirm the
/// run user can use them, so a sign-in that could never save its credential
/// fails at once with an actionable error instead of after the browser step.
pub async fn prepare_login_location(
    provider_id: &str,
    identity: &HostIdentity,
    location: &ProviderLocation,
) -> Result<(), LoginError> {
    let io_error = |operation: &str, error: std::io::Error| LoginError::Io {
        provider_id: provider_id.to_string(),
        operation: operation.to_string(),
        reason: error.to_string(),
    };
    create_private_leaf(&location.home, identity)
        .map_err(|error| io_error("preparing the sign-in home directory", error))?;
    create_private_leaf(&location.config_dir, identity)
        .map_err(|error| io_error("preparing the sign-in config directory", error))?;
    if !run_user_can_write(identity, &location.config_dir).await {
        return Err(LoginError::StateDirectoryInaccessible {
            provider_id: provider_id.to_string(),
            run_user: identity.run_user.clone().unwrap_or_default(),
        });
    }
    Ok(())
}

/// Whether the run user can write `dir`. Only meaningful when privileges are
/// dropped; the server's own user is assumed to own what it created.
pub async fn run_user_can_write(identity: &HostIdentity, dir: &Path) -> bool {
    let Some(user) = identity.run_user.as_deref() else {
        return true;
    };
    Command::new("runuser")
        .arg("-u")
        .arg(user)
        .arg("--")
        .arg("test")
        .arg("-w")
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_location_uses_dot_claude_below_home() {
        let location = ProviderLocation::below_home("claude_cli", Path::new("/state/claude_cli"))
            .expect("claude location");
        assert_eq!(location.config_dir, Path::new("/state/claude_cli/.claude"));
        assert_eq!(location.env_var, "CLAUDE_CONFIG_DIR");
        assert_eq!(
            location.credential_file,
            Path::new("/state/claude_cli/.claude/.credentials.json")
        );
    }

    #[test]
    fn codex_location_uses_codex_home() {
        let location = ProviderLocation::below_home("codex_cli", Path::new("/state/codex_cli"))
            .expect("codex location");
        assert_eq!(location.env_var, "CODEX_HOME");
        assert!(location.credential_file.ends_with(".codex/auth.json"));
    }

    #[test]
    fn unmanaged_providers_have_no_location() {
        assert!(ProviderLocation::below_home("opencode", Path::new("/state/opencode")).is_none());
    }

    #[test]
    fn config_dir_round_trips_to_its_home() {
        let location = ProviderLocation::from_config_dir(
            "claude_cli",
            Path::new("/state/claude_cli/login-1/.claude"),
        )
        .expect("location");
        assert_eq!(location.home, Path::new("/state/claude_cli/login-1"));
        assert!(
            ProviderLocation::from_config_dir("claude_cli", Path::new("/state/elsewhere"))
                .is_none()
        );
    }

    #[test]
    fn host_command_applies_managed_location_without_inheriting_secrets() {
        let location = ProviderLocation::below_home("claude_cli", Path::new("/state/claude_cli"))
            .expect("location");
        let command = host_command("claude", &HostIdentity::current(), Some(&location));
        let environment = command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            environment.get("CLAUDE_CONFIG_DIR").cloned().flatten(),
            Some("/state/claude_cli/.claude".to_string())
        );
        assert_eq!(
            environment.get("HOME").cloned().flatten(),
            Some("/state/claude_cli".to_string())
        );
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn dropped_privileges_wrap_the_program_in_runuser() {
        let identity = HostIdentity {
            run_user: Some("temps".to_string()),
        };
        let command = host_command("claude", &identity, None);
        let std_command = command.as_std();
        assert_eq!(std_command.get_program(), "runuser");
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["-u", "temps", "--", "claude"]);
    }

    #[test]
    fn private_leaf_is_owner_only_and_must_be_new() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("leaf");
        create_private_leaf(&dir, &HostIdentity::current()).expect("create");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(
            create_private_leaf(&dir, &HostIdentity::current()).is_err(),
            "an existing path must never be adopted"
        );
    }

    #[test]
    fn shared_dir_refuses_a_symlink_instead_of_following_it() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("victim");
        std::fs::create_dir(&target).expect("victim");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("chmod victim");
        let link = temp.path().join("shared");
        symlink(&target, &link).expect("symlink");

        let error = ensure_shared_dir(&link, &HostIdentity::current())
            .expect_err("a symlink must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        // The symlink target was left exactly as it was.
        let mode = std::fs::metadata(&target)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn shared_dir_is_created_and_traversable_when_privileges_drop() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("a").join("b");
        let dropped = HostIdentity {
            run_user: Some("temps".to_string()),
        };
        ensure_shared_dir(&dir, &dropped).expect("ensure");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o711);
        ensure_shared_dir(&dir, &HostIdentity::current()).expect("idempotent");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn credential_reads_refuse_symlinked_components() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let config = home.join(".claude");
        std::fs::create_dir_all(&config).expect("config");
        let relative = vec![".claude".to_string(), ".credentials.json".to_string()];

        // A regular file is read.
        std::fs::write(config.join(".credentials.json"), b"{\"ok\":true}").expect("write");
        assert_eq!(
            read_regular_file_below(&home, &relative, 1024).expect("read"),
            b"{\"ok\":true}"
        );

        // A symlinked file is refused even though its target is readable.
        let secret = temp.path().join("secret.json");
        std::fs::write(&secret, b"{\"stolen\":true}").expect("secret");
        std::fs::remove_file(config.join(".credentials.json")).expect("rm");
        symlink(&secret, config.join(".credentials.json")).expect("symlink file");
        assert!(read_regular_file_below(&home, &relative, 1024).is_err());

        // A symlinked config directory is refused too.
        std::fs::remove_file(config.join(".credentials.json")).expect("rm");
        std::fs::remove_dir(&config).expect("rmdir");
        let other = temp.path().join("other");
        std::fs::create_dir(&other).expect("other");
        std::fs::write(other.join(".credentials.json"), b"{\"stolen\":true}").expect("w");
        symlink(&other, &config).expect("symlink dir");
        assert!(read_regular_file_below(&home, &relative, 1024).is_err());
    }

    #[test]
    fn credential_reads_are_size_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).expect("config");
        std::fs::write(home.join(".claude/.credentials.json"), vec![b'x'; 100]).expect("w");
        let relative = vec![".claude".to_string(), ".credentials.json".to_string()];
        assert!(read_regular_file_below(&home, &relative, 50).is_err());
        assert_eq!(
            read_regular_file_below(&home, &relative, 100)
                .expect("fits")
                .len(),
            100
        );
    }

    #[test]
    fn codex_keeps_the_server_identity_so_its_files_never_change_owner() {
        assert_eq!(identity_for("codex_cli"), HostIdentity::current());
    }

    #[test]
    fn managed_home_sits_below_a_server_owned_provider_directory() {
        assert_eq!(
            managed_home(Path::new("/state"), "claude_cli"),
            Path::new("/state/claude_cli/home")
        );
    }
}
