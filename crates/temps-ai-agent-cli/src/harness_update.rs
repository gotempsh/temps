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
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Upper bounds for the commands run inside the workspace.
pub(crate) const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const LATEST_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const UPDATE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How a provider's CLI is installed in the managed workspace image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HarnessPackage {
    pub binary: &'static str,
    pub npm_package: &'static str,
}

/// Packages are fixed constants: nothing from a request reaches a command.
pub(crate) fn harness_package(provider_id: &str) -> Option<HarnessPackage> {
    match provider_id {
        "claude_cli" => Some(HarnessPackage {
            binary: "claude",
            npm_package: "@anthropic-ai/claude-code",
        }),
        "codex_cli" => Some(HarnessPackage {
            binary: "codex",
            npm_package: "@openai/codex",
        }),
        _ => None,
    }
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

/// Whether the sandbox user can replace a global npm package itself.
///
/// Current runtime images install the harness CLIs into a prefix the sandbox
/// user owns, so the update must run as that user: root running npm inside a
/// tree the sandbox controls would follow sandbox-planted links and leave
/// root-owned files that break the CLI's own self-update. Older images
/// installed them as root, where only a root update can succeed. The probe
/// runs as the sandbox user and exits 0 only when it can write there.
pub(crate) fn install_writable_probe_command() -> Vec<String> {
    bounded(
        VERSION_COMMAND_TIMEOUT.as_secs().saturating_sub(10).max(1),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"test -w "$(npm config get prefix)/lib/node_modules""#.to_string(),
        ],
    )
}

pub(crate) fn update_command(package: HarnessPackage) -> Vec<String> {
    bounded(
        UPDATE_COMMAND_TIMEOUT.as_secs().saturating_sub(30),
        vec![
            "npm".to_string(),
            "install".to_string(),
            "-g".to_string(),
            format!("{}@latest", package.npm_package),
            "--no-fund".to_string(),
            "--no-audit".to_string(),
            "--loglevel=error".to_string(),
        ],
    )
}

/// First `major.minor.patch` (optionally with a pre-release suffix) in `text`.
pub(crate) fn parse_version(text: &str) -> Option<String> {
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
pub(crate) fn is_newer(latest: &str, installed: &str) -> bool {
    match (numeric_core(latest), numeric_core(installed)) {
        (Some(latest), Some(installed)) => latest > installed,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_how_both_harnesses_are_installed() {
        assert_eq!(
            harness_package("claude_cli").map(|p| p.npm_package),
            Some("@anthropic-ai/claude-code")
        );
        assert_eq!(
            harness_package("codex_cli").map(|p| p.binary),
            Some("codex")
        );
        assert!(harness_package("opencode").is_none());
        assert!(harness_package("claude_cli; rm -rf /").is_none());
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
        assert_eq!(&update[..5], ["timeout", "-k", "10", "270", "npm"]);
        assert_eq!(
            &update[5..8],
            ["install", "-g", "@anthropic-ai/claude-code@latest"]
        );
    }

    #[test]
    fn the_writable_probe_is_a_fixed_bounded_command() {
        let probe = install_writable_probe_command();
        assert_eq!(&probe[..5], ["timeout", "-k", "10", "10", "sh"]);
        assert_eq!(probe[5], "-c");
        // Nothing request-derived may reach the script.
        assert_eq!(
            probe[6],
            r#"test -w "$(npm config get prefix)/lib/node_modules""#
        );
        assert_eq!(probe.len(), 7);
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
