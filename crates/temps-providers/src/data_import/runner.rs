// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The helper container that moves the data.
//!
//! One short-lived container per run, on the target service's Docker network,
//! runs `producer | consumer` (see [`super::TransferPlan`]). It is:
//!
//! - **bounded** — force-removed when the run's timeout expires;
//! - **attributable** — labelled with its run and service, so a cancel or a
//!   restart can find and stop it without trusting in-memory state;
//! - **honest about which side failed** — [`compose_script`] records the
//!   producer's exit status apart from the consumer's, so "the source refused
//!   the login" is never reported as "the target rejected the data";
//! - **contained** — no capabilities, no privilege escalation, and the source
//!   host pinned to the address the SSRF guard validated.
//!
//! The dump is streamed through a pipe: memory stays constant whatever the
//! size of the database, and nothing is written to disk.

use std::collections::HashMap;
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptions, ListContainersOptionsBuilder,
    LogsOptionsBuilder, RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use tracing::{info, warn};

use super::source::scrub_secrets;
use super::{DataImportError, TransferPlan};
use crate::externalsvc::restore_helper::KIND_LABEL;

/// Value of [`KIND_LABEL`] for data import helpers.
pub const DATA_IMPORT_HELPER_KIND: &str = "data-import-helper";
/// Label carrying the `service_data_imports.id` a helper belongs to.
pub const RUN_LABEL: &str = "sh.temps.data-import.run";
/// Label carrying the target service id.
pub const SERVICE_LABEL: &str = "sh.temps.data-import.service";

/// Exit status of the helper when the producer (the source side) failed.
pub const SOURCE_FAILED_EXIT: i64 = 90;
/// Exit status of the helper when the consumer (the target side) failed.
pub const TARGET_FAILED_EXIT: i64 = 91;

/// Lines of helper output kept for error messages.
const LOG_TAIL_LINES: &str = "60";
/// Characters of helper output kept for error messages.
const LOG_TAIL_MAX_CHARS: usize = 3000;
/// Bound on Docker API calls that are not the transfer itself.
const DOCKER_API_TIMEOUT: Duration = Duration::from_secs(60);
/// Seconds Docker waits after SIGTERM before SIGKILL when fencing a helper.
const FENCE_STOP_TIMEOUT_SECS: i32 = 10;

/// The shell script the helper runs: `producer | consumer`, with the
/// producer's exit status captured separately. A pipeline's own status is
/// only the consumer's, so without this a source that refuses the login
/// would pipe an empty stream into a consumer that happily exits 0.
///
/// The consumer is checked first: when it dies, the producer dies of SIGPIPE
/// right after, and the consumer's failure is the cause.
pub fn compose_script(producer: &str, consumer: &str) -> String {
    format!(
        r#"status_file="${{TMPDIR:-/tmp}}/temps-import-source.$$"
rm -f "$status_file"
{{ ( {producer} ); echo $? > "$status_file"; }} | ( {consumer} )
target_status=$?
source_status=$(cat "$status_file" 2>/dev/null || echo missing)
rm -f "$status_file"
if [ "$target_status" -ne 0 ]; then
  echo "temps-import: applying the data to the target failed (exit status $target_status)" >&2
  exit {TARGET_FAILED_EXIT}
fi
if [ "$source_status" != "0" ]; then
  echo "temps-import: reading the source failed (exit status $source_status)" >&2
  exit {SOURCE_FAILED_EXIT}
fi
exit 0
"#
    )
}

/// Everything needed to start one helper.
pub struct HelperRequest<'a> {
    pub run_id: i32,
    pub service_id: i32,
    /// Unique container name.
    pub container_name: &'a str,
    pub plan: &'a TransferPlan,
    /// Docker network the target is reachable on.
    pub network: &'a str,
    /// `host:ip` pins for the source hosts.
    pub extra_hosts: Vec<String>,
    pub timeout: Duration,
    /// Values scrubbed from any output returned.
    pub secrets: &'a [String],
}

