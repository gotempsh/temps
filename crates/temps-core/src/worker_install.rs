// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The commands a new worker runs to install Temps, for the commands the
//! server generates (the pairing command).
//!
//! The console's Add worker panel builds the same commands in
//! `web/src/lib/worker-join-script.ts`. Both are checked against one golden
//! file, `web/src/lib/worker-install.golden.json`, so they cannot drift: a
//! change to either renderer fails its own tests until the golden file, and
//! with it the other renderer, says the same.
//!
//! A Linux worker installs Temps as root and calls it by its absolute path:
//! `temps join` sets up WireGuard and the overlay, and `temps agent service
//! install` writes a systemd unit, so both need root; the agent service
//! refuses a binary root does not own; and `sudo` resets PATH, so a bare
//! `temps` is not found on the next line. `sudo -H` puts the install and the
//! agent's data in `/root/.temps` whatever sudo's HOME policy is.

/// The installer every command downloads.
pub const INSTALLER_URL: &str = "https://temps.sh/install.sh";
/// Where the installer puts Temps when root runs it with `sudo -H`.
pub const ROOT_TEMPS_BIN: &str = "/root/.temps/bin/temps";
/// Where the installer puts Temps for the user running it (a Mac).
pub const USER_TEMPS_BIN: &str = "~/.temps/bin/temps";
const AS_ROOT: &str = "sudo -H";

/// The version this control plane runs, as `temps serve` was built with it
/// (`TEMPS_VERSION`, e.g. `v0.1.0-beta.60 (abc1234) built ...`).
///
/// Registered as a service by `temps serve`. A host that does not register
/// it gets an unpinned install (the newest stable release), which is what
/// the server's commands did before they were pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneBuild {
    version: String,
}

impl PlaneBuild {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
        }
    }

    /// The running version tag: [`version_tag`] of the build string. This is
    /// the `current_version` the self-update capability reports, which the
    /// console pins its worker commands to.
    pub fn version_tag(&self) -> String {
        version_tag(&self.version)
    }

    /// The release a new worker installs to match this control plane, or
    /// `None` when the running version is not one the installer can
    /// download.
    pub fn release_tag(&self) -> Option<String> {
        release_tag(&self.version_tag()).map(str::to_string)
    }
}

/// The clean version tag in a `TEMPS_VERSION` build string: everything
/// before the first space, without a `-<commit>` suffix (7 to 12 hex
/// characters) that a build between tags carries.
///
/// `v1.0.0 (abc1234) built 2025-01-25 12:34:56 UTC` and
/// `v1.0.0-abc1234 built 2025-01-25 12:34:56 UTC` both give `v1.0.0`.
pub fn version_tag(full_version: &str) -> String {
    let version = full_version
        .split_whitespace()
        .next()
        .unwrap_or(full_version);
    if let Some((tag, suffix)) = version.rsplit_once('-') {
        if (7..=12).contains(&suffix.len()) && suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            return tag.to_string();
        }
    }
    version.to_string()
}

/// `version` trimmed, if it is a release tag the installer can download:
/// `v<major>.<minor>.<patch>` with an optional `-<prerelease>` of letters,
/// digits, dots and hyphens that starts with a letter or digit. A subset of
/// the installer's own plain-tag check, so the value is also safe to paste
/// into a shell. Anything else (a development build, an unreadable version)
/// is not pinned. Mirrors `releaseTag` in `web/src/lib/worker-join-script.ts`.
pub fn release_tag(version: &str) -> Option<&str> {
    let tag = version.trim();
    let rest = tag.strip_prefix('v')?;
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    let mut parts = core.split('.');
    let numeric = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    if !numeric || parts.next().is_some() {
        return None;
    }
    if let Some(pre) = pre {
        let mut bytes = pre.bytes();
        let starts_ok = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric());
        if !starts_ok || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
            return None;
        }
    }
    Some(tag)
}

