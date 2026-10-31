// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Detecting a usable Docker Compose v2 on the host.

use serde::{Deserialize, Serialize};
use std::process::Stdio;

use super::diagnostics::sanitize_config_stderr;
use super::exec::{isolated_docker_command, CapturedEnd};
use super::{ComposeError, ComposeExecutor, COMPOSE_VERSION_TIMEOUT};

/// The oldest Docker Compose release that accepts every option Temps passes
/// to `docker compose config` (`--no-env-resolution` arrived in v2.35.0).
pub const MIN_COMPOSE_VERSION: &str = "2.35.0";

/// Options Temps passes to `docker compose config` during resolution.
pub const COMPOSE_CONFIG_FLAGS: &[&str] = &[
    "--no-normalize",
    "--no-path-resolution",
    "--no-env-resolution",
    "--no-interpolate",
];

/// Options for the second, validation-only `docker compose config` pass:
/// `--no-interpolate` skips Compose's model consistency checks, so this pass
/// leaves interpolation on (there is nothing left to substitute) and discards
/// the output.
pub const COMPOSE_CONSISTENCY_FLAGS: &[&str] = &[
    "--no-normalize",
    "--no-path-resolution",
    "--no-env-resolution",
    "--quiet",
];

/// What the host's Docker Compose CLI supports, probed once at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeSupport {
    /// Whether `docker compose version` ran successfully.
    pub available: bool,
    /// Version reported by `docker compose version --short`.
    pub version: Option<String>,
    /// `docker compose config` options Temps needs that this CLI lacks.
    pub missing_flags: Vec<String>,
    /// Operator-facing explanation when something is missing.
    pub problem: Option<String>,
}

impl ComposeSupport {
    pub fn supported(&self) -> bool {
        self.available && self.missing_flags.is_empty()
    }
}

impl ComposeExecutor {
    /// Probe the host's Compose CLI once (at startup): its version and
    /// whether `docker compose config` knows every option Temps passes.
    /// Never fails; problems are reported in the returned value.
    pub async fn probe_compose_support() -> ComposeSupport {
        let mut version = isolated_docker_command();
        version.args(["compose", "version", "--short"]);
        let version = Self::bounded_command_output(
            version,
            COMPOSE_VERSION_TIMEOUT,
            "compose-support-probe",
            "probe Docker Compose version",
        )
        .await;
        let version = match version {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            Ok(output) => {
                return ComposeSupport {
                    available: false,
                    version: None,
                    missing_flags: Vec::new(),
                    problem: Some(format!(
                        "`docker compose version` failed ({}): {}. Install the Docker Compose v2 plugin, v{MIN_COMPOSE_VERSION} or newer.",
                        output.status,
                        sanitize_config_stderr(&output.stderr, &[])
                    )),
                }
            }
            Err(error) => {
                return ComposeSupport {
                    available: false,
                    version: None,
                    missing_flags: Vec::new(),
                    problem: Some(format!(
                        "Could not run `docker compose version`: {error}. Install the Docker Compose v2 plugin, v{MIN_COMPOSE_VERSION} or newer."
                    )),
                }
            }
        };
        let mut help = isolated_docker_command();
        help.args(["compose", "config", "--help"]);
        let help = Self::bounded_command_output(
            help,
            COMPOSE_VERSION_TIMEOUT,
            "compose-support-probe",
            "probe Docker Compose config options",
        )
        .await
        .map(|output| {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            text
        })
        .unwrap_or_default();
        Self::compose_support_from(Some(version), &help)
    }

