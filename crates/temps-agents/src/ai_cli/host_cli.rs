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
//! <state root>/<provider id>/            HOME of the CLI process
//!     .claude/                           CLAUDE_CONFIG_DIR (or .codex/ for CODEX_HOME)
//!         .credentials.json              written and refreshed by the CLI only
//! ```
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
    ProviderLocation::below_home(provider_id, &state_root()?.join(provider_id))
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

/// Create `path` (and parents) readable only by its owner, handing ownership
/// of the leaf to the run user when privileges are dropped.
pub fn prepare_private_dir(path: &Path, identity: &HostIdentity) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    if let Some(user) = identity.run_user.as_deref() {
        let uid = lookup_id(user, "-u")?;
        let gid = lookup_id(user, "-g")?;
        std::os::unix::fs::chown(path, Some(uid), Some(gid))?;
    }
    Ok(())
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
    prepare_private_dir(&location.home, identity)
        .map_err(|error| io_error("preparing the sign-in home directory", error))?;
    prepare_private_dir(&location.config_dir, identity)
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
    fn private_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("a").join("b");
        prepare_private_dir(&dir, &HostIdentity::current()).expect("prepare");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}
