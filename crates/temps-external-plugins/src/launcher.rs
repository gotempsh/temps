// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running plugins under an identity of their own.
//!
//! By default a plugin runs as the Temps user, so it can read everything Temps
//! can: the process environment, the encryption key, the Cloud link state.
//! An installation can instead name a *plugin launcher*: a program the
//! operator provides that starts each plugin as a different user. Temps then
//! never executes a plugin itself. It asks the launcher to, and before it
//! sends a plugin anything sensitive it checks that the plugin really is
//! another user.
//!
//! The launcher speaks three verbs, one per invocation:
//!
//! * `exec <sha256> <binary> -- <args...>` replaces itself with `binary` as the
//!   plugin user, after checking the binary still has the signed digest. It
//!   must `exec` in place, so the plugin stays Temps' direct child and its pid
//!   cannot be reused while Temps holds it.
//! * `signal KILL <pid>` stops a plugin. Temps can't signal another user's
//!   process itself.
//! * `probe <paths...>` reports, as the plugin user, which paths it can open:
//!   one `uid <n>` line, then `readable|denied|absent <path>` per path.
//!   Temps refuses to start plugins when any of them is readable.
//!
//! The launcher is configured with `--plugin-launcher` (`TEMPS_PLUGIN_LAUNCHER`),
//! a bootstrap value fixed by how the host or image is built.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

/// How long a `probe` or `signal` may take before it counts as failed.
const LAUNCHER_TIMEOUT: Duration = Duration::from_secs(10);

/// The operator's plugin launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLauncher {
    program: PathBuf,
}

/// What the plugin identity could open, as the launcher reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IsolationReport {
    /// The plugin identity's uid.
    pub uid: u32,
    /// Paths the plugin identity could open. Any entry fails the check.
    pub readable: Vec<String>,
    /// Paths that exist and the plugin identity could not open.
    pub denied: Vec<String>,
    /// Paths that don't exist here.
    pub absent: Vec<String>,
}

impl IsolationReport {
    /// One log line naming what was proven.
    pub fn summary(&self) -> String {
        format!(
            "plugins run as uid {} and cannot open {} of {} protected path(s): {}{}",
            self.uid,
            self.denied.len(),
            self.denied.len() + self.readable.len(),
            self.denied.join(", "),
            if self.absent.is_empty() {
                String::new()
            } else {
                format!(" (not present here: {})", self.absent.join(", "))
            }
        )
    }

    /// `Err` with an actionable message unless the plugin identity is
    /// another, unprivileged user that can open none of the probed paths.
    pub fn verify(&self, temps_uid: u32) -> Result<(), String> {
        if self.uid == 0 || self.uid == temps_uid {
            return Err(format!(
                "The plugin launcher runs plugins as uid {}, which is {}. Configure it to use a \
                 dedicated plugin account, or remove --plugin-launcher to run plugins as Temps.",
                self.uid,
                if self.uid == 0 {
                    "root"
                } else {
                    "the Temps user itself"
                }
            ));
        }
        if !self.readable.is_empty() {
            return Err(format!(
                "Plugins were not started: the plugin account (uid {}) can read {}. Make these \
                 readable only by the Temps user, then reload plugins.",
                self.uid,
                self.readable.join(", ")
            ));
        }
        if self.denied.is_empty() {
            return Err(
                "Plugins were not started: the plugin launcher's isolation probe checked no \
                 protected path. Update the launcher so its probe reports on the paths it is \
                 given."
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl PluginLauncher {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The command that starts `binary` as the plugin identity. The caller
    /// appends the plugin's own arguments.
    pub fn exec_command(&self, binary: &Path, sha256: &str) -> Command {
        let mut command = Command::new(&self.program);
        command.arg("exec").arg(sha256).arg(binary).arg("--");
        command
    }

    /// Stop plugin `pid`. Blocking, because it also runs from `Drop`.
    pub fn kill(&self, pid: u32) -> Result<(), String> {
        let mut child = std::process::Command::new(&self.program)
            .arg("signal")
            .arg("KILL")
            .arg(pid.to_string())
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| {
                format!(
                    "the plugin launcher {} could not be run to stop plugin pid {pid}: {error}",
                    self.program.display()
                )
            })?;
        let deadline = std::time::Instant::now() + LAUNCHER_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(format!(
                        "the plugin launcher could not stop plugin pid {pid} ({status})"
                    ))
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "the plugin launcher did not stop plugin pid {pid} within {}s",
                        LAUNCHER_TIMEOUT.as_secs()
                    ));
                }
                Err(error) => return Err(format!("waiting for the plugin launcher: {error}")),
            }
        }
    }

    /// Ask the launcher which of `paths` the plugin identity can open.
    pub async fn probe(&self, paths: &[PathBuf]) -> Result<IsolationReport, String> {
        let mut command = Command::new(&self.program);
        command
            .arg("probe")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for path in paths {
            let text = path.to_string_lossy();
            if text.contains('\n') {
                return Err(format!("cannot probe a path with a newline: {text:?}"));
            }
            command.arg(path);
        }
        let output = tokio::time::timeout(LAUNCHER_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                format!(
                    "the plugin launcher {} did not finish its isolation probe within {}s",
                    self.program.display(),
                    LAUNCHER_TIMEOUT.as_secs()
                )
            })?
            .map_err(|error| {
                format!(
                    "the plugin launcher {} could not be run: {error}",
                    self.program.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "the plugin launcher {} failed its isolation probe ({})",
                self.program.display(),
                output.status
            ));
        }
        parse_probe(&String::from_utf8_lossy(&output.stdout))
    }
}