/// The installer's arguments: ` -s -- <tag>` to install exactly that
/// release (the installer then ignores its channel), nothing for the newest
/// stable release.
fn installer_args(tag: Option<&str>) -> String {
    tag.map(|tag| format!(" -s -- {tag}")).unwrap_or_default()
}

/// Install Temps as root on a Linux worker. Without `pipefail` a failed
/// download feeds bash nothing, which exits 0, and the next command would
/// run whatever binary was there before.
pub fn linux_install_command(tag: Option<&str>) -> String {
    format!(
        "(set -o pipefail; curl -fsSL {INSTALLER_URL} | {AS_ROOT} bash{})",
        installer_args(tag)
    )
}

/// Install Temps for the current user, without sudo (a Mac, ADR 090).
pub fn mac_install_command(tag: Option<&str>) -> String {
    format!(
        "(set -o pipefail; curl -fsSL {INSTALLER_URL} | bash{})",
        installer_args(tag)
    )
}

/// Run a `temps` subcommand as root, from the root install.
pub fn as_root_temps(args: &str) -> String {
    format!("{AS_ROOT} {ROOT_TEMPS_BIN} {args}")
}

/// The command a Linux worker runs for a pairing (ADR 048 D2b): install
/// Temps pinned to `tag`, join with the pairing code, start the agent
/// service. One command over three lines, chained with `&&` so a failed
/// step stops the rest and read whole by the shell, so a sudo password
/// prompt cannot swallow the lines after it.
///
/// The code is a secret for the pairing's lifetime, so it never goes in a
/// process's argv or sudo's log: the `printf` builtin pipes it to `temps
/// join --pair -` (no process is started with it; `printf` rather than a
/// `<<<` here-string, which dash lacks), and `sudo` runs only `temps join`.
/// The leading space keeps the whole command out of the shell history where
/// the shell honours it (bash with `HISTCONTROL=ignorespace`/`ignoreboth`,
/// the Debian/Ubuntu default, zsh with `HIST_IGNORE_SPACE`).
///
/// Pairing is for a worker this server dials. A Mac worker joins outbound
/// only (its end of the mesh runs in its Docker app's VM, which nothing can
/// dial), so there is no Mac pairing command.
pub fn linux_pairing_command(code: &str, tag: Option<&str>) -> String {
    // A code is `tpair1.` + base64url and never holds a quote; escape one
    // anyway so the command can never break out of its quoting.
    let quoted = code.replace('\'', r"'\''");
    format!(
        " {} && \\\nprintf '%s\\n' '{quoted}' | {} && \\\n{}",
        linux_install_command(tag),
        as_root_temps("join --pair -"),
        as_root_temps("agent service install"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden file the console's renderer is tested against too.
    const GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/src/lib/worker-install.golden.json"
    ));

    #[derive(serde::Deserialize)]
    struct Golden {
        installer_url: String,
        root_temps_bin: String,
        user_temps_bin: String,
        installs: Vec<InstallCase>,
        as_root_temps: Vec<AsRootCase>,
        release_tags: ReleaseTags,
    }

    #[derive(serde::Deserialize)]
    struct InstallCase {
        tag: Option<String>,
        linux: String,
        mac: String,
    }

    #[derive(serde::Deserialize)]
    struct AsRootCase {
        args: String,
        command: String,
    }

    #[derive(serde::Deserialize)]
    struct ReleaseTags {
        pinned: Vec<(String, String)>,
        not_pinned: Vec<String>,
    }

    fn golden() -> Golden {
        serde_json::from_str(GOLDEN).expect("worker-install.golden.json parses")
    }

    #[test]
    fn the_install_commands_match_the_consoles() {
        let golden = golden();
        assert_eq!(golden.installer_url, INSTALLER_URL);
        assert_eq!(golden.root_temps_bin, ROOT_TEMPS_BIN);
        assert_eq!(golden.user_temps_bin, USER_TEMPS_BIN);
        assert!(!golden.installs.is_empty());
        for case in &golden.installs {
            assert_eq!(linux_install_command(case.tag.as_deref()), case.linux);
            assert_eq!(mac_install_command(case.tag.as_deref()), case.mac);
        }
        for case in &golden.as_root_temps {
            assert_eq!(as_root_temps(&case.args), case.command);
        }
    }

    #[test]
    fn release_tags_are_pinned_exactly_as_the_console_pins_them() {
        let golden = golden();
        for (version, tag) in &golden.release_tags.pinned {
            assert_eq!(release_tag(version), Some(tag.as_str()), "{version:?}");
        }
        for version in &golden.release_tags.not_pinned {
            assert_eq!(release_tag(version), None, "{version:?}");
        }
    }

    #[test]
    fn the_version_tag_drops_the_build_details_and_a_commit_suffix() {
        assert_eq!(
            version_tag("v0.1.0-beta.60 (abc1234) built 2026-11-06 08:00:00 UTC"),
            "v0.1.0-beta.60"
        );
        assert_eq!(
            version_tag("v0.1.0-beta.60-6af2f4949 built 2026-11-06 08:00:00 UTC"),
            "v0.1.0-beta.60"
        );
        assert_eq!(
            version_tag("v0.1.0-nightly.20261106.6af2f4949 (6af2f4949) built x"),
            "v0.1.0-nightly.20261106.6af2f4949"
        );
        assert_eq!(version_tag("v1.2.3"), "v1.2.3");
        assert_eq!(version_tag(""), "");
    }

    #[test]
    fn a_release_build_pins_its_tag_and_anything_else_is_unpinned() {
        let release = PlaneBuild::new("v0.1.0-beta.60 (abc1234) built 2026-11-06 08:00:00 UTC");
        assert_eq!(release.version_tag(), "v0.1.0-beta.60");
        assert_eq!(release.release_tag().as_deref(), Some("v0.1.0-beta.60"));
        assert_eq!(PlaneBuild::new("dev").release_tag(), None);
        assert_eq!(PlaneBuild::new("").release_tag(), None);
    }

    #[test]
    fn the_pairing_command_installs_pinned_joins_as_root_and_starts_the_agent() {
        assert_eq!(
            linux_pairing_command("tpair1.abc-_9", Some("v0.1.0-beta.60")),
            " (set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- v0.1.0-beta.60) && \\\n\
             printf '%s\\n' 'tpair1.abc-_9' | sudo -H /root/.temps/bin/temps join --pair - && \\\n\
             sudo -H /root/.temps/bin/temps agent service install"
        );
        let unpinned = linux_pairing_command("tpair1.abc", None);
        assert!(
            unpinned.starts_with(
                " (set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash) && "
            ),
            "{unpinned}"
        );
    }

    #[test]
    fn the_pairing_code_stays_out_of_argv_sudo_and_history() {
        let command = linux_pairing_command("tpair1.secret", Some("v1.2.3"));
        // Out of the history: the whole command starts with a space.
        assert!(command.starts_with(' '), "{command}");
        // Only `printf` (a builtin) sees the code; sudo never does.
        let join_line = command
            .lines()
            .find(|line| line.contains("--pair"))
            .expect("a join line");
        let (feeds, joins) = join_line.split_once('|').expect("a pipe");
        assert!(feeds.trim_start().starts_with("printf "), "{join_line}");
        assert!(feeds.contains("tpair1.secret"), "{join_line}");
        assert!(!joins.contains("tpair1"), "{join_line}");
        assert!(!command.contains("sudo -H printf"), "{command}");
        assert_eq!(command.matches("tpair1.secret").count(), 1, "{command}");
    }

    #[test]
    fn a_quote_in_a_code_cannot_break_out_of_the_command() {
        let command = linux_pairing_command("a'b", None);
        assert!(command.contains(r"printf '%s\n' 'a'\''b' | "), "{command}");
    }
}
