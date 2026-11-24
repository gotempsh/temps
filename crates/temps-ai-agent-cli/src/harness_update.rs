// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure helpers for checking and updating a harness CLI inside a persistent
//! workspace.
//!
//! A harness that is behind the latest release cannot offer models published
//! after it. The workspace image bundles one version, so it drifts; these
//! helpers let the console notice and let the operator update it in place.

use std::time::Duration;

/// How long a version check stays fresh before the workspace is asked again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Upper bounds for the commands run inside the workspace.
pub(crate) const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const LATEST_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const UPDATE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How a provider's CLI is installed in a workspace.
///
/// The runtime workspace image installs every CLI with npm into a prefix the
/// sandbox user owns. The local fallback image uses each vendor's own
/// installer under the sandbox user's home instead (Claude Code's native
/// installer, `bun add -g` for Codex, OpenCode's install script), so an npm
/// install there would add a second copy that the original shadows on `PATH`.
/// Updates therefore go through whichever installer owns the binary that
/// actually runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPackage {
    /// Temps provider id (`claude_cli`, `codex_cli`, `opencode`).
    pub provider_id: &'static str,
    /// Product name shown to operators.
    pub name: &'static str,
    pub binary: &'static str,
    pub npm_package: &'static str,
    /// The vendor installer's update, for a copy installed under `$HOME`.
    /// A fixed shell snippet: nothing from a request reaches it.
    home_update: &'static str,
}

const PACKAGES: &[HarnessPackage] = &[
    HarnessPackage {
        provider_id: "claude_cli",
        name: "Claude Code",
        binary: "claude",
        npm_package: "@anthropic-ai/claude-code",
        home_update: "claude update",
    },
    HarnessPackage {
        provider_id: "codex_cli",
        name: "Codex",
        binary: "codex",
        npm_package: "@openai/codex",
        home_update: r#"BUN_INSTALL="$HOME/.bun" BUN_INSTALL_BIN="$HOME/.bun/bin" bun add -g @openai/codex@latest"#,
    },
    HarnessPackage {
        provider_id: "opencode",
        name: "OpenCode",
        binary: "opencode",
        npm_package: "opencode-ai",
        home_update: "opencode upgrade --method curl",
    },
];

/// Every coding-agent CLI Temps can check and update.
pub fn harness_packages() -> &'static [HarnessPackage] {
    PACKAGES
}

/// Packages are fixed constants: nothing from a request reaches a command.
pub fn harness_package(provider_id: &str) -> Option<HarnessPackage> {
    PACKAGES
        .iter()
        .find(|package| package.provider_id == provider_id)
        .copied()
}

/// Shell prelude that resolves the binary that actually runs and npm's
/// global prefix. Exits 3 when npm cannot report a prefix and 4 when the
/// binary is not on `PATH`.
fn locate(package: HarnessPackage) -> String {
    format!(
        r#"prefix="$(npm config get prefix)" || exit 3; [ -n "$prefix" ] || exit 3; bin="$(command -v {binary})" || exit 4; bin="$(readlink -f "$bin")" || exit 4;"#,
        binary = package.binary
    )
}

/// Run `argv` under coreutils `timeout` so the command ends itself inside the
/// sandbox. Dropping the client side of an exec does not stop the process; a
/// stalled registry request or install would otherwise outlive its caller.
fn bounded(seconds: u64, argv: Vec<String>) -> Vec<String> {
    let mut command = vec![
        "timeout".to_string(),
        "-k".to_string(),
        "10".to_string(),
        seconds.to_string(),
    ];
    command.extend(argv);
    command
}

pub(crate) fn version_command(package: HarnessPackage) -> Vec<String> {
    bounded(
        VERSION_COMMAND_TIMEOUT.as_secs().saturating_sub(10).max(1),
        vec![package.binary.to_string(), "--version".to_string()],
    )
}

pub(crate) fn latest_version_command(package: HarnessPackage) -> Vec<String> {
    bounded(
        LATEST_COMMAND_TIMEOUT.as_secs().saturating_sub(10).max(1),
        vec![
            "npm".to_string(),
            "view".to_string(),
            package.npm_package.to_string(),
            "version".to_string(),
        ],
    )
}

/// Exit status of the install probe when the sandbox user can write the
/// global package directory.
const PROBE_WRITABLE: i32 = 0;
/// Exit status of the install probe when it ran and the directory is not
/// writable. Deliberately not `1`: a failed `npm`, a `timeout` kill (124/137)
/// or a missing shell (127) must not read as "owned by root".
const PROBE_NOT_WRITABLE: i32 = 10;