/// How a helper ended. Every carried output is already scrubbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperOutcome {
    /// The copy finished; `output` is what the tools printed.
    Succeeded { output: String },
    /// Reading the source failed.
    SourceFailed { output: String },
    /// Applying the data to the target failed.
    TargetFailed { output: String },
    /// The helper exited with an unexpected status (killed, tool missing, ...).
    Exited { exit_code: i64, output: String },
    /// The timeout expired; the helper was force-removed.
    TimedOut { output: String },
    /// The helper vanished while being waited on (removed by a cancel).
    Vanished { reason: String },
}

impl HelperOutcome {
    /// What the helper printed, when it got as far as printing anything.
    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Succeeded { output }
            | Self::SourceFailed { output }
            | Self::TargetFailed { output }
            | Self::Exited { output, .. }
            | Self::TimedOut { output } => Some(output.as_str()).filter(|o| !o.is_empty()),
            Self::Vanished { .. } => None,
        }
    }
}

/// Labels identifying a helper.
pub fn helper_labels(run_id: i32, service_id: i32) -> HashMap<String, String> {
    HashMap::from([
        (KIND_LABEL.to_string(), DATA_IMPORT_HELPER_KIND.to_string()),
        (RUN_LABEL.to_string(), run_id.to_string()),
        (SERVICE_LABEL.to_string(), service_id.to_string()),
    ])
}

/// Where a helper must run to reach `target_container`, and the host name it
/// uses for it: the Temps workload network by name when the target is on it,
/// otherwise the target's first network — by name on a user-defined network,
/// by address on the default bridge, which has no DNS.
pub async fn resolve_target_network(
    docker: &Docker,
    service_id: i32,
    target_container: &str,
) -> Result<(String, String), DataImportError> {
    let inspect = tokio::time::timeout(
        DOCKER_API_TIMEOUT,
        docker.inspect_container(target_container, None::<InspectContainerOptions>),
    )
    .await
    .map_err(|_| DataImportError::NotReady {
        service_id,
        reason: format!(
            "inspecting container '{}' timed out after {}s",
            target_container,
            DOCKER_API_TIMEOUT.as_secs()
        ),
    })?
    .map_err(|e| DataImportError::NotReady {
        service_id,
        reason: format!(
            "container '{}' cannot be inspected: {}",
            target_container, e
        ),
    })?;

    let networks = inspect
        .network_settings
        .and_then(|settings| settings.networks)
        .unwrap_or_default();
    pick_network(
        &networks,
        temps_core::NETWORK_NAME.as_str(),
        target_container,
    )
    .ok_or_else(|| DataImportError::NotReady {
        service_id,
        reason: format!(
            "container '{}' is not attached to any network a helper container can join",
            target_container
        ),
    })
}

/// Pure choice behind [`resolve_target_network`].
fn pick_network(
    networks: &HashMap<String, bollard::models::EndpointSettings>,
    preferred: &str,
    target_container: &str,
) -> Option<(String, String)> {
    if networks.contains_key(preferred) {
        return Some((preferred.to_string(), target_container.to_string()));
    }
    let mut names: Vec<&String> = networks
        .keys()
        .filter(|name| !matches!(name.as_str(), "host" | "none"))
        .collect();
    names.sort();
    let name = names.first()?;
    if name.as_str() == "bridge" {
        let ip = networks
            .get(*name)
            .and_then(|endpoint| endpoint.ip_address.clone())
            .filter(|ip| !ip.is_empty())?;
        Some(((*name).clone(), ip))
    } else {
        Some(((*name).clone(), target_container.to_string()))
    }
}

/// Make sure `image` is present, pulling it if needed.
async fn ensure_image(
    docker: &Docker,
    service_id: i32,
    image: &str,
) -> Result<(), DataImportError> {
    if docker.inspect_image(image).await.is_ok() {
        return Ok(());
    }
    crate::utils::pull_image_with_retry(docker, image, None)
        .await
        .map_err(|e| DataImportError::Helper {
            service_id,
            reason: format!("pulling image '{}' failed: {}", image, e),
        })
}

