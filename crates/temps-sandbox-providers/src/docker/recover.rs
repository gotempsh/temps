// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recovering sandboxes after a restart: finding an existing container,
//! checking its policy, and quarantining stale or mismatched ones.

use super::*;

impl DockerSandboxProvider {
    /// Stop every running managed sandbox whose immutable container config
    /// predates the current isolation policy. This runs during provider
    /// startup, before any DB row or in-memory handle can make the container
    /// reachable again. Recreation is intentionally deferred until the user
    /// accesses that sandbox so persistent volumes remain untouched.
    pub async fn quarantine_stale_sandboxes(&self) -> Result<(), AgentError> {
        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "label".to_string(),
                    vec![format!("{SANDBOX_CONTAINER_LABEL}=true")],
                )])),
                ..Default::default()
            }))
            .await
            .map_err(|source| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("list managed sandboxes for isolation validation: {source}"),
            })?;
        for summary in containers {
            let Some(id) = summary.id.as_deref() else {
                continue;
            };
            let container_name = summary
                .names
                .as_ref()
                .and_then(|names| names.first())
                .map(|name| name.trim_start_matches('/'))
                .unwrap_or(id);
            let info = self
                .docker
                .inspect_container(
                    id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .map_err(|source| AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "inspect managed sandbox '{container_name}' during isolation validation: {source}"
                    ),
                })?;
            if !recovered_container_matches_egress_policy(&info, container_name) {
                self.quarantine_container(id, container_name).await?;
            }
        }
        // Networks of sandboxes removed while Temps was down (or whose
        // teardown failed), and of sandboxes that are stopped, would
        // otherwise keep their subnets until Docker runs out of address
        // pools.
        let gateway = self
            .config
            .preview_gateway_container_name
            .read()
            .await
            .clone();
        let reaped = self
            .reap_orphaned_sandbox_networks(&gateway, ORPHANED_NETWORK_MIN_AGE)
            .await;
        if reaped > 0 {
            tracing::info!(
                networks = reaped,
                "Released isolated networks of sandboxes that no longer exist"
            );
        }
        Ok(())
    }

    pub(super) async fn quarantine_container(
        &self,
        container_reference: &str,
        container_name: &str,
    ) -> Result<(), AgentError> {
        tracing::warn!(
            container = container_name,
            "Stopping sandbox because its network policy is stale or invalid"
        );
        match self
            .docker
            .stop_container(
                container_reference,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(0),
                    signal: None,
                }),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if docker_error_is_not_found(&error) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304,
                ..
            }) => Ok(()),
            Err(source) => Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "stop sandbox '{container_name}' after its isolation policy failed validation: {source}"
                ),
            }),
        }
    }

    pub(super) async fn inspect_handle_policy(
        &self,
        handle: &SandboxHandle,
    ) -> Result<bollard::models::ContainerInspectResponse, AgentError> {
        let info = self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|source| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("inspect sandbox isolation policy: {source}"),
            })?;
        if recovered_container_matches_egress_policy(&info, &handle.sandbox_name) {
            return Ok(info);
        }
        self.quarantine_container(&handle.sandbox_id, &handle.sandbox_name)
            .await?;
        Err(AgentError::SandboxProviderUnavailable {
            provider: "docker".to_string(),
            reason: format!(
                "sandbox '{}' was safely stopped because its network isolation policy is stale; retry to recreate it automatically",
                handle.sandbox_name
            ),
        })
    }

    /// Shared recovery by absolute container name — looks up the container
    /// and returns a handle for it regardless of whether it's currently
    /// running or stopped. Only returns `None` when the container has been
    /// removed at the Docker level.
    ///
    /// IMPORTANT: this function must never delete a container. Stopped is a
    /// legitimate long-lived state for standalone sandboxes — the
    /// expiration sweeper parks expired sandboxes there, and the user
    /// resumes them later. An earlier version of this function auto-removed
    /// stopped containers on the assumption that "stopped" meant "leaked
    /// leftover"; that destroyed sandbox filesystems + volumes on every
    /// server restart that happened between stop and resume. Callers who
    /// genuinely want to destroy must go through the explicit `destroy`
    /// path, which is the only code that should call `remove_container`.
    ///
    /// Used by both `recover(run_id)` (numeric naming for agent runs /
    /// workspace sessions) and `recover_by_name(id)` (public_id naming for
    /// standalone sandboxes).
    pub(super) async fn recover_container(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        match self
            .docker
            .inspect_container(
                container_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => {
                if !recovered_container_matches_egress_policy(&info, container_name) {
                    tracing::warn!(
                        container = container_name,
                        "Refusing to recover sandbox that predates the managed egress policy; it will be recreated with its persistent volumes"
                    );
                    let container_reference = info.id.as_deref().unwrap_or(container_name);
                    self.quarantine_container(container_reference, container_name)
                        .await?;
                    return Ok(None);
                }
                let network_mode = info
                    .host_config
                    .as_ref()
                    .and_then(|config| config.network_mode.as_deref());
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                if network_mode != Some("none") && running {
                    // Recovery must reconcile sidecars too. The sandbox
                    // container's immutable proxy variables can still match
                    // while the egress sidecar predates a stricter routing
                    // policy; `ensure_network` replaces that drifted sidecar
                    // without touching the persistent workspace volumes.
                    // A stopped sandbox is reconciled by `start` instead, so
                    // recovering it does not take back the subnet it gave up.
                    self.ensure_network(
                        container_name,
                        &sandbox_network_name(container_name),
                        &self.config.control_plane_url,
                        None,
                    )
                    .await?;
                }
                let container_id = info.id.unwrap_or_default();
                tracing::info!("Recovered sandbox {} (running={})", container_name, running);

                // Re-run ownership normalization on the recovered container.
                // Without this, any container created before the original
                // chown landed (or whose host workspace was rewritten by a
                // later host-side git operation) stays root-owned, and the
                // sandbox user gets "Permission denied" on every write into
                // /home/temps/workspace.
                //
                // Best-effort if the container isn't running: starting it
                // here would change recovery semantics, so we just log and
                // skip — the next create()/start path will fix it.
                if running && !container_id.is_empty() {
                    // run_id is only used for error context; we don't have
                    // it on this path, so pass 0.
                    if let Err(e) = self.normalize_ownership(&container_id, 0).await {
                        tracing::warn!(
                            "Sandbox {} recovered but ownership normalization failed: {} \
                             — workspace writes may still fail",
                            container_name,
                            e
                        );
                    }
                }

                Ok(Some(SandboxHandle {
                    node_id: None,
                    sandbox_id: container_id,
                    sandbox_name: container_name.to_string(),
                    work_dir: PathBuf::from(CONTAINER_WORK_DIR),
                    backend: super::SandboxBackend::Docker,
                    image: String::new(),
                    conditions: Vec::new(),
                }))
            }
            Err(error) if docker_error_is_not_found(&error) => Ok(None),
            // Anything but "no such container" (socket down, daemon
            // restarting, timeout) says nothing about whether the container
            // exists. `Ok(None)` is read as "gone" by callers that then
            // rewrite the sandbox row, so it must stay an error.
            Err(error) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: container_name.to_string(),
                reason: format!("Failed to inspect container {container_name}: {error}"),
            }),
        }
    }
}