fn parse_probe(output: &str) -> Result<IsolationReport, String> {
    let mut uid = None;
    let mut report = IsolationReport::default();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let (verb, rest) = line
            .split_once(' ')
            .ok_or_else(|| format!("unrecognised isolation probe line: {line:?}"))?;
        match verb {
            "uid" => {
                uid = Some(
                    rest.trim()
                        .parse::<u32>()
                        .map_err(|_| format!("unrecognised plugin uid: {rest:?}"))?,
                )
            }
            "readable" => report.readable.push(rest.to_string()),
            "denied" => report.denied.push(rest.to_string()),
            "absent" => report.absent.push(rest.to_string()),
            _ => return Err(format!("unrecognised isolation probe line: {line:?}")),
        }
    }
    report.uid = uid.ok_or("the isolation probe did not report the plugin uid")?;
    Ok(report)
}

/// The real, effective, saved and filesystem uids of `pid`, from
/// `/proc/<pid>/status`.
#[cfg(target_os = "linux")]
pub fn process_uids(pid: u32) -> Result<[u32; 4], String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| format!("reading /proc/{pid}/status: {error}"))?;
    parse_status_uids(&status).ok_or_else(|| format!("/proc/{pid}/status has no Uid line"))
}

#[cfg(not(target_os = "linux"))]
pub fn process_uids(_pid: u32) -> Result<[u32; 4], String> {
    Err("a plugin launcher is supported on Linux only".to_string())
}

// Linux-only at runtime; the unit test runs everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_status_uids(status: &str) -> Option<[u32; 4]> {
    let line = status.lines().find(|line| line.starts_with("Uid:"))?;
    let ids: Vec<u32> = line["Uid:".len()..]
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    ids.try_into().ok()
}

/// This process's effective uid: the Temps user.
pub fn temps_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// `Err` unless every uid of `pid` is someone other than root and Temps.
pub fn verify_plugin_process(pid: u32, temps_uid: u32) -> Result<(), String> {
    let uids = process_uids(pid)?;
    if uids.iter().any(|uid| *uid == 0 || *uid == temps_uid) {
        return Err(format!(
            "the plugin launcher started the plugin with uids {uids:?}, which include root or \
             the Temps user (uid {temps_uid}); the plugin was stopped before it received \
             anything. Configure the launcher to use a dedicated plugin account."
        ));
    }
    Ok(())
}

/// What Temps does to its own files so the plugin identity, which shares
/// Temps' group, can use what a plugin needs and nothing more.
#[cfg(unix)]
pub mod access {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::Path;

    fn owned_by_us(metadata: &std::fs::Metadata) -> bool {
        // SAFETY: geteuid has no preconditions.
        metadata.uid() == unsafe { libc::geteuid() }
    }