    /// Interpret probe output; separate from the probe so it can be tested.
    fn compose_support_from(version: Option<String>, config_help: &str) -> ComposeSupport {
        let missing_flags: Vec<String> = COMPOSE_CONFIG_FLAGS
            .iter()
            .filter(|flag| {
                !config_help
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .any(|word| word == **flag)
            })
            .map(|flag| flag.to_string())
            .collect();
        let problem = (!missing_flags.is_empty()).then(|| {
            format!(
                "Docker Compose {} on this host does not support {} for `docker compose config`. Compose deployments that use a project override, include, extends or merge tags will fail until the Docker Compose plugin is upgraded to v{MIN_COMPOSE_VERSION} or newer.",
                version.as_deref().map(|v| format!("v{}", v.trim_start_matches('v'))).unwrap_or_else(|| "(unknown version)".to_string()),
                missing_flags.join(", ")
            )
        });
        ComposeSupport {
            available: true,
            version,
            missing_flags,
            problem,
        }
    }

    pub(super) async fn require_compose_v2(&self) -> Result<(), ComposeError> {
        let mut command = isolated_docker_command();
        command.args(["compose", "version"]);
        Self::check_compose_v2(command, COMPOSE_VERSION_TIMEOUT).await
    }

    async fn check_compose_v2(
        mut command: tokio::process::Command,
        timeout: std::time::Duration,
    ) -> Result<(), ComposeError> {
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| ComposeError::ComposeUnavailable {
                reason: format!("failed to run `docker compose version`: {error}"),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ComposeError::ComposeUnavailable {
                reason: "`docker compose version` did not expose stdout".to_string(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ComposeError::ComposeUnavailable {
                reason: "`docker compose version` did not expose stderr".to_string(),
            })?;
        let run = async {
            let (stdout, stderr, status) = tokio::try_join!(
                Self::read_bounded_stream(stdout, CapturedEnd::Head, &[], None),
                Self::read_bounded_stream(stderr, CapturedEnd::Tail, &[], None),
                child.wait()
            )?;
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout,
                stderr,
            })
        };
        let output = tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| ComposeError::ComposeUnavailable {
                reason: format!(
                    "`docker compose version` did not finish within {} seconds",
                    timeout.as_secs_f64()
                ),
            })?
            .map_err(|error| ComposeError::ComposeUnavailable {
                reason: format!("failed while running `docker compose version`: {error}"),
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let reason = if stderr.is_empty() {
            format!(
                "`docker compose version` exited with status {}",
                output.status
            )
        } else {
            format!("`docker compose version` failed: {stderr}")
        };
        Err(ComposeError::ComposeUnavailable { reason })
    }

    /// Whether this executor was constructed with an available Docker
    /// client. Callers that must refuse a whole deployment attempt before
    /// writing any files or invoking `docker compose` -- e.g. `DeployComposeJob`,
    /// which wants a job-level `LocalWorkloadsDisabled` failure rather than
    /// this crate's `ComposeError::DockerUnavailable` surfacing partway
    /// through -- should check this first.
    pub fn docker_available(&self) -> bool {
        self.docker.is_available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::exec::isolated_docker_command_with_candidates;
    use crate::compose::test_support::disabled_executor;
    use crate::compose::{
        ComposeError, ComposeExecutor, COMPOSE_VERSION_TIMEOUT, MAX_COMPOSE_COMMAND_OUTPUT_BYTES,
    };
    use bollard::Docker;
    use std::sync::Arc;

    #[tokio::test]
    async fn compose_v2_preflight_returns_actionable_error_when_plugin_is_missing() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "printf \"unknown shorthand flag: 'p' in -p\\n\" >&2; exit 125",
        ]);

        let error = ComposeExecutor::check_compose_v2(command, COMPOSE_VERSION_TIMEOUT)
            .await
            .expect_err("a Docker CLI without Compose must fail preflight");

