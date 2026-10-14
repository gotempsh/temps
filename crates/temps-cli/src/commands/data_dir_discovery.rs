// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which data directory a setup command should act on.
//!
//! `temps microsandbox setup` and `temps firecracker setup` install state
//! under `<data dir>/…`, and the server only ever reads *its own* data
//! directory. Run from a shell, those commands do not inherit the server's
//! `TEMPS_DATA_DIR`, so a bare invocation quietly provisions a directory the
//! server never looks at. This module finds the directory the server really
//! uses and, when it cannot, says plainly that the chosen one holds no Temps
//! data.

use std::path::{Path, PathBuf};

/// Where the resolved directory came from, for the setup banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DataDirSource {
    /// `--data-dir` or `TEMPS_DATA_DIR` in this shell.
    Explicit,
    /// Read from a running `temps serve` process.
    RunningServer { pid: u32 },
    /// The `~/.temps` / `~/.temps/data` layout `temps doctor` recognises.
    InstallLayout,
}

impl DataDirSource {
    pub(crate) fn describe(&self) -> String {
        match self {
            DataDirSource::Explicit => "from --data-dir / TEMPS_DATA_DIR".to_string(),
            DataDirSource::RunningServer { pid } => {
                format!("detected from the running `temps serve` (pid {pid})")
            }
            DataDirSource::InstallLayout => "detected from the install layout".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDataDir {
    pub dir: PathBuf,
    pub source: DataDirSource,
    /// Problems the operator should see before anything is installed.
    pub warnings: Vec<String>,
}

/// A directory holds Temps state once the server has generated its keys.
/// Same test `temps doctor` uses to recognise an installation.
pub(crate) fn looks_like_data_dir(dir: &Path) -> bool {
    dir.join("auth_secret").is_file() || dir.join("encryption_key").is_file()
}

/// Resolve the data directory for a setup command.
///
/// Order: explicit flag/env, then a running `temps serve` (the only source
/// that knows the answer for sure), then the install layout `temps doctor`
/// detects.
pub(crate) fn resolve(explicit: Option<&Path>, home: Option<&Path>) -> ResolvedDataDir {
    resolve_with(explicit, home, running_server_data_dir())
}

fn resolve_with(
    explicit: Option<&Path>,
    home: Option<&Path>,
    server: Option<(u32, PathBuf)>,
) -> ResolvedDataDir {
    let mut warnings = Vec::new();

    let (dir, source) = match (explicit, server.as_ref()) {
        (Some(dir), server) => {
            if let Some((pid, server_dir)) = server {
                if !same_path(dir, server_dir) {
                    warnings.push(format!(
                        "the running `temps serve` (pid {pid}) uses {}, not {}; it will not \
                         see anything installed there. Use `--data-dir {}` to set up the \
                         directory that server reads",
                        server_dir.display(),
                        dir.display(),
                        server_dir.display()
                    ));
                }
            }
            (dir.to_path_buf(), DataDirSource::Explicit)
        }
        (None, Some((pid, server_dir))) => (
            server_dir.clone(),
            DataDirSource::RunningServer { pid: *pid },
        ),
        (None, None) => match home {
            Some(home) => (
                super::doctor::default_data_dir(home),
                DataDirSource::InstallLayout,
            ),
            None => (PathBuf::from(".temps"), DataDirSource::InstallLayout),
        },
    };

    if !looks_like_data_dir(&dir) {
        warnings.push(format!(
            "{} contains no Temps data (no encryption_key or auth_secret), so it is probably \
             not the directory your server uses. If `temps serve` runs with a different \
             TEMPS_DATA_DIR, what is installed here will not be seen by it: re-run with \
             `--data-dir <that directory>`. (Expected only on a machine that has not started \
             Temps yet.)",
            dir.display()
        ));
    }

    ResolvedDataDir {
        dir,
        source,
        warnings,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}

/// The data directory of the `temps serve` running on this host, if exactly
/// one can be determined. Linux only (reads `/proc`); a process owned by
/// another user has an unreadable environment and is simply not matched.
#[cfg(target_os = "linux")]
fn running_server_data_dir() -> Option<(u32, PathBuf)> {
    let me = std::process::id();
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
        if let Some(dir) = server_data_dir_from_process(&cmdline, &environ) {
            found.push((pid, dir));
        }
    }
    found.sort_by_key(|(pid, _)| *pid);
    let (pid, dir) = found.first()?.clone();
    // Several servers on different directories: do not guess.
    found
        .iter()
        .all(|(_, other)| other == &dir)
        .then_some((pid, dir))
}

#[cfg(not(target_os = "linux"))]
fn running_server_data_dir() -> Option<(u32, PathBuf)> {
    None
}

/// The data directory a `temps serve` process runs with, from its raw
/// `/proc/<pid>/cmdline` and `/proc/<pid>/environ` (both NUL-separated).
/// `None` when the process is not `temps serve`, or the directory cannot be
/// determined.
///
/// Mirrors the server's own resolution: `--data-dir` wins, then
/// `TEMPS_DATA_DIR`, then `$HOME/.temps`.
#[cfg(any(target_os = "linux", test))]
fn server_data_dir_from_process(cmdline: &[u8], environ: &[u8]) -> Option<PathBuf> {
    let args: Vec<&str> = cmdline
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok())
        .collect();
    let program = Path::new(args.first()?).file_name()?.to_str()?;
    if program != "temps" || !args[1..].contains(&"serve") {
        return None;
    }

    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(value));
        }
        if *arg == "--data-dir" {
            return iter.next().map(PathBuf::from);
        }
    }

    let env = |key: &str| {
        environ
            .split(|b| *b == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .find_map(|entry| entry.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    env("TEMPS_DATA_DIR").or_else(|| env("HOME").map(|home| home.join(".temps")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(parts: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn server_dir_comes_from_the_flag_before_the_environment() {
        let cmdline = nul(&["/usr/local/bin/temps", "serve", "--data-dir", "/srv/temps"]);
        let environ = nul(&["TEMPS_DATA_DIR=/ignored", "HOME=/home/u"]);
        assert_eq!(
            server_data_dir_from_process(&cmdline, &environ),
            Some(PathBuf::from("/srv/temps"))
        );

        let eq_form = nul(&["temps", "serve", "--data-dir=/srv/other"]);
        assert_eq!(
            server_data_dir_from_process(&eq_form, &[]),
            Some(PathBuf::from("/srv/other"))
        );
    }

    #[test]
    fn server_dir_falls_back_to_the_environment_then_home() {
        let cmdline = nul(&["temps", "serve", "--address", "0.0.0.0:80"]);
        assert_eq!(
            server_data_dir_from_process(
                &cmdline,
                &nul(&["HOME=/home/u", "TEMPS_DATA_DIR=/home/u/.temps/data"])
            ),
            Some(PathBuf::from("/home/u/.temps/data"))
        );
        assert_eq!(
            server_data_dir_from_process(&cmdline, &nul(&["HOME=/home/u"])),
            Some(PathBuf::from("/home/u/.temps"))
        );
        // Unreadable environment (another user's process) and no flag:
        // undeterminable, so not guessed.
        assert_eq!(server_data_dir_from_process(&cmdline, &[]), None);
    }

    #[test]
    fn only_temps_serve_processes_match() {
        let environ = nul(&["TEMPS_DATA_DIR=/d"]);
        for cmdline in [
            nul(&["temps", "microsandbox", "setup"]),
            nul(&["temps", "proxy"]),
            nul(&["bash", "-c", "temps serve"]),
            nul(&["/usr/bin/other-temps", "serve"]),
        ] {
            assert_eq!(server_data_dir_from_process(&cmdline, &environ), None);
        }
    }

    #[test]
    fn running_server_wins_over_layout_and_explicit_mismatch_is_flagged() {
        let home = tempfile::tempdir().unwrap();
        let server_dir = home.path().join(".temps").join("data");
        std::fs::create_dir_all(&server_dir).unwrap();
        std::fs::write(server_dir.join("encryption_key"), b"k").unwrap();

        let resolved = resolve_with(None, Some(home.path()), Some((4242, server_dir.clone())));
        assert_eq!(resolved.dir, server_dir);
        assert_eq!(resolved.source, DataDirSource::RunningServer { pid: 4242 });
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

        let wrong = home.path().join(".temps");
        let resolved = resolve_with(
            Some(&wrong),
            Some(home.path()),
            Some((4242, server_dir.clone())),
        );
        assert_eq!(resolved.source, DataDirSource::Explicit);
        assert!(
            resolved
                .warnings
                .iter()
                .any(|w| w.contains("pid 4242") && w.contains(&*server_dir.to_string_lossy())),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn a_directory_without_temps_data_is_called_out() {
        let home = tempfile::tempdir().unwrap();
        let resolved = resolve_with(None, Some(home.path()), None);
        assert_eq!(resolved.source, DataDirSource::InstallLayout);
        assert_eq!(resolved.warnings.len(), 1);
        let warning = &resolved.warnings[0];
        assert!(warning.contains("contains no Temps data"), "{warning}");
        assert!(warning.contains("--data-dir"), "{warning}");

        // A real data dir is quiet.
        let root = home.path().join(".temps");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("auth_secret"), b"s").unwrap();
        assert!(resolve_with(None, Some(home.path()), None)
            .warnings
            .is_empty());
    }
}