/// Who may replace a global npm package in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallOwner {
    /// The sandbox user owns the prefix: the update runs as that user.
    SandboxUser,
    /// An older image installed the CLIs as root: only root can replace them.
    Root,
}

/// Whether the sandbox user can update the CLI itself.
///
/// Current runtime images install the harness CLIs into a prefix the sandbox
/// user owns, so the update must run as that user: root running npm inside a
/// tree the sandbox controls would follow sandbox-planted links and leave
/// root-owned files that break the CLI's own self-update. Older images
/// installed them as root, where only a root update can succeed. A copy a
/// vendor installer put under the sandbox user's home is the sandbox user's.
/// The probe runs as the sandbox user and reports the answer in its exit
/// status: [`PROBE_WRITABLE`], [`PROBE_NOT_WRITABLE`], or any other status
/// when it could not tell (`npm` failed, no binary, an install outside both
/// npm's prefix and the home directory, killed by the timeout).
pub(crate) fn install_writable_probe_command(package: HarnessPackage) -> Vec<String> {
    bounded(
        VERSION_COMMAND_TIMEOUT.as_secs().saturating_sub(10).max(1),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                r#"{locate} case "$bin" in "$prefix"/*) ;; "$HOME"/*) exit {PROBE_WRITABLE};; *) exit 5;; esac; if test -w "$prefix/lib/node_modules"; then exit {PROBE_WRITABLE}; else exit {PROBE_NOT_WRITABLE}; fi"#,
                locate = locate(package)
            ),
        ],
    )
}

/// Read the probe's exit status. `None` means the probe could not establish
/// who owns the install, which callers must treat as an error rather than
/// picking an account.
pub(crate) fn install_owner_from_probe(exit_code: i32) -> Option<InstallOwner> {
    match exit_code {
        PROBE_WRITABLE => Some(InstallOwner::SandboxUser),
        PROBE_NOT_WRITABLE => Some(InstallOwner::Root),
        _ => None,
    }
}

/// Install the latest release with the installer that owns the running
/// binary: npm for a copy in npm's global prefix, the vendor's own updater
/// for a copy under the home directory. A binary anywhere else is left
/// alone (exit 5) rather than shadowed by a second install.
pub(crate) fn update_command(package: HarnessPackage) -> Vec<String> {
    bounded(
        UPDATE_COMMAND_TIMEOUT.as_secs().saturating_sub(30),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                r#"{locate} case "$bin" in "$prefix"/*) exec npm install -g {npm_package}@latest --no-fund --no-audit --loglevel=error;; "$HOME"/*) {home_update};; *) echo "{binary} at $bin was not installed by npm or under the home directory; update it where it was installed" >&2; exit 5;; esac"#,
                locate = locate(package),
                npm_package = package.npm_package,
                home_update = package.home_update,
                binary = package.binary,
            ),
        ],
    )
}