        assert!(matches!(error, ComposeError::ComposeUnavailable { .. }));
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("Docker Compose v2 is unavailable"));
        assert!(diagnostic.contains("Install the Docker Compose CLI plugin"));
        assert!(diagnostic.contains("docker compose version"));
        assert!(diagnostic.contains("restart Temps"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compose_v2_preflight_checks_the_same_first_docker_candidate_used_for_deploys() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let first = directory.path().join("first-docker");
        let second = directory.path().join("second-docker");
        std::fs::write(
            &first,
            "#!/bin/sh\nprintf \"compose is unavailable\\n\" >&2\nexit 125\n",
        )
        .expect("first fake Docker CLI should be written");
        std::fs::write(&second, "#!/bin/sh\nexit 0\n")
            .expect("second fake Docker CLI should be written");
        for path in [&first, &second] {
            let mut permissions = std::fs::metadata(path)
                .expect("fake Docker CLI metadata should be readable")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions)
                .expect("fake Docker CLI should be executable");
        }
        let first = first.to_str().expect("temporary path should be UTF-8");
        let second = second.to_str().expect("temporary path should be UTF-8");
        let mut command = isolated_docker_command_with_candidates(&[first, second]);
        command.args(["compose", "version"]);

        let error = ComposeExecutor::check_compose_v2(command, COMPOSE_VERSION_TIMEOUT)
            .await
            .expect_err("preflight must check the same first candidate used by deployment");

        assert!(matches!(error, ComposeError::ComposeUnavailable { .. }));
        assert!(error.to_string().contains("compose is unavailable"));
    }

    #[tokio::test]
    async fn compose_v2_preflight_bounds_diagnostic_output() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "head -c 2097152 /dev/zero | tr '\\0' x >&2; exit 125"]);

        let error = ComposeExecutor::check_compose_v2(command, COMPOSE_VERSION_TIMEOUT)
            .await
            .expect_err("a noisy failing Compose probe must fail preflight");
        let diagnostic = error.to_string();

        assert!(matches!(error, ComposeError::ComposeUnavailable { .. }));
        assert!(
            diagnostic.len() <= MAX_COMPOSE_COMMAND_OUTPUT_BYTES + 512,
            "diagnostic must remain bounded, got {} bytes",
            diagnostic.len()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compose_v2_preflight_kills_child_when_probe_times_out() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let pid_file = directory.path().join("compose-probe.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "echo $$ > \"$PID_FILE\"; exec sleep 30"])
            .env("PID_FILE", &pid_file);

        let error =
            ComposeExecutor::check_compose_v2(command, std::time::Duration::from_millis(100))
                .await
                .expect_err("a hanging Compose probe must time out");
        assert!(matches!(error, ComposeError::ComposeUnavailable { .. }));

        let pid = std::fs::read_to_string(&pid_file)
            .expect("probe should record its pid before timing out");
        let pid = pid.trim();
        let mut still_running = true;
        for _ in 0..50 {
            still_running = std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .is_ok_and(|status| status.success());
            if !still_running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !still_running,
            "timed-out Compose probe {pid} is still running"
        );
    }

    #[test]
    fn docker_available_is_false_for_a_disabled_handle_and_true_for_a_real_client() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        assert!(!disabled_executor(tmp.path().to_path_buf()).docker_available());

        if let Ok(docker) = Docker::connect_with_defaults() {
            let executor = ComposeExecutor::new(Arc::new(docker), tmp.path().to_path_buf());
            assert!(executor.docker_available());
        }
    }

    #[test]
    fn compose_support_reports_missing_config_flags() {
        let old_help = "Usage: docker compose config [OPTIONS]\n      --no-normalize   Don't normalize\n      --no-path-resolution  Don't resolve\n      --no-interpolate  Don't interpolate\n";
        let support = ComposeExecutor::compose_support_from(Some("2.29.7".to_string()), old_help);
        assert!(support.available);
        assert!(!support.supported());
        assert_eq!(
            support.missing_flags,
            vec!["--no-env-resolution".to_string()]
        );
        let problem = support.problem.unwrap();
        assert!(
            problem.contains("v2.29.7") && problem.contains(MIN_COMPOSE_VERSION),
            "{problem}"
        );

        let new_help =
            format!("{old_help}      --no-env-resolution  Don't resolve service env files\n");
        let support = ComposeExecutor::compose_support_from(Some("2.35.1".to_string()), &new_help);
        assert!(support.supported());
        assert!(support.problem.is_none());
    }
}