/// Create, start and wait for the helper, then stop it. Test-only: it starts
/// the helper without checking for a cancellation that arrived while the
/// image was pulled, so production code must not use it. The import job
/// calls [`create_helper`], checks for a cancellation, then
/// [`start_and_wait`].
#[cfg(test)]
pub async fn run_helper(
    docker: &Docker,
    request: &HelperRequest<'_>,
) -> Result<HelperOutcome, DataImportError> {
    let id = create_helper(docker, request).await?;
    let outcome = start_and_wait(docker, &id, request).await;
    ensure_helper_stopped(docker, request.service_id, &id).await?;
    outcome
}

/// Pull the image if needed and create (but do not start) the helper.
/// Returns the container id. Splitting creation from start lets the caller
/// check for a cancellation that arrived during a slow image pull before
/// anything runs.
pub async fn create_helper(
    docker: &Docker,
    request: &HelperRequest<'_>,
) -> Result<String, DataImportError> {
    let service_id = request.service_id;
    ensure_image(docker, service_id, &request.plan.image).await?;

    let script = compose_script(&request.plan.producer, &request.plan.consumer);
    let env: Vec<String> = request
        .plan
        .env
        .iter()
        .map(|e| format!("{}={}", e.name, e.value))
        .collect();
    let body = ContainerCreateBody {
        image: Some(request.plan.image.clone()),
        entrypoint: Some(vec!["sh".to_string(), "-c".to_string()]),
        cmd: Some(vec![script]),
        env: Some(env),
        labels: Some(helper_labels(request.run_id, service_id)),
        host_config: Some(HostConfig {
            network_mode: Some(request.network.to_string()),
            extra_hosts: Some(request.extra_hosts.clone()),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            auto_remove: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = tokio::time::timeout(
        DOCKER_API_TIMEOUT,
        docker.create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(request.container_name)
                    .build(),
            ),
            body,
        ),
    )
    .await
    .map_err(|_| DataImportError::Helper {
        service_id,
        reason: format!(
            "creating helper container '{}' timed out",
            request.container_name
        ),
    })?
    .map_err(|e| DataImportError::Helper {
        service_id,
        reason: format!(
            "creating helper container '{}' failed: {}",
            request.container_name, e
        ),
    })?;

    Ok(created.id)
}

/// Start a helper created by [`create_helper`] and wait for it, bounded by
/// the request's timeout.
///
/// Returns `Err` only when the helper could not be started (for instance
/// because a cancellation removed it first); once it runs, every ending is
/// a [`HelperOutcome`]. The helper is removed on a best-effort basis; the
/// caller must confirm it stopped with [`ensure_helper_stopped`].
pub async fn start_and_wait(
    docker: &Docker,
    container_id: &str,
    request: &HelperRequest<'_>,
) -> Result<HelperOutcome, DataImportError> {
    let service_id = request.service_id;
    if let Err(e) = docker
        .start_container(container_id, None::<StartContainerOptions>)
        .await
    {
        remove_container(docker, container_id).await;
        return Err(DataImportError::Helper {
            service_id,
            reason: format!(
                "starting helper container '{}' failed: {}",
                request.container_name, e
            ),
        });
    }
    info!(
        run_id = request.run_id,
        service_id,
        container = request.container_name,
        image = %request.plan.image,
        timeout_secs = request.timeout.as_secs(),
        "Started data import helper"
    );

    let mut wait = docker.wait_container(container_id, None::<WaitContainerOptions>);
    let waited = tokio::time::timeout(request.timeout, wait.next()).await;
    let outcome = match waited {
        // bollard reports a non-zero exit as an error carrying the code.
        Ok(Some(Ok(result))) => Ok(result.status_code),
        Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => Ok(code),
        Ok(Some(Err(e))) => Err(HelperOutcome::Vanished {
            reason: scrub_secrets(&e.to_string(), request.secrets),
        }),
        Ok(None) => Err(HelperOutcome::Vanished {
            reason: "the Docker wait stream ended without an exit status".to_string(),
        }),
        Err(_) => {
            let output = log_tail(docker, container_id, request.secrets).await;
            Err(HelperOutcome::TimedOut { output })
        }
    };

    let result = match outcome {
        Ok(exit_code) => {
            let output = log_tail(docker, container_id, request.secrets).await;
            match exit_code {
                0 => HelperOutcome::Succeeded { output },
                SOURCE_FAILED_EXIT => HelperOutcome::SourceFailed { output },
                TARGET_FAILED_EXIT => HelperOutcome::TargetFailed { output },
                exit_code => HelperOutcome::Exited { exit_code, output },
            }
        }
        Err(outcome) => outcome,
    };
    remove_container(docker, container_id).await;
    Ok(result)
}

/// Last lines of the helper's output, scrubbed and bounded.
async fn log_tail(docker: &Docker, container_id: &str, secrets: &[String]) -> String {
    let mut stream = docker.logs(
        container_id,
        Some(
            LogsOptionsBuilder::new()
                .stdout(true)
                .stderr(true)
                .tail(LOG_TAIL_LINES)
                .build(),
        ),
    );
    let mut output = String::new();
    let collect = async {
        while let Some(Ok(chunk)) = stream.next().await {
            output.push_str(&chunk.to_string());
        }
    };
    let _ = tokio::time::timeout(DOCKER_API_TIMEOUT, collect).await;
    bound_output(&scrub_secrets(&output, secrets))
}

/// Keep the end of `output` — where the error is — within the size limit.
fn bound_output(output: &str) -> String {
    let trimmed = output.trim();
    let count = trimmed.chars().count();
    if count <= LOG_TAIL_MAX_CHARS {
        return trimmed.to_string();
    }
    let tail: String = trimmed.chars().skip(count - LOG_TAIL_MAX_CHARS).collect();
    format!("…{tail}")
}

async fn remove_container(docker: &Docker, container: &str) {
    let removed = tokio::time::timeout(
        DOCKER_API_TIMEOUT,
        docker.remove_container(
            container,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        ),
    )
    .await;
    match removed {
        Ok(Ok(())) => {}
        Ok(Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        })) => {}
        Ok(Err(e)) => warn!(container, "Could not remove data import helper: {}", e),
        Err(_) => warn!(container, "Removing data import helper timed out"),
    }
}