/// First `major.minor.patch` (optionally with a pre-release suffix) in `text`.
pub fn parse_version(text: &str) -> Option<String> {
    for token in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"')) {
        let candidate = token.trim_start_matches('v');
        let core = candidate.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn numeric_core(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// True only when both versions parse and `latest` is strictly newer.
/// Pre-release suffixes are ignored: a beta of the installed number is not an
/// update.
pub fn is_newer(latest: &str, installed: &str) -> bool {
    match (numeric_core(latest), numeric_core(installed)) {
        (Some(latest), Some(installed)) => latest > installed,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_how_every_harness_is_installed() {
        assert_eq!(
            harness_package("claude_cli").map(|p| p.npm_package),
            Some("@anthropic-ai/claude-code")
        );
        assert_eq!(
            harness_package("codex_cli").map(|p| p.binary),
            Some("codex")
        );
        assert_eq!(
            harness_package("opencode").map(|p| (p.binary, p.npm_package)),
            Some(("opencode", "opencode-ai"))
        );
        assert_eq!(harness_packages().len(), 3);
        assert!(harness_package("claude_cli; rm -rf /").is_none());
        assert!(harness_package("gateway").is_none());
    }

    #[test]
    fn commands_are_fixed_argument_vectors_that_bound_themselves() {
        let package = harness_package("claude_cli").expect("package");
        assert_eq!(
            version_command(package),
            ["timeout", "-k", "10", "10", "claude", "--version"]
        );
        assert_eq!(
            latest_version_command(package),
            [
                "timeout",
                "-k",
                "10",
                "20",
                "npm",
                "view",
                "@anthropic-ai/claude-code",
                "version"
            ]
        );
        let update = update_command(package);
        assert_eq!(&update[..6], ["timeout", "-k", "10", "270", "sh", "-c"]);
        assert_eq!(update.len(), 7);
    }

    #[test]
    fn an_update_goes_through_the_installer_that_owns_the_running_binary() {
        for package in harness_packages() {
            let script = &update_command(*package)[6];
            // The binary that runs is resolved through its links first.
            assert!(
                script.contains(&format!(r#"command -v {}"#, package.binary)),
                "{script}"
            );
            assert!(script.contains(r#"readlink -f "$bin""#), "{script}");
            // npm only for a copy in npm's prefix...
            assert!(
                script.contains(&format!(
                    r#""$prefix"/*) exec npm install -g {}@latest"#,
                    package.npm_package
                )),
                "{script}"
            );
            // ...the vendor updater for one under the home directory...
            assert!(
                script.contains(&format!(r#""$HOME"/*) {};;"#, package.home_update)),
                "{script}"
            );
            // ...and nothing for one installed anywhere else.
            assert!(script.contains("exit 5;; esac"), "{script}");
        }
        let codex = &update_command(harness_package("codex_cli").expect("codex"))[6];
        assert!(codex.contains("bun add -g @openai/codex@latest"));
        let opencode = &update_command(harness_package("opencode").expect("opencode"))[6];
        assert!(opencode.contains("opencode upgrade --method curl"));
    }

    #[test]
    fn the_writable_probe_is_a_fixed_bounded_command() {
        let package = harness_package("opencode").expect("package");
        let probe = install_writable_probe_command(package);
        assert_eq!(&probe[..5], ["timeout", "-k", "10", "10", "sh"]);
        assert_eq!(probe[5], "-c");
        // Nothing request-derived may reach the script, and a failing `npm`
        // must end the probe before the writability test can run.
        assert!(probe[6].starts_with(r#"prefix="$(npm config get prefix)" || exit 3;"#));
        assert!(probe[6].contains(r#"command -v opencode"#));
        // A home-directory install belongs to the sandbox user.
        assert!(probe[6].contains(r#""$HOME"/*) exit 0;;"#));
        assert!(probe[6].contains(r#"test -w "$prefix/lib/node_modules""#));
        assert_eq!(probe.len(), 7);
    }

    #[test]
    fn only_an_explicit_answer_picks_the_install_owner() {
        assert_eq!(install_owner_from_probe(0), Some(InstallOwner::SandboxUser));
        assert_eq!(install_owner_from_probe(10), Some(InstallOwner::Root));
        // `test -w`'s own failure status, an npm failure, a missing binary,
        // an install outside npm and the home directory, a `timeout` kill
        // and a missing shell are all "could not tell".
        for exit_code in [1, 2, 3, 4, 5, 124, 126, 127, 137, -1] {
            assert_eq!(install_owner_from_probe(exit_code), None, "{exit_code}");
        }
    }

    #[test]
    fn the_in_sandbox_limit_expires_before_the_client_side_one() {
        // Otherwise the client could give up (and release the workspace)
        // while the command is still running.
        let package = harness_package("codex_cli").expect("package");
        let limit = |command: Vec<String>| -> u64 { command[3].parse().expect("seconds") };
        assert!(limit(update_command(package)) + 10 < UPDATE_COMMAND_TIMEOUT.as_secs());
        assert!(limit(latest_version_command(package)) + 10 <= LATEST_COMMAND_TIMEOUT.as_secs());
        assert!(limit(version_command(package)) + 10 <= VERSION_COMMAND_TIMEOUT.as_secs());
    }

    #[test]
    fn parses_the_version_each_cli_prints() {
        assert_eq!(
            parse_version("2.1.263 (Claude Code)\n").as_deref(),
            Some("2.1.263")
        );
        assert_eq!(
            parse_version("codex-cli 0.154.0\n").as_deref(),
            Some("0.154.0")
        );
        assert_eq!(parse_version("0.162.1\n").as_deref(), Some("0.162.1"));
        assert_eq!(
            parse_version("npm notice\n1.2.3-beta.4\n").as_deref(),
            Some("1.2.3-beta.4")
        );
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn compares_versions_numerically_not_lexically() {
        assert!(is_newer("2.1.296", "2.1.263"));
        assert!(is_newer("0.162.1", "0.154.0"));
        assert!(is_newer("2.10.0", "2.9.9"));
        assert!(!is_newer("2.1.263", "2.1.263"));
        assert!(!is_newer("2.1.262", "2.1.263"));
        // A pre-release of the same number is not an update.
        assert!(!is_newer("2.1.263-beta.1", "2.1.263"));
        assert!(!is_newer("unknown", "2.1.263"));
        assert!(!is_newer("2.1.263", "unknown"));
    }
}
