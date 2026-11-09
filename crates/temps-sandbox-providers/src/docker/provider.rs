// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `SandboxProvider` implementation for Docker containers.

use super::*;

#[async_trait]
impl SandboxProvider for DockerSandboxProvider {
    async fn service_access(
        &self,
        handle: &SandboxHandle,
        command: crate::service_access::ServiceAccessCommand,
    ) -> Result<crate::service_access::ServiceAccessReply, AgentError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.service_access_command(handle, command),
        )
        .await
        .map_err(|_| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: "service access command exceeded its 20-second deadline".into(),
        })?
    }
    async fn image_identity(&self, handle: &SandboxHandle) -> Result<String, AgentError> {
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
                reason: format!("inspect immutable container image identity: {source}"),
            })?;
        info.image
            .filter(|id| {
                id.strip_prefix("sha256:").is_some_and(|digest| {
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .ok_or_else(|| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "container has no immutable sha256 image identity".into(),
            })
    }
    async fn check_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<RuntimeCompatibility, AgentError> {
        let config = bollard::models::ExecConfig {
            user: Some(format!("{SANDBOX_UID}:{SANDBOX_GID}")),
            cmd: Some(vec![
                "/usr/local/bin/temps-sandbox-runtime".into(),
                "check".into(),
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.run_exec_config(&handle.sandbox_id, 0, "runtime-compatibility", config),
        )
        .await
        {
            Ok(Ok(0)) => {
                // The SDK socket can be healthy on an older image that cannot
                // safely fence orphaned harness turns after a backend restart.
                let health = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.run_exec_config_captured(
                        &handle.sandbox_id,
                        0,
                        "runtime-health-capabilities",
                        runtime_health_exec_config(),
                    ),
                )
                .await;
                match health {
                    Ok(Ok((0, output))) if has_harness_recovery_capability(&output) => {
                        Ok(RuntimeCompatibility::Compatible)
                    }
                    Ok(Ok((0, _))) => Ok(RuntimeCompatibility::Incompatible {
                        reason: "runtime health does not advertise recover_harness; update the workspace runtime".into(),
                    }),
                    Ok(Ok((code, _))) => Ok(RuntimeCompatibility::Unavailable {
                        reason: format!("runtime health probe exited {code}"),
                    }),
                    Ok(Err(error)) => Ok(RuntimeCompatibility::Unavailable {
                        reason: format!("runtime health probe failed: {error}"),
                    }),
                    Err(_) => Ok(RuntimeCompatibility::Unavailable {
                        reason: "runtime health probe timed out after 5 seconds".into(),
                    }),
                }
            }
            Ok(Ok(2)) => Ok(RuntimeCompatibility::Unavailable {
                reason: "retained agent runtime socket is unavailable or probe timed out".into(),
            }),
            Ok(Ok(code)) => Ok(RuntimeCompatibility::Incompatible {
                reason: format!(
                    "runtime SDK protocol probe exited {code}; image may have an older runtime"
                ),
            }),
            Ok(Err(error)) => Ok(RuntimeCompatibility::Unavailable {
                reason: error.to_string(),
            }),
            Err(_) => Ok(RuntimeCompatibility::Unavailable {
                reason: "runtime SDK protocol probe timed out after 5 seconds".into(),
            }),
        }
    }
    async fn recover_agent_harness(
        &self,
        handle: &SandboxHandle,
        epoch: u64,
    ) -> Result<(), AgentError> {
        let config = bollard::models::ExecConfig {
            // Only a host-issued root Docker exec may reclaim daemon turns.
            // Ordinary harness processes run as SANDBOX_UID.
            user: Some("0:0".into()),
            cmd: Some(vec![
                "/usr/local/bin/temps-sandbox-runtime".into(),
                "recover-harness".into(),
                epoch.to_string(),
            ]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.run_exec_config(&handle.sandbox_id, 0, "recover-harness", config),
        ).await {
            Ok(Ok(0)) => Ok(()),
            Ok(Ok(code)) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("harness recovery exited {code}; update the workspace runtime if its image lacks recover-harness"),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "harness recovery timed out before provider termination could be confirmed".into(),
            }),
        }
    }
    async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
        self.create_inner(config).await
    }

    async fn model_relay_base_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
    ) -> Result<String, AgentError> {
        self.ensure_network(
            &handle.sandbox_name,
            &sandbox_network_name(&handle.sandbox_name),
            control_plane_url,
            None,
        )
        .await?;
        Ok(SANDBOX_MODEL_RELAY_BASE_URL.to_string())
    }

    async fn git_relay_base_url(
        &self,
        handle: &SandboxHandle,
        _control_plane_url: &str,
    ) -> Result<String, AgentError> {
        Err(AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: "Git relay is disabled until sidecar capability authentication is implemented"
                .to_string(),
        })
    }

    async fn harness_mcp_url(
        &self,
        handle: &SandboxHandle,
        control_plane_url: &str,
        registered_url: &str,
    ) -> Result<String, AgentError> {
        self.ensure_network(
            &handle.sandbox_name,
            &sandbox_network_name(&handle.sandbox_name),
            control_plane_url,
            None,
        )
        .await?;
        sandbox_mcp_relay_url(control_plane_url, registered_url).ok_or_else(|| {
            AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "turn-scoped MCP URL does not match the configured control-plane capability route"
                    .to_string(),
            }
        })
    }

    async fn configure_application_network(
        &self,
        handle: &SandboxHandle,
        network_name: &str,
        service_containers: &[String],
    ) -> Result<(), AgentError> {
        if network_name.is_empty()
            || network_name.len() > 63
            || !network_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: "invalid application network name".to_string(),
            });
        }

        let networks = self
            .docker
            .list_networks(None::<bollard::query_parameters::ListNetworksOptions>)
            .await
            .map_err(|error| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("list application networks: {error}"),
            })?;
        if !networks
            .iter()
            .any(|network| network.name.as_deref() == Some(network_name))
        {
            self.docker
                .create_network(bollard::models::NetworkCreateRequest {
                    name: network_name.to_string(),
                    driver: Some("bridge".to_string()),
                    internal: Some(true),
                    enable_ipv6: Some(false),
                    labels: Some(HashMap::from([(
                        "sh.temps.sandbox-data-network".to_string(),
                        "true".to_string(),
                    )])),
                    ..Default::default()
                })
                .await
                .map_err(|error| AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!("create application network '{network_name}': {error}"),
                })?;
        }

        // Revoke stale membership before granting new membership. If a later
        // connect fails, the network may be narrower than desired but can
        // never retain access that the committed topology removed.
        let network = self
            .docker
            .inspect_network(
                network_name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|error| AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("inspect application network '{network_name}': {error}"),
            })?;
        if !docker_network_matches_policy(&network, true, true) {
            return Err(AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!(
                    "existing application network '{network_name}' does not match the required managed, internal, IPv6-disabled bridge policy"
                ),
            });
        }
        let allowed_names = service_containers
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let network_containers = network.containers.unwrap_or_default();
        let sandbox_endpoint = network_containers
            .iter()
            .find_map(|(container_id, endpoint)| {
                let is_sandbox = container_id == &handle.sandbox_id
                    || endpoint.name.as_deref() == Some(handle.sandbox_name.as_str());
                is_sandbox.then(|| container_id.clone())
            });

        // Disconnect application compute on every reconciliation, including
        // topology narrowing. Removing only a database endpoint can leave an
        // already-open TCP socket usable; removing the sandbox endpoint tears
        // down those connections before the new allow-list is installed.
        if let Some(container_id) = sandbox_endpoint {
            self.docker
                .disconnect_network(
                    network_name,
                    bollard::models::NetworkDisconnectRequest {
                        container: container_id,
                        force: Some(true),
                    },
                )
                .await
                .map_err(|error| AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: format!(
                        "detach application sandbox from network '{network_name}' before reconciliation: {error}"
                    ),
                })?;
        }

        for (container_id, endpoint) in network_containers {
            let is_sandbox = container_id == handle.sandbox_id
                || endpoint.name.as_deref() == Some(handle.sandbox_name.as_str());
            if is_sandbox {
                continue;
            }
            let allowed = endpoint
                .name
                .as_deref()
                .is_some_and(|name| allowed_names.contains(name));
            if allowed {
                continue;
            }
            self.docker
                .disconnect_network(
                    network_name,
                    bollard::models::NetworkDisconnectRequest {
                        container: container_id.clone(),
                        force: Some(true),
                    },
                )
                .await
                .map_err(|error| AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: format!(
                        "detach stale container '{container_id}' from application network '{network_name}': {error}"
                    ),
                })?;
        }

        for container in
            application_network_connection_order(&handle.sandbox_id, service_containers)
        {
            let request = bollard::models::NetworkConnectRequest {
                container: container.clone(),
                endpoint_config: None,
            };
            if let Err(error) = self.docker.connect_network(network_name, request).await {
                // Docker reports a conflict when the endpoint is already
                // attached. Treat only that idempotent case as success.
                let message = error.to_string();
                if !message.contains("already exists") && !message.contains("already connected") {
                    return Err(AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!(
                            "attach container '{container}' to application network '{network_name}': {message}"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    async fn connect_agent_runtime(
        &self,
        handle: &SandboxHandle,
    ) -> Result<PtyAttachment, AgentError> {
        let failure = |operation: &str, error: String| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!("retained agent runtime {operation} failed: {error}"),
        };
        let exec = self
            .docker
            .create_exec(&handle.sandbox_id, agent_runtime_exec_config())
            .await
            .map_err(|error| failure("create connection", error.to_string()))?;
        let attached = self
            .docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    tty: false,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| failure("open connection", error.to_string()))?;
        match attached {
            StartExecResults::Attached { output, input } => {
                let sandbox_id = handle.sandbox_id.clone();
                let output = output.filter_map(move |item| {
                    let sandbox_id = sandbox_id.clone();
                    async move {
                        match item {
                            Ok(LogOutput::StdOut { message }) => Some(Ok(message)),
                            // Never mix diagnostics into binary protocol frames or expose their payload.
                            Ok(LogOutput::StdErr { .. }) | Err(_) => Some(Err(AgentError::SandboxExecFailed {
                                run_id: 0, sandbox_id,
                                reason: "retained agent runtime connection failed; verify the sandbox image supports the agent daemon protocol".into(),
                            })),
                            Ok(_) => None,
                        }
                    }
                });
                Ok(PtyAttachment {
                    output: Box::pin(output),
                    input,
                })
            }
            StartExecResults::Detached => Err(failure(
                "open connection",
                "Docker did not attach the protocol streams".into(),
            )),
        }
    }

    async fn exec(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        // Wrap the legacy stdout-only callback into a stream-aware one that
        // discards stderr events, then delegate to the unified inner impl.
        // Keeping one implementation avoids drift between `exec` and
        // `exec_streamed`.
        let stream_cb: Option<OnStreamEventCallback> = on_output.map(|cb| {
            let cb = cb.clone();
            let f: OnStreamEventCallback =
                std::sync::Arc::new(move |stream: ExecStream, line: String| {
                    let cb = cb.clone();
                    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                        Box::pin(async move {
                            if matches!(stream, ExecStream::Stdout) {
                                cb(line).await;
                            }
                        });
                    fut
                });
            f
        });
        self.exec_inner(handle, cmd, env, stream_cb, None).await
    }

    async fn exec_as_root(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_as_user(handle, "0:0", cmd, env, on_output).await
    }

    async fn exec_as_user(
        &self,
        handle: &SandboxHandle,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        let stream_cb: Option<OnStreamEventCallback> = on_output.map(|cb| {
            let cb = cb.clone();
            let f: OnStreamEventCallback =
                std::sync::Arc::new(move |stream: ExecStream, line: String| {
                    let cb = cb.clone();
                    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                        Box::pin(async move {
                            if matches!(stream, ExecStream::Stdout) {
                                cb(line).await;
                            }
                        });
                    fut
                });
            f
        });
        self.exec_inner(handle, cmd, env, stream_cb, Some(user.to_string()))
            .await
    }

    async fn exec_streamed(
        &self,
        handle: &SandboxHandle,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_event: Option<OnStreamEventCallback>,
    ) -> Result<SandboxExecResult, AgentError> {
        self.exec_inner(handle, cmd, env, on_event, None).await
    }

    async fn is_alive(&self, handle: &SandboxHandle) -> Result<bool, AgentError> {
        match self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => {
                let running = info
                    .state
                    .as_ref()
                    .and_then(|state| state.running)
                    .unwrap_or(false);
                if recovered_container_matches_egress_policy(&info, &handle.sandbox_name) {
                    return Ok(running);
                }
                self.quarantine_container(&handle.sandbox_id, &handle.sandbox_name)
                    .await?;
                Ok(false)
            }
            Err(error) => inspect_failure_verdict(handle, error),
        }
    }

    async fn destroy(&self, handle: &SandboxHandle, purge_volumes: bool) -> Result<(), AgentError> {
        let _access_operation = self
            .service_access
            .as_ref()
            .map(|mounts| mounts.operation(&handle.sandbox_id))
            .transpose()?;
        tracing::info!(
            "Destroying sandbox container {} ({})",
            handle.sandbox_name,
            &handle.sandbox_id[..std::cmp::min(12, handle.sandbox_id.len())]
        );

        let access_mount = self.close_service_access_for_destroy(handle).await?;
        // Stop gracefully (5s timeout), then force remove
        let _ = self
            .docker
            .stop_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(5),
                    signal: None,
                }),
            )
            .await;

        match self
            .docker
            .remove_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(_) => {}
            Err(e) if docker_error_is_not_found(&e) => {
                // An operator may have removed the container directly while
                // the registry still has its handle. Destroy is cleanup and
                // must be idempotent so rebuild can recreate compute over the
                // surviving application workspace and home volume.
                tracing::debug!(
                    sandbox_id = %handle.sandbox_id,
                    "sandbox container already absent during destroy"
                );
            }
            Err(e) => {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: format!("Failed to remove container: {}", e),
                });
            }
        }

        if let (Some(mounts), Some(id)) = (&self.service_access, access_mount) {
            mounts.remove_detached(&id)?;
        }
        let _ = self
            .remove_isolated_sandbox_network(&handle.sandbox_name)
            .await;

        // Only remove the named home volume when the caller asks for a
        // full purge (session *delete*, or ephemeral agent runs). On a
        // plain session *close* the volume must survive so claude auth,
        // shell history, and ~/.claude/projects are preserved when the
        // session is reopened.
        if purge_volumes {
            let home_volume_name = Self::home_volume_name(&handle.sandbox_name);
            if let Err(e) = self
                .docker
                .remove_volume(
                    &home_volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                // Not fatal — the container is already gone, so the sandbox
                // is destroyed either way. Nothing sweeps up afterwards, so
                // the volume is stranded until an operator reclaims it; it
                // carries HOME_VOLUME_LABEL for exactly that.
                tracing::warn!(
                    "Failed to remove sandbox home volume {} (may not exist): {}",
                    home_volume_name,
                    e
                );
            }
        }

        Ok(())
    }

    async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let _access_operation = self
            .service_access
            .as_ref()
            .map(|mounts| mounts.operation(&handle.sandbox_id))
            .transpose()?;
        tracing::info!("Stopping sandbox container {}", handle.sandbox_name);
        if let Some(mounts) = &self.service_access {
            mounts.close(&handle.sandbox_id).await?;
        }
        let result = self
            .docker
            .stop_container(
                &handle.sandbox_id,
                Some(bollard::query_parameters::StopContainerOptions {
                    t: Some(10),
                    signal: None,
                }),
            )
            .await;
        if result.as_ref().is_err_and(docker_error_is_not_found) {
            return Err(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: format!("container {}", handle.sandbox_name),
            });
        }
        result.map_err(|e| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!("Failed to stop container: {}", e),
        })?;
        self.release_parked_sandbox_network(&handle.sandbox_name)
            .await;
        Ok(())
    }

    async fn start(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let _access_operation = self
            .service_access
            .as_ref()
            .map(|mounts| mounts.operation(&handle.sandbox_id))
            .transpose()?;
        tracing::info!("Starting sandbox container {}", handle.sandbox_name);
        let info = self.inspect_handle_policy(handle).await?;
        let _network = self.prepare_network_for_start(handle, &info).await?;
        let result = self
            .docker
            .start_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await;
        if result.as_ref().is_err_and(docker_error_is_not_found) {
            return Err(AgentError::SandboxNotFound {
                run_id: 0,
                sandbox: format!("container {}", handle.sandbox_name),
            });
        }
        result.map_err(|e| AgentError::SandboxExecFailed {
            run_id: 0,
            sandbox_id: handle.sandbox_id.clone(),
            reason: format!("Failed to start container: {}", e),
        })?;
        self.recover_service_access_after_start(handle).await?;
        Ok(())
    }

    async fn restart(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
        let _access_operation = self
            .service_access
            .as_ref()
            .map(|mounts| mounts.operation(&handle.sandbox_id))
            .transpose()?;
        tracing::info!("Restarting sandbox container {}", handle.sandbox_name);
        if let Some(mounts) = &self.service_access {
            mounts.close(&handle.sandbox_id).await?;
        }
        let info = self.inspect_handle_policy(handle).await?;
        let _network = self.prepare_network_for_start(handle, &info).await?;
        self.docker
            .restart_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::RestartContainerOptions>,
            )
            .await
            .map_err(|e| AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("Failed to restart container: {}", e),
            })?;
        self.recover_service_access_after_start(handle).await?;
        Ok(())
    }

    async fn write_file(
        &self,
        handle: &SandboxHandle,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), AgentError> {
        if is_turn_secret_path(path) {
            return self
                .write_turn_secret_file(handle, path, contents, mode)
                .await;
        }
        // Split the absolute path into the parent dir (extraction target) and
        // the file basename (entry name inside the tar). Docker's
        // upload_to_container extracts the tar at the given `path`.
        let (parent_dir, file_name) = match path.rsplit_once('/') {
            Some((p, f)) if !f.is_empty() => {
                let parent = if p.is_empty() { "/" } else { p };
                (parent.to_string(), f.to_string())
            }
            _ => {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: format!(
                        "write_file: path '{}' must be absolute with a filename",
                        path
                    ),
                });
            }
        };

        // Build an in-memory tar with a single file entry. tar crate is sync,
        // so we do this on the current thread (cheap for skill/CLAUDE/.env sized files).
        let tar_bytes = {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            // Files under /home/temps must be owned by the `temps` user (uid 1000)
            // created in the sandbox Dockerfile, otherwise tight modes like 0600
            // become unreadable by the container's runtime user.
            if runtime_user_owns_uploaded_file(path) {
                header.set_uid(SANDBOX_UID.into());
                header.set_gid(SANDBOX_GID.into());
            }
            header.set_mtime(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            header.set_cksum();

            let mut buf: Vec<u8> = Vec::with_capacity(contents.len() + 1024);
            {
                let mut builder = tar::Builder::new(&mut buf);
                builder
                    .append_data(&mut header, &file_name, contents)
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_file: tar build failed for {}: {}", path, e),
                    })?;
                builder
                    .finish()
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_file: tar finish failed for {}: {}", path, e),
                    })?;
            }
            buf
        };

        // Ensure parent directory exists. upload_to_container won't create
        // intermediate dirs — extraction fails if `parent_dir` is missing.
        // Use a short, well-bounded exec for mkdir (it produces no output but
        // returns quickly; the polling exec loop handles the phantom case).
        let mkdir = vec!["mkdir".to_string(), "-p".to_string(), parent_dir.clone()];
        // Best-effort: if mkdir hangs, the upload below will fail loudly with
        // a clear "no such directory" error rather than hanging silently.
        let _ = self.exec(handle, mkdir, HashMap::new(), None).await;

        let options = bollard::query_parameters::UploadToContainerOptionsBuilder::default()
            .path(&parent_dir)
            // Preserve the tar header's explicit uid/gid. Docker's
            // copyUIDGID=true rewrites uploads to the configured container
            // user, which would make root-staged attachments mutable by the
            // harness uid.
            .copy_uidgid("false")
            .build();

        let body = bollard::body_full(tar_bytes.into());

        // Hard timeout so we never replicate the phantom-stream hang.
        let upload = self
            .docker
            .upload_to_container(&handle.sandbox_id, Some(options), body);

        match tokio::time::timeout(std::time::Duration::from_secs(30), upload).await {
            Ok(Ok(())) => {
                tracing::debug!(
                    "write_file: uploaded {} bytes to {} in container {}",
                    contents.len(),
                    path,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_file: upload to {} failed: {}", path, e),
            }),
            Err(_) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_file: upload to {} timed out after 30s", path),
            }),
        }
    }

    async fn read_file(&self, handle: &SandboxHandle, path: &str) -> Result<Vec<u8>, AgentError> {
        self.read_file_limited(handle, path, None).await
    }

    async fn read_file_bounded(
        &self,
        handle: &SandboxHandle,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AgentError> {
        self.read_file_limited(handle, path, Some(max_bytes)).await
    }

    async fn write_directory(
        &self,
        handle: &SandboxHandle,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), AgentError> {
        let plan = directory_upload_files(local_dir, local_dir).map_err(|e| {
            AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "write_directory: failed to list {} for upload to {}: {}",
                    local_dir.display(),
                    target_path,
                    e
                ),
            }
        })?;

        // Build an in-memory tar containing all files from local_dir,
        // preserving relative paths.
        let tar_bytes = {
            let mut buf: Vec<u8> = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut buf);

                for (path, relative) in &plan.files {
                    let path = path.as_path();
                    let relative = relative.as_path();
                    let contents =
                        std::fs::read(path).map_err(|e| AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id: handle.sandbox_id.clone(),
                            reason: format!(
                                "write_directory: failed to read {}: {}",
                                path.display(),
                                e
                            ),
                        })?;

                    let mut header = tar::Header::new_gnu();
                    header.set_size(contents.len() as u64);
                    header.set_mode(0o644);
                    // Set ownership for /home/temps paths
                    let full_target = format!("{}/{}", target_path, relative.display());
                    if full_target.starts_with("/home/temps") {
                        header.set_uid(1000);
                        header.set_gid(1000);
                    }
                    header.set_mtime(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    );
                    header.set_cksum();

                    builder
                        .append_data(&mut header, relative, std::io::Cursor::new(&contents))
                        .map_err(|e| AgentError::SandboxExecFailed {
                            run_id: 0,
                            sandbox_id: handle.sandbox_id.clone(),
                            reason: format!(
                                "write_directory: tar append failed for {}: {}",
                                relative.display(),
                                e
                            ),
                        })?;
                }

                builder
                    .finish()
                    .map_err(|e| AgentError::SandboxExecFailed {
                        run_id: 0,
                        sandbox_id: handle.sandbox_id.clone(),
                        reason: format!("write_directory: tar finish failed: {}", e),
                    })?;
            }
            buf
        };

        // Ensure target directory exists
        let mkdir = vec![
            "mkdir".to_string(),
            "-p".to_string(),
            target_path.to_string(),
        ];
        let _ = self.exec(handle, mkdir, HashMap::new(), None).await;

        let options = bollard::query_parameters::UploadToContainerOptionsBuilder::default()
            .path(target_path)
            .build();

        let body = bollard::body_full(tar_bytes.into());

        match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            self.docker
                .upload_to_container(&handle.sandbox_id, Some(options), body),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(
                    "write_directory: uploaded {} to container {}",
                    target_path,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!("write_directory: upload to {} failed: {}", target_path, e),
            }),
            Err(_) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: handle.sandbox_id.clone(),
                reason: format!(
                    "write_directory: upload to {} timed out after 60s",
                    target_path
                ),
            }),
        }
    }

    async fn kill_processes(
        &self,
        handle: &SandboxHandle,
        pattern: &str,
        signal: super::KillSignal,
    ) -> Result<(), AgentError> {
        // Fresh exec running the kill script. Bounded by a 10s timeout so we
        // never replicate the phantom-stream hang.
        let sig_num = signal.as_number();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            kill_processes_script(pattern, signal),
        ];

        let exec = self.exec(handle, cmd, HashMap::new(), None);
        match tokio::time::timeout(std::time::Duration::from_secs(10), exec).await {
            Ok(Ok(_)) => {
                tracing::debug!(
                    "kill_processes: sent signal {} to '{}' in {}",
                    sig_num,
                    pattern,
                    handle.sandbox_name
                );
                Ok(())
            }
            Ok(Err(e)) => {
                // Don't propagate — kill is best-effort. Log and move on.
                tracing::warn!(
                    "kill_processes: exec failed for '{}' in {}: {}",
                    pattern,
                    handle.sandbox_name,
                    e
                );
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    "kill_processes: timed out killing '{}' in {}",
                    pattern,
                    handle.sandbox_name
                );
                Ok(())
            }
        }
    }

    /// Bridge to the in-sandbox PTY agent, exactly as ADR-008 §Host-side
    /// Bridge specifies: a `docker exec` running `socat` that relays the
    /// exec's stdio to the agent's unix socket.
    ///
    /// Nothing long-lived runs inside this exec — `socat` exits when its
    /// stdin closes, and the exec dies with it. The only persistent
    /// in-container process is the agent itself, which is what keeps a
    /// `claude` session alive across a dropped connection.
    ///
    /// Runs as `SANDBOX_USER` because the socket is mode 0600 owned by that
    /// user. Executing as root would work but would break the "same trust
    /// boundary as the sandbox user" property the ADR relies on.
    async fn attach_pty(&self, handle: &SandboxHandle) -> Result<PtyAttachment, AgentError> {
        self.exec_byte_pipe(
            handle,
            vec![
                "socat".to_string(),
                "-".to_string(),
                format!("UNIX-CONNECT:{}", PTY_AGENT_SOCKET),
            ],
            "PTY attach",
            "PTY agent relay",
        )
        .await
    }

    /// Replace the sandbox's egress proxy with one enforcing `policy`.
    ///
    /// A running sandbox gets a new proxy at once: its outbound connections
    /// are cut while the old proxy is removed and the new one starts (the
    /// elapsed time is logged as `interruption_ms`, typically one to three
    /// seconds), and tunnels open through the old proxy are closed, so a
    /// tightened policy also ends connections it no longer allows. A stopped
    /// sandbox gets a parked proxy holding the policy for its next start.
    async fn set_egress_policy(
        &self,
        handle: &SandboxHandle,
        policy: &SandboxEgressPolicy,
    ) -> Result<(), AgentError> {
        let policy = policy
            .normalized()
            .map_err(|error| AgentError::Validation {
                message: format!("sandbox '{}': {error}", handle.sandbox_name),
            })?;
        let info = match self
            .docker
            .inspect_container(
                &handle.sandbox_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => info,
            Err(error) if docker_error_is_not_found(&error) => {
                return Err(AgentError::SandboxNotFound {
                    run_id: 0,
                    sandbox: format!("container {}", handle.sandbox_name),
                })
            }
            Err(error) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "inspect sandbox '{}' before applying its egress policy: {error}",
                        handle.sandbox_name
                    ),
                })
            }
        };
        let network_mode = info
            .host_config
            .as_ref()
            .and_then(|config| config.network_mode.as_deref());
        if network_mode == Some("none") {
            // No network, so no egress to restrict in any mode.
            tracing::info!(
                sandbox = %handle.sandbox_name,
                egress_mode = policy.mode.as_str(),
                "Sandbox has no network; its egress policy has nothing to enforce"
            );
            return Ok(());
        }
        let _guard = self.network_lock.lock().await;
        if container_is_parked(&info) {
            self.park_egress_policy(&handle.sandbox_name, &policy)
                .await?;
            tracing::info!(
                sandbox = %handle.sandbox_name,
                egress_mode = policy.mode.as_str(),
                egress_rules = policy.rules.len(),
                "Egress policy of a stopped sandbox stored for its next start"
            );
            return Ok(());
        }
        let started = std::time::Instant::now();
        let replaced = self
            .ensure_network_locked(
                &handle.sandbox_name,
                &sandbox_network_name(&handle.sandbox_name),
                &self.config.control_plane_url,
                Some(&policy),
            )
            .await?;
        tracing::info!(
            sandbox = %handle.sandbox_name,
            egress_mode = policy.mode.as_str(),
            egress_rules = policy.rules.len(),
            replaced_proxy = replaced,
            interruption_ms = if replaced {
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
            } else {
                0
            },
            "Egress policy applied to a running sandbox"
        );
        Ok(())
    }

    /// `socat` from the exec's stdio to `127.0.0.1:<port>` inside the
    /// sandbox, as `SANDBOX_USER`: the same pipe the terminal uses, pointed
    /// at the dev server. Each preview connection is one exec, which ends
    /// when either side closes. A refused connection is socat's stderr, so it
    /// arrives as the stream's first item, before any byte was relayed.
    async fn connect_port(
        &self,
        handle: &SandboxHandle,
        port: u16,
    ) -> Result<PtyAttachment, AgentError> {
        if port == 0 {
            return Err(AgentError::Validation {
                message: format!(
                    "port 0 of sandbox '{}' is not a port a preview can reach",
                    handle.sandbox_name
                ),
            });
        }
        self.exec_byte_pipe(
            handle,
            vec![
                "socat".to_string(),
                "-".to_string(),
                format!("TCP:127.0.0.1:{port}"),
            ],
            "port connect",
            "port relay",
        )
        .await
    }

    async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
        let container_name = Self::container_name(run_id);
        self.recover_container(&container_name).await
    }

    async fn recover_by_name(
        &self,
        container_name: &str,
    ) -> Result<Option<SandboxHandle>, AgentError> {
        let full_name = format!("{}{}", SANDBOX_NAME_PREFIX, container_name);
        self.recover_container(&full_name).await
    }

    async fn release_orphaned_resources(&self, container_name: &str) -> Result<(), AgentError> {
        let full_name = format!("{}{}", SANDBOX_NAME_PREFIX, container_name);
        // Same exclusion the automatic reaper uses: refuse while a create of
        // this name is running (it may already have reused the network), and
        // keep creates of this name waiting until the release is done.
        let Some(_claim) = ReapClaim::try_new(&self.creates_in_flight, &full_name) else {
            return Err(AgentError::SandboxConflictOnNode {
                sandbox: full_name,
                node_name: "this node".to_string(),
                reason: "the sandbox is being created or released right now; its network and egress proxy are in use".to_string(),
            });
        };
        // The container may have been created since the caller looked.
        match self
            .docker
            .inspect_container(
                &full_name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(_) => {
                return Err(AgentError::SandboxConflictOnNode {
                    sandbox: full_name,
                    node_name: "this node".to_string(),
                    reason: "the sandbox's container exists, so its network is in use; destroy the sandbox instead".to_string(),
                });
            }
            Err(error) if docker_error_is_not_found(&error) => {}
            Err(source) => {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "docker".to_string(),
                    reason: format!(
                        "inspect sandbox '{full_name}' before releasing its leftover resources: {source}"
                    ),
                });
            }
        }
        // A network labelled for another Temps instance's gateway is not
        // ours to remove even when it carries this sandbox's name.
        let gateway = self
            .config
            .preview_gateway_container_name
            .read()
            .await
            .clone();
        if let Ok(network) = self
            .docker
            .inspect_network(
                &sandbox_network_name(&full_name),
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
        {
            if let Some(labelled) = network_labelled_for_another_gateway(&network, &gateway) {
                tracing::warn!(
                    sandbox = %full_name,
                    network_gateway = %labelled,
                    "Leaving the network of a sandbox that belongs to another Temps instance's preview gateway"
                );
                return Ok(());
            }
        }
        let errors = self.remove_isolated_sandbox_network(&full_name).await;
        if errors.is_empty() {
            return Ok(());
        }
        Err(AgentError::SandboxProviderUnavailable {
            provider: "docker".to_string(),
            reason: format!(
                "release leftover resources of removed sandbox '{full_name}': {}",
                errors.join("; ")
            ),
        })
    }

    fn supports_backend(&self, backend: super::SandboxBackend) -> bool {
        matches!(backend, super::SandboxBackend::Docker)
    }

    fn default_resource_limits(
        &self,
        _backend: Option<super::SandboxBackend>,
    ) -> super::DefaultResourceLimits {
        super::DefaultResourceLimits {
            cpu_limit: Some(self.config.default_cpu_limit),
            memory_limit_mb: Some(self.config.default_memory_limit_mb),
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
            cpu_whole_cores: false,
        }
    }

    fn name(&self) -> &str {
        "docker"
    }

    async fn is_available(&self) -> bool {
        self.docker.ping().await.is_ok()
    }

    async fn image_status(&self) -> Result<(bool, String), AgentError> {
        let image_name = self.config.resolved_image();
        let ready = self.docker.inspect_image(&image_name).await.is_ok();
        Ok((ready, image_name))
    }

    async fn rebuild_image(&self) -> Result<String, AgentError> {
        let image_name = self.config.resolved_image();
        ensure_mutable_rebuild_target(&image_name)?;

        // Remove existing image (force, in case containers reference it)
        if self.docker.inspect_image(&image_name).await.is_ok() {
            let opts = bollard::query_parameters::RemoveImageOptionsBuilder::new()
                .force(true)
                .build();
            let _ = self
                .docker
                .remove_image(&image_name, Some(opts), None)
                .await;
            tracing::info!("Removed old sandbox image {}", image_name);
        }

        // Rebuild locally — explicit rebuilds always build from the
        // generated Dockerfile so local changes (Claude backup, custom
        // packages, etc.) are picked up. Never pull from Hub here.
        self.build_image_locally(&self.config.runtime, None).await?;

        Ok(image_name)
    }

    async fn rebuild_image_with_progress(
        &self,
        on_progress: tokio::sync::mpsc::Sender<String>,
    ) -> Result<String, AgentError> {
        let image_name = self.config.resolved_image();
        ensure_mutable_rebuild_target(&image_name)?;

        // Remove existing image
        if self.docker.inspect_image(&image_name).await.is_ok() {
            let _ = on_progress
                .send(format!("Removing old image {}...", image_name))
                .await;
            let opts = bollard::query_parameters::RemoveImageOptionsBuilder::new()
                .force(true)
                .build();
            let _ = self
                .docker
                .remove_image(&image_name, Some(opts), None)
                .await;
            tracing::info!("Removed old sandbox image {}", image_name);
        }

        // Rebuild locally with progress — never pull from Hub on explicit rebuild.
        self.build_image_locally(&self.config.runtime, Some(&on_progress))
            .await?;

        Ok(image_name)
    }

    // ── Snapshot: take ────────────────────────────────────────────────────────

    /// Capture the current state of `handle` as a content-addressed tarball.
    async fn take_snapshot(
        &self,
        handle: &SandboxHandle,
        label: Option<String>,
        max_size_bytes: u64,
    ) -> Result<super::SnapshotArtifact, AgentError> {
        self.take_snapshot_inner(handle, label, max_size_bytes)
            .await
    }

    // ── Snapshot: restore ─────────────────────────────────────────────────────

    /// Create a new sandbox seeded from a snapshot artifact.
    async fn create_from_snapshot(
        &self,
        artifact: &super::SnapshotArtifact,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        self.create_from_snapshot_inner(artifact, config).await
    }

    // ── Snapshot: delete image ────────────────────────────────────────────────

    /// Remove the Docker image tag for a deleted snapshot.
    ///
    /// Called by `SnapshotService::delete_snapshot` after the DB row is
    /// soft-deleted and the tarball has been removed. This cleans up the
    /// Docker daemon's image store so it doesn't accumulate stale snapshot
    /// images indefinitely.
    ///
    /// Uses `noprune: false` (default) so untagged intermediate layers are
    /// also reclaimed. `force: false` so an image referenced by a running
    /// container isn't deleted — that's a bug, not a normal condition.
    async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
        tracing::debug!(image_ref = %image_ref, "snapshot: removing Docker image");
        match self
            .docker
            .remove_image(
                image_ref,
                Some(
                    bollard::query_parameters::RemoveImageOptionsBuilder::new()
                        .force(false)
                        .noprune(false)
                        .build(),
                ),
                None,
            )
            .await
        {
            Ok(_) => {
                tracing::debug!(image_ref = %image_ref, "snapshot: Docker image removed");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                // Image already absent — idempotent.
                tracing::debug!(
                    image_ref = %image_ref,
                    "snapshot: Docker image not found during delete (already removed)"
                );
                Ok(())
            }
            Err(e) => Err(AgentError::SandboxExecFailed {
                run_id: 0,
                sandbox_id: String::new(),
                reason: format!(
                    "snapshot: failed to remove Docker image '{}': {}",
                    image_ref, e
                ),
            }),
        }
    }
}