/// Confirm the helper can no longer write: it is gone, or it exists but is
/// not running. A running helper is killed and removed first. Returns an
/// error when that cannot be confirmed (Docker unreachable, kill refused);
/// the caller must then keep the run — and with it the database lock — and
/// try again.
pub async fn ensure_helper_stopped(
    docker: &Docker,
    service_id: i32,
    container: &str,
) -> Result<(), DataImportError> {
    let unconfirmed = |reason: String| DataImportError::Helper {
        service_id,
        reason: format!("could not confirm helper '{container}' stopped: {reason}"),
    };
    if !helper_running(docker, container)
        .await
        .map_err(unconfirmed)?
    {
        remove_container(docker, container).await;
        return Ok(());
    }
    // Still writing: force-remove kills it.
    tokio::time::timeout(
        DOCKER_API_TIMEOUT,
        docker.remove_container(
            container,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        ),
    )
    .await
    .map_err(|_| unconfirmed("removing it timed out".to_string()))?
    .or_else(|e| match e {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => Ok(()),
        other => Err(unconfirmed(other.to_string())),
    })?;
    if helper_running(docker, container)
        .await
        .map_err(unconfirmed)?
    {
        return Err(unconfirmed("it is still running after removal".to_string()));
    }
    Ok(())
}

