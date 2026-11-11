// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Finding the PostgreSQL client tools (`pg_dump`, `pg_dumpall`) a backup can
//! run, on the Temps host or through Docker.
//!
//! Shared by the control-plane engine ([`super::control_plane`]) and the
//! pre-migration backup ([`crate::pre_migration`]). Both dump the Temps
//! database itself, and both need a client whose major version is at least
//! the server's: `pg_dump` refuses to dump a newer server.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

/// Bound on asking a client binary for its `--version`.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A PostgreSQL client tool found on the host, with its major version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTool {
    pub binary: PathBuf,
    pub major: u32,
}

/// What looking for one client tool on the host found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostToolProbe {
    Found(HostTool),
    /// No executable of that name on `PATH`.
    Missing,
    /// On `PATH`, but `--version` failed or printed no PostgreSQL version.
    Unusable {
        binary: PathBuf,
        reason: String,
    },
}

/// `pg_dump (PostgreSQL) 18.6 (Ubuntu 18.6-1)` -> `18`. Works for every
/// client tool (`pg_dumpall (PostgreSQL) 18.1` -> `18`).
pub fn parse_pg_tool_major(version_output: &str) -> Option<u32> {
    version_output
        .split_whitespace()
        .find_map(|token| {
            let major = token.split('.').next()?;
            if token.contains('.') || token.chars().all(|c| c.is_ascii_digit()) {
                major.parse::<u32>().ok()
            } else {
                None
            }
        })
        .filter(|major| *major >= 9)
}

/// The first regular file named `program` in the directories of a `PATH`
/// value.
pub fn find_in_path(program: &str, path_value: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_value)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// [`find_in_path`] against this process's `PATH`.
#[allow(
    clippy::disallowed_methods,
    reason = "host lookup: PATH is searched for a PostgreSQL client binary, not configuration"
)]
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_path(program, &path)
}

/// Look `program` up in a `PATH` value and ask it for its version.
pub async fn probe_host_tool_in(program: &str, path_value: &OsStr) -> HostToolProbe {
    match find_in_path(program, path_value) {
        Some(binary) => probe_binary(binary).await,
        None => HostToolProbe::Missing,
    }
}

/// [`probe_host_tool_in`] against this process's `PATH`.
pub async fn probe_host_tool(program: &str) -> HostToolProbe {
    match find_on_path(program) {
        Some(binary) => probe_binary(binary).await,
        None => HostToolProbe::Missing,
    }
}

async fn probe_binary(binary: PathBuf) -> HostToolProbe {
    let output = tokio::time::timeout(
        VERSION_PROBE_TIMEOUT,
        tokio::process::Command::new(&binary)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return HostToolProbe::Unusable {
                binary,
                reason: format!("running it with --version failed: {error}"),
            }
        }
        Err(_) => {
            return HostToolProbe::Unusable {
                binary,
                reason: format!(
                    "--version did not answer within {}s",
                    VERSION_PROBE_TIMEOUT.as_secs()
                ),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_pg_tool_major(&stdout) {
        Some(major) if output.status.success() => HostToolProbe::Found(HostTool { binary, major }),
        _ => HostToolProbe::Unusable {
            binary,
            reason: format!(
                "--version exited with {} and printed no PostgreSQL version: {:?}",
                output.status,
                stdout.trim()
            ),
        },
    }
}

/// `Ok(())` when a Docker daemon answers a ping within `timeout`, otherwise
/// why not.
pub async fn probe_docker(timeout: Duration) -> Result<(), String> {
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| format!("cannot connect to the Docker daemon: {error}"))?;
    match tokio::time::timeout(timeout, docker.ping()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("the Docker daemon did not answer a ping: {error}")),
        Err(_) => Err(format!(
            "the Docker daemon did not answer a ping within {}s",
            timeout.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_tool_versions_are_parsed() {
        assert_eq!(
            parse_pg_tool_major("pg_dump (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg22.04+2)\n"),
            Some(18)
        );
        assert_eq!(
            parse_pg_tool_major("pg_dumpall (PostgreSQL) 18.1"),
            Some(18)
        );
        assert_eq!(parse_pg_tool_major("pg_dump (PostgreSQL) 16.4"), Some(16));
        assert_eq!(parse_pg_tool_major("pg_dump (PostgreSQL) 17"), Some(17));
        assert_eq!(parse_pg_tool_major("command not found"), None);
        assert_eq!(parse_pg_tool_major(""), None);
    }

    #[test]
    fn path_lookup_takes_the_first_match_and_skips_directories() {
        let first = tempfile::tempdir().expect("first dir");
        let second = tempfile::tempdir().expect("second dir");
        // A directory named like the tool is not the tool.
        std::fs::create_dir(first.path().join("pg_dump")).expect("decoy dir");
        std::fs::write(second.path().join("pg_dump"), b"").expect("tool");
        let path = std::env::join_paths([first.path(), second.path()]).expect("PATH value");

        assert_eq!(
            find_in_path("pg_dump", &path),
            Some(second.path().join("pg_dump"))
        );
        assert_eq!(find_in_path("pg_dumpall", &path), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probing_reports_found_missing_and_unusable_tools() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("bin dir");
        let write_script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        };
        let pg_dump = write_script("pg_dump", "echo 'pg_dump (PostgreSQL) 18.1'");
        let broken = write_script("pg_dumpall", "echo 'segfault' >&2; exit 1");
        let path = dir.path().as_os_str();

        assert_eq!(
            probe_host_tool_in("pg_dump", path).await,
            HostToolProbe::Found(HostTool {
                binary: pg_dump,
                major: 18
            })
        );
        match probe_host_tool_in("pg_dumpall", path).await {
            HostToolProbe::Unusable { binary, reason } => {
                assert_eq!(binary, broken);
                assert!(reason.contains("printed no PostgreSQL version"), "{reason}");
            }
            other => panic!("expected an unusable tool, got {other:?}"),
        }
        assert_eq!(
            probe_host_tool_in("psql", path).await,
            HostToolProbe::Missing
        );
    }
}