    fn add_mode(path: &Path, bits: u32) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !owned_by_us(&metadata) {
            return Ok(());
        }
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & bits != bits {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | bits))?;
        }
        Ok(())
    }

    /// Group search (`g+x`) on every directory from `root` down to `dir`,
    /// both included, so the plugin identity can reach `dir` by name. It
    /// grants no listing.
    pub fn grant_traverse(root: &Path, dir: &Path) -> std::io::Result<()> {
        let relative = dir.strip_prefix(root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not beneath {}", dir.display(), root.display()),
            )
        })?;
        let mut current = root.to_path_buf();
        add_mode(&current, 0o010)?;
        for component in relative.components() {
            current.push(component);
            add_mode(&current, 0o010)?;
        }
        Ok(())
    }

    /// A verified plugin binary, executable by the group: `0550`.
    pub fn grant_exec(binary: &Path) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(binary)?;
        if !metadata.file_type().is_file() || !owned_by_us(&metadata) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} must be a regular file owned by the Temps user",
                    binary.display()
                ),
            ));
        }
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o550))
    }

    /// A directory the plugin owns in practice: `dir` becomes group
    /// read-write, and so does everything Temps itself created beneath it
    /// (the plugin's files from before it had an identity of its own).
    /// Symbolic links are never followed.
    pub fn share_dir(dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        share_tree(dir, 0)
    }

    const MAX_DEPTH: usize = 64;

    fn share_tree(path: &Path, depth: usize) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.is_dir() {
            add_mode(path, 0o070)?;
            if depth >= MAX_DEPTH {
                return Ok(());
            }
            for entry in std::fs::read_dir(path)? {
                share_tree(&entry?.path(), depth + 1)?;
            }
        } else if metadata.is_file() {
            let user_exec = metadata.permissions().mode() & 0o100;
            add_mode(path, 0o060 | (user_exec >> 3))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_is_parsed_and_summarised() {
        let report = parse_probe(
            "uid 1003\ndenied /proc/7/environ\ndenied /proc/7/mem\nabsent /data/auth_secret\n",
        )
        .unwrap();
        assert_eq!(report.uid, 1003);
        assert_eq!(report.denied, ["/proc/7/environ", "/proc/7/mem"]);
        assert_eq!(report.absent, ["/data/auth_secret"]);
        report.verify(1000).expect("isolated");
        assert!(report.summary().contains("uid 1003"));
        assert!(report.summary().contains("cannot open 2 of 2"));
    }

    #[test]
    fn a_readable_secret_fails_the_check_and_names_it() {
        let report =
            parse_probe("uid 1003\ndenied /proc/7/environ\nreadable /run/temps-secrets/key\n")
                .unwrap();
        let error = report.verify(1000).unwrap_err();
        assert!(error.contains("/run/temps-secrets/key"), "{error}");
        assert!(error.contains("not started"), "{error}");
    }

    #[test]
    fn root_or_the_temps_user_is_not_a_plugin_identity() {
        for uid in [0, 1000] {
            let report = IsolationReport {
                uid,
                denied: vec!["/x".into()],
                ..Default::default()
            };
            assert!(report.verify(1000).is_err());
        }
    }

    #[test]
    fn a_probe_that_checked_nothing_proves_nothing() {
        let report = parse_probe("uid 1003\nabsent /x\n").unwrap();
        assert!(report.verify(1000).is_err());
    }

    #[test]
    fn a_probe_without_a_uid_or_with_noise_is_refused() {
        assert!(parse_probe("denied /x\n").is_err());
        assert!(parse_probe("uid 1003\nmaybe /x\n").is_err());
        assert!(parse_probe("uid abc\n").is_err());
    }

    #[test]
    fn status_uids_are_read_from_the_uid_line() {
        let status = "Name:\tplugin\nUid:\t1003\t1003\t1003\t1003\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(parse_status_uids(status), Some([1003; 4]));
        assert_eq!(parse_status_uids("Name:\tx\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn sharing_a_directory_grants_the_group_without_following_links() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugin-data").join("demo");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = dir.join("nested").join("state.db");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"secret").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();

        access::share_dir(&dir).unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o770);
        assert_eq!(mode(&file), 0o660);
        assert_eq!(mode(&outside), 0o600, "a link is never followed");
    }

    #[cfg(unix)]
    #[test]
    fn traversal_is_granted_from_the_root_down_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("plugins").join("demo").join("1.0.0");
        std::fs::create_dir_all(&leaf).unwrap();
        for dir in [
            root.path().join("plugins"),
            root.path().join("plugins/demo"),
            leaf.clone(),
        ] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        access::grant_traverse(&root.path().join("plugins"), &leaf).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root.path().join("plugins")), 0o710);
        assert_eq!(mode(&leaf), 0o710);
        assert!(access::grant_traverse(&leaf, root.path()).is_err());
    }
}