/// Whether `container` exists and is running. `Err` when Docker cannot say.
async fn helper_running(docker: &Docker, container: &str) -> Result<bool, String> {
    match tokio::time::timeout(
        DOCKER_API_TIMEOUT,
        docker.inspect_container(container, None::<InspectContainerOptions>),
    )
    .await
    {
        Err(_) => Err("inspecting it timed out".to_string()),
        Ok(Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        })) => Ok(false),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(inspect)) => Ok(inspect
            .state
            .and_then(|state| state.running)
            .unwrap_or(false)),
    }
}

/// Force-remove a run's helper by name — the cancel path. A helper that no
/// longer exists is not an error.
pub async fn stop_helper(docker: &Docker, container_name: &str) {
    remove_container(docker, container_name).await;
}

/// Stop and remove every helper labelled with `run_id`. Used at startup for
/// runs whose owning process died. Returns how many helpers were found.
pub async fn fence_run_helpers(
    docker: &Docker,
    service_id: i32,
    run_id: i32,
) -> Result<usize, DataImportError> {
    let filters = HashMap::from([(
        "label".to_string(),
        vec![format!("{}={}", RUN_LABEL, run_id)],
    )]);
    let containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .map_err(|e| DataImportError::Helper {
            service_id,
            reason: format!("listing helpers of data import {} failed: {}", run_id, e),
        })?;

    let mut found = 0;
    for container in containers {
        let Some(id) = container.id else { continue };
        found += 1;
        if matches!(
            container.state,
            Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
                | Some(bollard::models::ContainerSummaryStateEnum::RESTARTING)
                | Some(bollard::models::ContainerSummaryStateEnum::PAUSED)
        ) {
            docker
                .stop_container(
                    &id,
                    Some(bollard::query_parameters::StopContainerOptions {
                        t: Some(FENCE_STOP_TIMEOUT_SECS),
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| DataImportError::Helper {
                    service_id,
                    reason: format!(
                        "stopping surviving helper {} of data import {} failed: {}",
                        id, run_id, e
                    ),
                })?;
        }
        remove_container(docker, &id).await;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::EndpointSettings;

    fn endpoint(ip: &str) -> EndpointSettings {
        EndpointSettings {
            ip_address: Some(ip.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn prefers_the_temps_network_by_container_name() {
        let networks = HashMap::from([
            ("bridge".to_string(), endpoint("172.17.0.4")),
            ("temps-app-network".to_string(), endpoint("172.20.0.9")),
        ]);
        assert_eq!(
            pick_network(&networks, "temps-app-network", "postgres-orders"),
            Some((
                "temps-app-network".to_string(),
                "postgres-orders".to_string()
            ))
        );
    }

    #[test]
    fn falls_back_to_a_user_defined_network_by_name_and_bridge_by_address() {
        let user_defined = HashMap::from([("legacy_default".to_string(), endpoint("172.30.0.2"))]);
        assert_eq!(
            pick_network(&user_defined, "temps-app-network", "db"),
            Some(("legacy_default".to_string(), "db".to_string()))
        );

        let bridge = HashMap::from([("bridge".to_string(), endpoint("172.17.0.4"))]);
        assert_eq!(
            pick_network(&bridge, "temps-app-network", "db"),
            Some(("bridge".to_string(), "172.17.0.4".to_string()))
        );
    }

    #[test]
    fn host_and_none_networks_are_never_joined() {
        let networks = HashMap::from([
            ("host".to_string(), endpoint("")),
            ("none".to_string(), endpoint("")),
        ]);
        assert_eq!(pick_network(&networks, "temps-app-network", "db"), None);
    }

    #[test]
    fn script_reports_the_failing_side_with_distinct_statuses() {
        let script = compose_script("pg_dump \"$SRC\"", "psql \"$DST\"");
        assert!(script
            .contains("{ ( pg_dump \"$SRC\" ); echo $? > \"$status_file\"; } | ( psql \"$DST\" )"));
        assert!(script.contains(&format!("exit {TARGET_FAILED_EXIT}")));
        assert!(script.contains(&format!("exit {SOURCE_FAILED_EXIT}")));
        // The consumer is judged first: a target failure kills the producer
        // with SIGPIPE, and that must not be blamed on the source.
        let target_check = script.find("target_status\" -ne 0").expect("target check");
        let source_check = script
            .find("source_status\" != \"0\"")
            .expect("source check");
        assert!(target_check < source_check);
    }

    #[test]
    fn helper_labels_identify_run_and_service() {
        let labels = helper_labels(42, 7);
        assert_eq!(
            labels.get(KIND_LABEL).map(String::as_str),
            Some(DATA_IMPORT_HELPER_KIND)
        );
        assert_eq!(labels.get(RUN_LABEL).map(String::as_str), Some("42"));
        assert_eq!(labels.get(SERVICE_LABEL).map(String::as_str), Some("7"));
    }

    #[test]
    fn output_keeps_the_end_where_the_error_is() {
        let long = format!(
            "{}ERROR: relation exists",
            "x".repeat(LOG_TAIL_MAX_CHARS * 2)
        );
        let bounded = bound_output(&long);
        assert!(bounded.ends_with("ERROR: relation exists"));
        assert!(bounded.chars().count() <= LOG_TAIL_MAX_CHARS + 1);
    }

    /// A helper that is still running is killed and gone afterwards; one that
    /// no longer exists counts as stopped. This is what keeps a run (and its
    /// database lock) held until no writer can survive it.
    #[tokio::test]
    async fn ensure_helper_stopped_kills_a_running_helper() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let image = "python:3.13-slim";
        if ensure_image(&docker, 1, image).await.is_err() {
            println!("{image} not available, skipping");
            return;
        }
        let name = format!(
            "temps-data-import-stop-test-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec!["sleep".to_string(), "300".to_string()]),
                    labels: Some(helper_labels(-1, 1)),
                    ..Default::default()
                },
            )
            .await
            .expect("create");
        docker
            .start_container(&name, None::<StartContainerOptions>)
            .await
            .expect("start");

        ensure_helper_stopped(&docker, 1, &name)
            .await
            .expect("stopped");
        let gone = docker
            .inspect_container(&name, None::<InspectContainerOptions>)
            .await;
        assert!(
            matches!(
                gone,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                })
            ),
            "helper must be gone: {gone:?}"
        );
        // Already gone: nothing left to stop.
        ensure_helper_stopped(&docker, 1, &name)
            .await
            .expect("absent helper counts as stopped");
    }

    /// Runs the composed script in a real shell to prove each side's failure
    /// maps to its own exit status — the whole point of the wrapper.
    #[test]
    fn script_exit_statuses_match_the_failing_side() {
        let run = |producer: &str, consumer: &str| -> i32 {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(compose_script(producer, consumer))
                .output()
                .expect("run sh")
                .status
                .code()
                .unwrap_or(-1)
        };
        assert_eq!(run("echo data", "cat >/dev/null"), 0);
        assert_eq!(
            run("echo partial; exit 3", "cat >/dev/null"),
            SOURCE_FAILED_EXIT as i32
        );
        assert_eq!(
            run("echo data", "cat >/dev/null; exit 4"),
            TARGET_FAILED_EXIT as i32
        );
        // Both failing: the target is the cause.
        assert_eq!(
            run("exit 2", "cat >/dev/null; exit 5"),
            TARGET_FAILED_EXIT as i32
        );
        // A consumer that is itself a list still reads the whole stream.
        assert_eq!(run("echo data", "x=1 && grep -q data && cat >/dev/null"), 0);
        assert_eq!(
            run("echo other", "x=1 && grep -q data"),
            TARGET_FAILED_EXIT as i32
        );
    }
}
