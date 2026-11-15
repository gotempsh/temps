// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Waiting for services to become ready, and discovering and verifying the
//! containers Compose started.

use bollard::query_parameters::{LogsOptions, RemoveContainerOptions};
use futures::{StreamExt, TryStreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use super::diagnostics::{
    looks_like_capability_denial, quote_service_list, sanitize_compose_diagnostic,
};
use super::{
    ComposeDeployFailure, ComposeDeployRequest, ComposeError, ComposeExecutor, ComposePortBinding,
    ComposeServiceResult, COMPOSE_READY_POLL_INTERVAL, ELEVATED_PERMISSIONS_SETTINGS_PATH,
    PORT_PROBE_CONNECT_TIMEOUT,
};
use tokio::net::TcpStream;
use tracing::{debug, warn};
use yaml_serde::Value as YamlValue;

impl ComposeExecutor {
    /// Trailing log lines captured per unhealthy/stopped container when a
    /// Compose deploy fails — enough to show the actual startup error (e.g. a
    /// Postgres auth or permission failure) without risking an unbounded dump
    /// into the job's error message.
    const FAILED_CONTAINER_LOG_TAIL: &'static str = "150";

    /// Best-effort debug aid for a failed Compose deploy: finds every
    /// container that isn't `running` + (`healthy` or no healthcheck), and
    /// appends its own log tail. Compose's own failure text only says e.g.
    /// "container X is unhealthy" — it never explains *why*, so without this
    /// the user has to know to go find the container and inspect it manually.
    /// Never itself fails the caller: a container that can't be inspected or
    /// whose logs can't be fetched just gets a short note instead of blocking
    /// the (already-failing) deploy from reporting its real error.
    pub(super) async fn describe_unhealthy_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
    ) -> String {
        let entries = match self
            .compose_ps(project_dir, project_name, compose_file)
            .await
        {
            Ok(entries) => entries,
            Err(e) => {
                return format!(
                    "\n\n(could not list containers to capture debug logs: {})",
                    e
                )
            }
        };

        let mut sections = Vec::new();
        let mut capability_denied = Vec::new();
        for entry in &entries {
            let is_unhealthy = !entry.health.is_empty() && entry.health != "healthy";
            let is_not_running = entry.state != "running";
            if !is_unhealthy && !is_not_running {
                continue;
            }

            let logs = sanitize_compose_diagnostic(
                &self.container_log_tail(&entry.id).await,
                redact_values,
            );
            if looks_like_capability_denial(&logs) {
                capability_denied.push(entry.service.clone());
            }
            let health = if entry.health.is_empty() {
                "n/a"
            } else {
                &entry.health
            };
            sections.push(format!(
                "--- {} (service '{}', state={}, health={}) ---\n{}",
                entry.name,
                entry.service,
                entry.state,
                health,
                logs.trim()
            ));
        }

        if sections.is_empty() {
            return String::new();
        }

        format!(
            "\n\nContainer logs for unhealthy/stopped services:\n\n{}{}",
            sections.join("\n\n"),
            Self::capability_denial_remediation(&capability_denied)
        )
    }

    /// Turn a detected capability denial into the one instruction that fixes
    /// it. Appended to the failure text rather than only logged, because the
    /// advisory `deploy_compose` emits before the deploy is easy to miss under
    /// hundreds of lines of image-pull progress — whereas the error is the one
    /// thing the operator cannot avoid reading.
    ///
    /// Every sandboxed service already gets `RELAXED_CAPABILITIES` by
    /// default, so a denial matching this pattern means the image needs a
    /// capability outside that default set (or hit an unrelated permission
    /// problem, e.g. a read-only mount) — the fix is no longer a toggle to
    /// flip but exempting the service from the sandbox entirely.
    fn capability_denial_remediation(denied_services: &[String]) -> String {
        if denied_services.is_empty() {
            return String::new();
        }

        format!(
            "\n\nService(s) {} failed with \"Operation not permitted\" despite Temps granting \
             every sandboxed Compose service {} by default — the capabilities official image \
             entrypoints commonly need to fix ownership on a data/cache directory and drop from \
             root to a service user. This means the image needs a capability outside that \
             default set, or hit an unrelated permission problem (e.g. a read-only mount). If \
             the image genuinely needs broader permissions, use \"Disable sandbox\" for it in \
             {}, then redeploy.",
            quote_service_list(&denied_services.iter().collect::<Vec<_>>()),
            Self::RELAXED_CAPABILITIES.join(", "),
            ELEVATED_PERMISSIONS_SETTINGS_PATH,
        )
    }

    /// Fetches the last [`Self::FAILED_CONTAINER_LOG_TAIL`] lines (stdout +
    /// stderr, interleaved in emission order) for one container. Returns a
    /// human-readable placeholder instead of an error so callers can always
    /// embed the result directly in a debug message.
    pub(super) async fn container_log_tail(&self, container_id: &str) -> String {
        let Some(docker) = self.docker.get().cloned() else {
            return "(no log output: local Docker daemon unavailable)".to_string();
        };
        let logs_stream = docker.logs(
            container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                tail: Self::FAILED_CONTAINER_LOG_TAIL.to_string(),
                ..Default::default()
            }),
        );

        match logs_stream
            .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).into_owned()))
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(chunks) if chunks.iter().all(|c| c.trim().is_empty()) => {
                "(no log output)".to_string()
            }
            Ok(chunks) => chunks.join(""),
            Err(e) => format!("(failed to fetch logs: {})", e),
        }
    }

    /// Poll `docker compose ps` until every service is `running` (and
    /// `healthy`, for services that declare a healthcheck) or `timeout`
    /// elapses. A service stuck `exited`/`dead`/`restarting` fails fast
    /// instead of waiting out the full timeout.
    pub(super) async fn wait_for_services_ready(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        redact_values: &[String],
        timeout: std::time::Duration,
        expectations: &ReadinessExpectations,
    ) -> Result<(), ComposeError> {
        let start = std::time::Instant::now();
        let mut observed_ready_once = false;
        loop {
            let entries = self
                .compose_ps(project_dir, project_name, compose_file)
                .await?;
            match Self::classify_readiness_with(&entries, expectations) {
                ComposeReadiness::Ready => {
                    // `docker compose ps` reports a container "running" the
                    // instant its process starts — for a service with no
                    // `healthcheck:` block, that can be well before the app
                    // inside has actually bound its published port. Trusting
                    // that state as done meant `deploy_compose` returned
                    // success while the app was still starting, and the
                    // public-readiness gate in MarkDeploymentCompleteJob
                    // (which runs immediately after) would race it and
                    // revert a perfectly good deployment. Close that gap
                    // here instead: for services without a healthcheck,
                    // require their published TCP ports to actually accept
                    // a connection before calling the stack ready.
                    let unreachable =
                        Self::unreachable_published_ports(&entries, self.published_host_ip).await;
                    if unreachable.is_empty() {
                        // Docker may briefly report a just-started container as
                        // running before its health state is populated (or
                        // before a short-lived process exits). Require one
                        // stable poll interval before accepting readiness.
                        if observed_ready_once {
                            return Ok(());
                        }
                        observed_ready_once = true;
                        tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                        continue;
                    }
                    observed_ready_once = false;
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                redact_values,
                            )
                            .await;
                        return Err(ComposeError::ServicesNotReady {
                            project: project_name.to_string(),
                            timeout_secs: timeout.as_secs(),
                            reason: format!("{}{}", unreachable.join(", "), container_logs),
                        });
                    }
                    tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                }
                ComposeReadiness::Failed(reasons) => {
                    let container_logs = self
                        .describe_unhealthy_containers(
                            project_dir,
                            project_name,
                            compose_file,
                            redact_values,
                        )
                        .await;
                    return Err(ComposeError::ServicesNotReady {
                        project: project_name.to_string(),
                        timeout_secs: timeout.as_secs(),
                        reason: format!("{}{}", reasons.join(", "), container_logs),
                    });
                }
                ComposeReadiness::Pending(reasons) => {
                    observed_ready_once = false;
                    if start.elapsed() >= timeout {
                        let container_logs = self
                            .describe_unhealthy_containers(
                                project_dir,
                                project_name,
                                compose_file,
                                redact_values,
                            )
                            .await;
                        return Err(ComposeError::ServicesNotReady {
                            project: project_name.to_string(),
                            timeout_secs: timeout.as_secs(),
                            reason: format!("{}{}", reasons.join(", "), container_logs),
                        });
                    }
                    tokio::time::sleep(COMPOSE_READY_POLL_INTERVAL).await;
                }
            }
        }
    }

    /// Classify a `docker compose ps` snapshot with every service required
    /// to keep running (no one-shot tasks). See [`Self::classify_readiness_with`].
    #[cfg(test)]
    fn classify_readiness(entries: &[ComposePsEntry]) -> ComposeReadiness {
        Self::classify_readiness_with(entries, &ReadinessExpectations::all_long_running())
    }

    /// Classify a `docker compose ps` snapshot into ready/pending/failed.
    /// A service is ready once it's `running` and, if it declares a
    /// healthcheck, `healthy`. `dead`, and `exited` with a non-zero status,
    /// fail fast rather than waiting out the full timeout; any other state
    /// (`created`, `restarting`, `starting` health) is still pending.
    ///
    /// A service that exited with status 0 is a completed one-shot task — a
    /// migration, seed or init container — unless `expectations` says it
    /// must keep running (it is routed, publishes ports, has a healthcheck,
    /// or another service waits for it to be healthy). A stack in which
    /// nothing is left running is still a failure.
    fn classify_readiness_with(
        entries: &[ComposePsEntry],
        expectations: &ReadinessExpectations,
    ) -> ComposeReadiness {
        if entries.is_empty() {
            return ComposeReadiness::Pending(vec![
                "no containers found after 'docker compose up'".to_string(),
            ]);
        }

        let mut failed = Vec::new();
        let mut pending = Vec::new();
        let mut running = 0_usize;
        for entry in entries {
            match entry.state.as_str() {
                "running" => {
                    running += 1;
                    match entry.health.as_str() {
                        "" | "healthy" => {}
                        "unhealthy" => {
                            failed.push(format!("service '{}' is unhealthy", entry.service))
                        }
                        other => pending.push(format!("service '{}' is {other}", entry.service)),
                    }
                }
                "exited" if entry.exit_code == 0 => {
                    if let Some(reason) = expectations.why_long_running(&entry.service) {
                        failed.push(format!(
                            "service '{}' exited (code 0) but must keep running because it {reason}",
                            entry.service
                        ));
                    }
                }
                "exited" => failed.push(format!(
                    "service '{}' exited (code {})",
                    entry.service, entry.exit_code
                )),
                "dead" => failed.push(format!("service '{}' dead", entry.service)),
                other => pending.push(format!("service '{}' is {other}", entry.service)),
            }
        }

        if !failed.is_empty() {
            ComposeReadiness::Failed(failed)
        } else if !pending.is_empty() {
            ComposeReadiness::Pending(pending)
        } else if running == 0 {
            ComposeReadiness::Failed(vec![
                "every service exited before the stack became ready: one-shot tasks completed, \
                 but no service keeps running to serve the deployment"
                    .to_string(),
            ])
        } else {
            ComposeReadiness::Ready
        }
    }

    /// Work out which services must stay running, from the compose file, the
    /// inline override and the services Temps routes to. An unparsable
    /// document falls back to requiring every service to run, which is the
    /// behaviour before one-shot tasks were recognised.
    pub(crate) fn readiness_expectations(
        compose_content: &str,
        compose_override: Option<&str>,
        routed_services: &[String],
    ) -> ReadinessExpectations {
        let mut reasons: HashMap<String, &'static str> = HashMap::new();
        for service in routed_services {
            reasons.insert(service.clone(), "receives the deployment's public traffic");
        }
        for document in std::iter::once(compose_content).chain(compose_override) {
            let mut root: YamlValue = match yaml_serde::from_str(document) {
                Ok(root) => root,
                Err(_) => return ReadinessExpectations::all_long_running(),
            };
            if root.apply_merge().is_err() {
                return ReadinessExpectations::all_long_running();
            }
            let Some(services) = root.get("services").and_then(YamlValue::as_mapping) else {
                continue;
            };
            for (name, definition) in services {
                let (Some(name), Some(definition)) = (name.as_str(), definition.as_mapping())
                else {
                    continue;
                };
                let field = |key: &str| definition.get(YamlValue::String(key.to_string()));
                if field("ports")
                    .and_then(YamlValue::as_sequence)
                    .is_some_and(|ports| !ports.is_empty())
                {
                    reasons.entry(name.to_string()).or_insert("publishes ports");
                }
                let healthcheck_disabled = field("healthcheck")
                    .and_then(|check| check.get("disable"))
                    .and_then(YamlValue::as_bool)
                    .unwrap_or(false);
                if field("healthcheck").is_some_and(YamlValue::is_mapping) && !healthcheck_disabled
                {
                    reasons
                        .entry(name.to_string())
                        .or_insert("declares a healthcheck");
                }
                if matches!(
                    field("restart").and_then(YamlValue::as_str),
                    Some("always" | "unless-stopped")
                ) {
                    reasons
                        .entry(name.to_string())
                        .or_insert("has an always/unless-stopped restart policy");
                }
                if let Some(depends_on) = field("depends_on").and_then(YamlValue::as_mapping) {
                    for (dependency, condition) in depends_on {
                        let healthy = condition.get("condition").and_then(YamlValue::as_str)
                            == Some("service_healthy");
                        if let (true, Some(dependency)) = (healthy, dependency.as_str()) {
                            reasons
                                .entry(dependency.to_string())
                                .or_insert("is a dependency other services wait on to be healthy");
                        }
                    }
                }
            }
        }
        ReadinessExpectations {
            all_long_running: false,
            long_running: reasons,
        }
    }

    /// For every `running` service that declares no Compose `healthcheck`,
    /// verify at least one of its published TCP ports actually accepts a
    /// connection. `docker compose ps` state alone can't tell "container
    /// process started" from "app is listening" — a Node/Python/etc. app
    /// commonly takes a few seconds after process start to bind its port,
    /// and without this check that gap was invisible to the deploy job.
    /// Services with no published ports (workers, internal-only sidecars)
    /// have nothing to probe and are left as-is.
    async fn unreachable_published_ports(
        entries: &[ComposePsEntry],
        host_ip: std::net::IpAddr,
    ) -> Vec<String> {
        let mut reasons = Vec::new();
        for entry in entries {
            if !entry.health.is_empty() {
                // Has a healthcheck — `classify_readiness` already required
                // it to be "healthy" before we got here.
                continue;
            }
            let tcp_ports: Vec<u16> = entry
                .publishers
                .iter()
                .filter(|publisher| {
                    publisher.published_port > 0
                        && (publisher.protocol.is_empty() || publisher.protocol == "tcp")
                })
                .map(|publisher| publisher.published_port)
                .collect();
            if tcp_ports.is_empty() {
                continue;
            }
            let mut any_reachable = false;
            for port in &tcp_ports {
                if Self::port_reachable(host_ip, *port).await {
                    any_reachable = true;
                    break;
                }
            }
            if !any_reachable {
                reasons.push(format!(
                    "service '{}' published port(s) {} not yet accepting connections",
                    entry.service,
                    tcp_ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        reasons
    }

    /// Best-effort TCP connect probe against a published host port.
    async fn port_reachable(host_ip: std::net::IpAddr, published_port: u16) -> bool {
        tokio::time::timeout(
            PORT_PROBE_CONNECT_TIMEOUT,
            TcpStream::connect((host_ip, published_port)),
        )
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false)
    }

    pub(super) async fn discover_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        let ps_entries = self
            .compose_ps(project_dir, project_name, compose_file)
            .await?;
        let mut results = Vec::new();

        for ps_entry in ps_entries {
            // Parse published ports
            let ports = self.parse_publishers(&ps_entry.publishers);

            // Resolve full container ID via Docker inspect (compose ps returns short IDs).
            // Best-effort: fall back to the short ID compose ps already gave us
            // on any inspect failure, including a missing local daemon.
            let full_id = match self.docker.get() {
                Some(docker) => match docker.inspect_container(&ps_entry.id, None).await {
                    Ok(info) => info.id.unwrap_or(ps_entry.id.clone()),
                    Err(_) => ps_entry.id.clone(),
                },
                None => ps_entry.id.clone(),
            };

            results.push(ComposeServiceResult {
                container_id: full_id,
                container_name: ps_entry.name,
                service_name: ps_entry.service,
                image_name: ps_entry.image,
                ports,
                status: ps_entry.state,
            });
        }

        debug!(
            project = %project_name,
            services = results.len(),
            "Discovered compose containers"
        );

        Ok(results)
    }

    /// Preserve the primary deployment error while best-effort discovering
    /// the candidate containers it left behind. Discovery must never replace
    /// the startup error: an empty list simply tells the caller that there is
    /// no safe live diagnostic surface to retain.
    pub(super) async fn failure_with_discovered_containers(
        &self,
        project_dir: &Path,
        project_name: &str,
        compose_file: &str,
        request: &ComposeDeployRequest,
        error: ComposeError,
    ) -> ComposeDeployFailure {
        let containers = match self
            .discover_containers(project_dir, project_name, compose_file)
            .await
        {
            Ok(containers) => containers,
            Err(discovery_error) => {
                warn!(
                    project = %project_name,
                    error = %discovery_error,
                    "Compose startup failed and candidate containers could not be discovered"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: Vec::new(),
                    retention_error: Some(discovery_error),
                };
            }
        };

        for container in &containers {
            if let Err(retention_error) = self
                .verify_labels(
                    &container.container_id,
                    &request.labels,
                    &container.service_name,
                )
                .await
            {
                warn!(
                    project = %project_name,
                    container_id = %container.container_id,
                    service = %container.service_name,
                    error = %retention_error,
                    "Failed to verify retained Compose container labels"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: Vec::new(),
                    retention_error: Some(retention_error),
                };
            }
        }

        // Only expose candidates for direct cleanup after ownership has been
        // verified for the entire stack. Port safety is checked separately so
        // an unsafe first container cannot short-circuit sibling verification.
        for container in &containers {
            if let Err(retention_error) = self
                .verify_retention_port_bindings(&container.container_id, &container.service_name)
                .await
            {
                warn!(
                    project = %project_name,
                    container_id = %container.container_id,
                    service = %container.service_name,
                    error = %retention_error,
                    "Failed Compose container has a host port that is unsafe to retain"
                );
                return ComposeDeployFailure {
                    error,
                    containers: Vec::new(),
                    cleanup_containers: containers.clone(),
                    retention_error: Some(retention_error),
                };
            }
        }

        ComposeDeployFailure {
            error,
            containers,
            cleanup_containers: Vec::new(),
            retention_error: None,
        }
    }

    fn parse_publishers(&self, publishers: &[ComposePsPublisher]) -> Vec<ComposePortBinding> {
        publishers
            .iter()
            .filter(|p| p.published_port > 0)
            .map(|p| ComposePortBinding {
                host_port: p.published_port,
                container_port: p.target_port,
                protocol: p.protocol.clone(),
            })
            .collect()
    }

    pub(super) async fn verify_labels(
        &self,
        container_id: &str,
        base_labels: &HashMap<String, String>,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let inspect = docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| ComposeError::Docker(format!("inspect failed: {}", e)))?;

        let actual_labels = inspect
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or_else(|| ComposeError::Docker(format!(
                "container {container_id} for service '{service_name}' has no labels; refusing to register an unowned Compose container"
            )))?;
        for (key, expected) in base_labels {
            let actual = actual_labels.get(key).map(String::as_str);
            if actual != Some(expected.as_str()) {
                return Err(ComposeError::Docker(format!(
                    "container {container_id} for service '{service_name}' has ownership label '{key}'={actual:?}, expected '{expected}'"
                )));
            }
        }
        if actual_labels.get("sh.temps.service").map(String::as_str) != Some(service_name) {
            return Err(ComposeError::Docker(format!(
                "container {container_id} has a mismatched sh.temps.service ownership label for Compose service '{service_name}'"
            )));
        }

        let state = inspect
            .state
            .as_ref()
            .and_then(|s| s.status.as_ref())
            .map(|s| format!("{:?}", s))
            .unwrap_or_else(|| "unknown".to_string());

        debug!(
            container_id = %container_id,
            service = %service_name,
            state = %state,
            labels = ?base_labels.keys().collect::<Vec<_>>(),
            "Verified compose container"
        );

        Ok(())
    }

    /// A retained failure must not bypass the Temps proxy through a Docker
    /// host port. Inspect Docker's configured bindings (not `compose ps`,
    /// which can omit stopped-container publishers) and admit loopback only.
    async fn verify_retention_port_bindings(
        &self,
        container_id: &str,
        service_name: &str,
    ) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let inspect = docker
            .inspect_container(container_id, None)
            .await
            .map_err(|error| {
                ComposeError::Docker(format!(
                    "failed to inspect port bindings for retained container {container_id}: {error}"
                ))
            })?;
        let Some(port_bindings) = inspect
            .host_config
            .and_then(|host_config| host_config.port_bindings)
        else {
            return Ok(());
        };

        for (container_port, bindings) in port_bindings {
            for binding in bindings.into_iter().flatten() {
                let host_ip = binding.host_ip.unwrap_or_default();
                let private = host_ip.parse::<std::net::IpAddr>().is_ok_and(|address| {
                    address.is_loopback() || address == self.published_host_ip
                });
                if !private {
                    return Err(ComposeError::Docker(format!(
                        "service '{service_name}' publishes container port {container_port} on host address '{}'; failed stacks may be retained only when every host binding is loopback or this node's private address",
                        if host_ip.is_empty() { "all interfaces" } else { &host_ip }
                    )));
                }
            }
        }
        Ok(())
    }

    /// Force-remove a set of candidate containers after re-verifying their
    /// exact Temps ownership labels. Used as the fail-closed fallback for a
    /// failed stack that is unsafe to retain (for example, it publishes a
    /// host port on all interfaces). Every candidate is attempted so one
    /// Docker error cannot leave a sibling publicly reachable.
    pub async fn remove_verified_containers(
        &self,
        containers: &[ComposeServiceResult],
        expected_labels: &HashMap<String, String>,
    ) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let mut failures = Vec::new();
        for container in containers {
            match docker
                .inspect_container(&container.container_id, None)
                .await
            {
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => continue,
                Err(error) => {
                    failures.push(format!(
                        "inspect {} (service '{}'): {error}",
                        container.container_id, container.service_name
                    ));
                    continue;
                }
                Ok(_) => {}
            }

            if let Err(error) = self
                .verify_labels(
                    &container.container_id,
                    expected_labels,
                    &container.service_name,
                )
                .await
            {
                failures.push(error.to_string());
                continue;
            }

            match docker
                .remove_container(
                    &container.container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(())
                | Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(error) => failures.push(format!(
                    "remove {} (service '{}'): {error}",
                    container.container_id, container.service_name
                )),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(ComposeError::Docker(format!(
                "failed to remove {} ownership-verified Compose candidate container(s): {}",
                failures.len(),
                failures.join("; ")
            )))
        }
    }
}

/// Which services a Compose deployment needs running to be ready.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReadinessExpectations {
    /// Every service must keep running (no service may exit, even with 0).
    all_long_running: bool,
    /// Services that must keep running, with the reason shown when one exits.
    long_running: HashMap<String, &'static str>,
}

impl ReadinessExpectations {
    fn all_long_running() -> Self {
        Self {
            all_long_running: true,
            long_running: HashMap::new(),
        }
    }

    /// Why `service` must keep running, or `None` when a clean exit is a
    /// completed one-shot task.
    pub(crate) fn why_long_running(&self, service: &str) -> Option<&'static str> {
        if self.all_long_running {
            return Some("is required to keep running");
        }
        self.long_running.get(service).copied()
    }
}

/// Result of classifying a `docker compose ps` snapshot for readiness.
#[derive(Debug, PartialEq)]
enum ComposeReadiness {
    Ready,
    /// Still starting; each entry is a human-readable reason, e.g.
    /// `"service 'web' is starting"`.
    Pending(Vec<String>),
    /// Reached a terminal failure state (`exited`, `dead`, `unhealthy`).
    Failed(Vec<String>),
}

/// JSON output from `docker compose ps --format json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ComposePsEntry {
    #[serde(alias = "ID")]
    id: String,
    name: String,
    service: String,
    image: String,
    state: String,
    /// "healthy"/"unhealthy"/"starting", or "" when the service defines no
    /// healthcheck (older Compose CLI versions omit the field entirely).
    #[serde(default)]
    health: String,
    /// Process exit status for an `exited` container (`0` otherwise).
    #[serde(default)]
    exit_code: i64,
    #[serde(default)]
    publishers: Vec<ComposePsPublisher>,
}

/// Port publisher from `docker compose ps --format json`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ComposePsPublisher {
    #[serde(default)]
    published_port: u16,
    #[serde(default)]
    target_port: u16,
    #[serde(default)]
    protocol: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::files::EnvFileSource;
    use crate::compose::test_support::{
        deploy_request, disabled_executor, executor_with_checks_disabled, LOOPBACK,
    };
    use crate::compose::{ComposeExecutor, ELEVATED_PERMISSIONS_SETTINGS_PATH};
    use bollard::Docker;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn test_parse_compose_ps_json() {
        let json = r#"{"ID":"abc123","Name":"myapp-web-1","Service":"web","Image":"nginx:latest","State":"running","Publishers":[{"URL":"0.0.0.0","TargetPort":80,"PublishedPort":8080,"Protocol":"tcp"}]}"#;

        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.service, "web");
        assert_eq!(entry.state, "running");
        assert_eq!(entry.health, "");
        assert_eq!(entry.publishers.len(), 1);
        assert_eq!(entry.publishers[0].published_port, 8080);
        assert_eq!(entry.publishers[0].target_port, 80);
    }

    #[test]
    fn test_parse_compose_ps_json_with_health() {
        let json = r#"{"ID":"abc123","Name":"myapp-web-1","Service":"web","Image":"nginx:latest","State":"running","Health":"healthy","Publishers":[]}"#;
        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.health, "healthy");
    }

    fn entry(service: &str, state: &str, health: &str) -> ComposePsEntry {
        ComposePsEntry {
            id: format!("{service}-id"),
            name: format!("{service}-1"),
            service: service.to_string(),
            image: "img:latest".to_string(),
            state: state.to_string(),
            health: health.to_string(),
            exit_code: 0,
            publishers: Vec::new(),
        }
    }

    #[test]
    fn classify_readiness_no_containers_is_pending() {
        let result = ComposeExecutor::classify_readiness(&[]);
        assert!(matches!(result, ComposeReadiness::Pending(_)));
    }

    #[test]
    fn classify_readiness_running_no_healthcheck_is_ready() {
        let entries = [entry("web", "running", "")];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    #[test]
    fn classify_readiness_running_healthy_is_ready() {
        let entries = [entry("web", "running", "healthy")];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    #[test]
    fn classify_readiness_running_starting_health_is_pending() {
        let entries = [entry("web", "running", "starting")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Pending(_)
        ));
    }

    #[test]
    fn classify_readiness_created_is_pending() {
        let entries = [entry("web", "created", "")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Pending(_)
        ));
    }

    #[test]
    fn classify_readiness_unhealthy_fails_fast() {
        let entries = [entry("web", "running", "unhealthy")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_exited_fails_fast() {
        let entries = [entry("web", "exited", "")];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_one_failed_service_fails_whole_stack() {
        let entries = [
            entry("web", "running", "healthy"),
            entry("db", "exited", ""),
        ];
        assert!(matches!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Failed(_)
        ));
    }

    #[test]
    fn classify_readiness_all_services_must_be_ready() {
        let entries = [
            entry("web", "running", "healthy"),
            entry("worker", "running", ""),
        ];
        assert_eq!(
            ComposeExecutor::classify_readiness(&entries),
            ComposeReadiness::Ready
        );
    }

    fn entry_with_publisher(service: &str, published_port: u16) -> ComposePsEntry {
        let mut e = entry(service, "running", "");
        e.publishers.push(ComposePsPublisher {
            published_port,
            target_port: published_port,
            protocol: "tcp".to_string(),
        });
        e
    }

    #[tokio::test]
    async fn unreachable_published_ports_no_healthcheck_and_no_listener_is_unreachable() {
        // Nothing is bound to this port, so the connect attempt must fail —
        // reproducing a service whose process started but whose app hasn't
        // opened its port yet.
        let entries = [entry_with_publisher("web", 18291)];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries, LOOPBACK).await;
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("web"));
        assert!(reasons[0].contains("18291"));
    }

    #[tokio::test]
    async fn unreachable_published_ports_no_healthcheck_and_listening_is_reachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let entries = [entry_with_publisher("web", port)];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries, LOOPBACK).await;
        assert!(reasons.is_empty());
    }

    #[tokio::test]
    async fn unreachable_published_ports_with_healthcheck_is_skipped() {
        // classify_readiness already required "healthy" for this service to
        // reach the caller, so the port probe must not second-guess it even
        // though nothing is listening on the (fake) published port here.
        let mut e = entry("web", "running", "healthy");
        e.publishers.push(ComposePsPublisher {
            published_port: 18292,
            target_port: 18292,
            protocol: "tcp".to_string(),
        });
        let reasons = ComposeExecutor::unreachable_published_ports(&[e], LOOPBACK).await;
        assert!(reasons.is_empty());
    }

    #[tokio::test]
    async fn unreachable_published_ports_with_no_published_ports_is_skipped() {
        // A worker/internal-only service has nothing to probe.
        let entries = [entry("worker", "running", "")];
        let reasons = ComposeExecutor::unreachable_published_ports(&entries, LOOPBACK).await;
        assert!(reasons.is_empty());
    }

    #[test]
    fn test_parse_compose_ps_no_ports() {
        let json = r#"{"ID":"def456","Name":"myapp-redis-1","Service":"redis","Image":"redis:7","State":"running","Publishers":[]}"#;

        let entry: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.service, "redis");
        assert!(entry.publishers.is_empty());
    }

    #[test]
    fn test_parse_publishers() {
        let docker = Docker::connect_with_defaults();
        if docker.is_err() {
            // Can still test parse_publishers without Docker
            return;
        }
        let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

        let publishers = vec![
            ComposePsPublisher {
                published_port: 8080,
                target_port: 80,
                protocol: "tcp".to_string(),
            },
            ComposePsPublisher {
                published_port: 0, // Not published
                target_port: 6379,
                protocol: "tcp".to_string(),
            },
        ];

        let ports = executor.parse_publishers(&publishers);
        assert_eq!(ports.len(), 1); // Only the published port
        assert_eq!(ports[0].host_port, 8080);
        assert_eq!(ports[0].container_port, 80);
    }

    #[test]
    fn test_capability_denial_remediation_names_the_service_and_the_escape_hatch() {
        let remediation = ComposeExecutor::capability_denial_remediation(&["postgres".to_string()]);

        assert!(remediation.contains("'postgres'"), "{remediation}");
        assert!(
            remediation.contains(ELEVATED_PERMISSIONS_SETTINGS_PATH),
            "{remediation}"
        );
        assert!(remediation.contains("SETUID"), "{remediation}");
        // The default-granted set means "Disable sandbox" is now the escape
        // hatch — there is no "Elevated permissions" toggle left to flip.
        assert!(remediation.contains("Disable sandbox"), "{remediation}");
    }

    #[test]
    fn test_capability_denial_remediation_is_empty_without_a_denial() {
        assert!(ComposeExecutor::capability_denial_remediation(&[]).is_empty());
    }

    #[tokio::test]
    async fn disabled_handle_container_log_tail_is_a_placeholder_not_a_panic() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let executor = disabled_executor(tmp.path().to_path_buf());

        // Best-effort diagnostic helper: never a Result, so absence of a
        // daemon must degrade to a placeholder string instead of touching
        // `self.docker` unconditionally (which would panic-free but still
        // attempt a bollard call against nothing).
        let logs = executor
            .container_log_tail("nonexistent-container-id")
            .await;
        assert!(
            logs.contains("Docker daemon unavailable"),
            "logs were: {logs}"
        );
    }

    fn ps(service: &str, state: &str, exit_code: i64) -> ComposePsEntry {
        let mut ps_entry = entry(service, state, "");
        ps_entry.exit_code = exit_code;
        ps_entry
    }

    #[test]
    fn one_shot_service_that_exits_zero_completes_instead_of_failing() {
        let compose = r#"
services:
  migrate:
    image: app:latest
    command: ["./migrate"]
  web:
    image: app:latest
    ports: ["127.0.0.1::3000"]
    depends_on:
      migrate:
        condition: service_completed_successfully
"#;
        let expectations = ComposeExecutor::readiness_expectations(compose, None, &[]);
        assert_eq!(
            ComposeExecutor::classify_readiness_with(
                &[ps("migrate", "exited", 0), ps("web", "running", 0)],
                &expectations
            ),
            ComposeReadiness::Ready
        );

        // A failing migration still fails fast, with its status.
        let ComposeReadiness::Failed(reasons) = ComposeExecutor::classify_readiness_with(
            &[ps("migrate", "exited", 1), ps("web", "running", 0)],
            &expectations,
        ) else {
            panic!("non-zero exit must fail");
        };
        assert_eq!(reasons, vec!["service 'migrate' exited (code 1)"]);
    }

    #[test]
    fn services_that_must_keep_running_fail_even_on_a_clean_exit() {
        let compose = r#"
services:
  api:
    image: app:latest
    ports: ["127.0.0.1::8080"]
  db:
    image: postgres:18
    healthcheck:
      test: ["CMD", "pg_isready"]
  cache:
    image: redis:7
    restart: unless-stopped
  queue:
    image: queue:latest
  worker:
    image: app:latest
    depends_on:
      queue:
        condition: service_healthy
  router:
    image: app:latest
"#;
        let expectations =
            ComposeExecutor::readiness_expectations(compose, None, &["router".to_string()]);
        for (service, reason) in [
            ("api", "publishes ports"),
            ("db", "declares a healthcheck"),
            ("cache", "restart policy"),
            ("queue", "wait on to be healthy"),
            ("router", "public traffic"),
        ] {
            let ComposeReadiness::Failed(reasons) = ComposeExecutor::classify_readiness_with(
                &[ps(service, "exited", 0), ps("worker", "running", 0)],
                &expectations,
            ) else {
                panic!("{service} exiting must fail the deploy");
            };
            assert!(reasons[0].contains(&format!("service '{service}' exited (code 0)")));
            assert!(reasons[0].contains(reason), "{service}: {reasons:?}");
        }
        // The worker itself is a plain service: a clean exit is a completion.
        assert_eq!(
            ComposeExecutor::classify_readiness_with(
                &[ps("worker", "exited", 0), ps("api", "running", 0)],
                &expectations
            ),
            ComposeReadiness::Ready
        );
    }

    #[test]
    fn a_stack_with_nothing_left_running_is_not_ready() {
        let expectations = ComposeExecutor::readiness_expectations(
            "services:\n  task:\n    image: busybox\n",
            None,
            &[],
        );
        assert!(matches!(
            ComposeExecutor::classify_readiness_with(&[ps("task", "exited", 0)], &expectations),
            ComposeReadiness::Failed(_)
        ));
        // Unparsable input keeps the strict behaviour.
        let strict = ComposeExecutor::readiness_expectations("{{{", None, &[]);
        assert!(matches!(
            ComposeExecutor::classify_readiness_with(
                &[ps("task", "exited", 0), ps("web", "running", 0)],
                &strict
            ),
            ComposeReadiness::Failed(_)
        ));
        // Ports added by the inline override count too.
        let with_override = ComposeExecutor::readiness_expectations(
            "services:\n  web:\n    image: app\n",
            Some("services:\n  web:\n    ports: ['127.0.0.1::80']\n"),
            &[],
        );
        assert!(with_override.why_long_running("web").is_some());
    }

    #[test]
    fn compose_ps_exit_code_is_parsed() {
        let json = r#"{"ID":"abc","Name":"temps-1-2-migrate-1","Service":"migrate","Image":"app","State":"exited","ExitCode":3,"Publishers":[]}"#;
        let parsed: ComposePsEntry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.exit_code, 3);
        let running = r#"{"ID":"abc","Name":"n","Service":"web","Image":"app","State":"running"}"#;
        assert_eq!(
            serde_json::from_str::<ComposePsEntry>(running)
                .unwrap()
                .exit_code,
            0
        );
    }

    /// Scenario: the generic `sample-compose-app` fixture (compose file in a
    /// subdirectory, anonymous volume, one-shot migration, shared env file,
    /// loopback-published web service) passes policy, gets consistent
    /// command inputs and env files, and is ready once the migration exits 0.
    #[tokio::test]
    async fn sample_compose_app_scenario_is_deployable() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-compose-app");
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join("deploy")).unwrap();
        for file in ["deploy/compose.yaml", "deploy/app.env"] {
            std::fs::copy(fixture.join(file), checkout.path().join(file)).unwrap();
        }
        let compose_file = "deploy/compose.yaml";
        let compose = std::fs::read_to_string(checkout.path().join(compose_file)).unwrap();

        let executor = executor_with_checks_disabled(&[]);
        executor.preflight_validate(&compose, None).unwrap();
        executor
            .preflight_validate_filesystem(checkout.path(), compose_file, &compose, None)
            .unwrap();

        let plans = ComposeExecutor::plan_env_files_for_compose_file(
            &compose,
            compose_file,
            Some(checkout.path()),
        );
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].path, "deploy/app.env");
        assert!(matches!(plans[0].source, EnvFileSource::Repository(_)));

        let mut request = deploy_request(&compose, compose_file);
        request.repo_dir = Some(checkout.path().to_path_buf());
        request.environment_vars = HashMap::from([("GREETING".into(), "from-temps".into())]);
        request.env_content = Some("GREETING=from-repository\n".into());
        request.routed_services = vec!["web".into()];
        let data_dir = tempfile::tempdir().unwrap();
        let writer = disabled_executor(data_dir.path().to_path_buf());
        writer
            .write_compose_files(checkout.path(), &request, "gen-1")
            .await
            .unwrap();
        let args = ComposeExecutor::compose_input_args(checkout.path(), compose_file);
        assert_eq!(&args[..2], ["-f", compose_file]);
        let env_files: Vec<&String> = args
            .windows(2)
            .filter(|pair| pair[0] == "--env-file")
            .map(|pair| &pair[1])
            .collect();
        assert_eq!(env_files, [".env", ".env.temps"]);

        let expectations = ComposeExecutor::readiness_expectations(
            &request.compose_content,
            None,
            &request.routed_services,
        );
        assert_eq!(
            ComposeExecutor::classify_readiness_with(
                &[ps("migrate", "exited", 0), ps("web", "running", 0)],
                &expectations
            ),
            ComposeReadiness::Ready
        );
        assert!(matches!(
            ComposeExecutor::classify_readiness_with(
                &[ps("migrate", "exited", 0), ps("web", "exited", 0)],
                &expectations
            ),
            ComposeReadiness::Failed(_)
        ));

        // With the Compose CLI available, confirm Compose itself resolves the
        // model the same way: `${GREETING:?}` is satisfied by the Temps value
        // (which wins over the repository .env), and both env files load.
        let compose_cli = tokio::process::Command::new("docker")
            .args(["compose", "version"])
            .output()
            .await
            .is_ok_and(|output| output.status.success());
        if !compose_cli {
            println!("Docker Compose CLI not available; skipping `docker compose config` check");
            return;
        }
        let output = tokio::process::Command::new("docker")
            .args(["compose", "-p", "temps-scenario-test"])
            .args(&args)
            .args(["config", "--format", "json"])
            .current_dir(checkout.path())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "docker compose config failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let model: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let web_env = &model["services"]["web"]["environment"];
        assert_eq!(web_env["GREETING"], "from-temps", "{web_env}");
        assert_eq!(web_env["APP_NAME"], "sample-compose-app", "{web_env}");
    }

    // ---- Standard Compose idioms, resolution and diagnostics ----
}
