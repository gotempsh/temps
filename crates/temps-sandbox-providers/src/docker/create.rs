// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creating a sandbox container: names, home volume, mounts, the create
//! itself and rolling back a failed create.

use super::*;

/// Naming prefix for the named volume backing a sandbox's `/home/temps`.
pub(crate) const HOME_VOLUME_PREFIX: &str = "temps-sandbox-home-";

/// Scheme marker in every home volume name this build generates.
///
/// This exists to make one specific attack impossible. Before the naming
/// fix, standalone sandboxes keyed their home volume on `sandboxes.id` and
/// agent runs keyed theirs on `agent_runs.id` — two independent sequences
/// sharing one namespace. On an upgraded host the standalone ones are
/// stranded (destroy no longer computes their name), and Docker attaches an
/// existing volume by name, so a later agent run whose id happened to match
/// would silently mount a *different user's* `/home/temps` — their Claude
/// credentials, shell history, and project state, read-write.
///
/// Every name we generate now carries this infix, so no name this build
/// produces can ever collide with a pre-fix volume. Stranded legacy volumes
/// become inert: nothing mounts them again, and the operator removes them
/// (see `HOME_VOLUME_LABEL`).
///
/// Cost of the change: a sandbox whose container is recreated across the
/// upgrade gets a fresh home once. Agent-run homes are ephemeral (purged on
/// destroy) and standalone containers are not recreated in place — stop,
/// start, and restart all reuse the existing container and its mounts — so
/// nothing a user is actively relying on is lost.
pub(crate) const HOME_VOLUME_SCHEME: &str = "v2-";

/// Label stamped on every home volume this provider creates.
///
/// Nothing in the server reads it — volumes are removed only by an explicit
/// sandbox destroy, which knows the exact name. It exists so an operator
/// can reclaim volumes this build strands (a destroy that failed to reach
/// the daemon, or a create that failed after the volume was made):
///
/// ```text
/// docker volume prune --filter label=sh.temps.sandbox.home
/// ```
///
/// That command does NOT cover volumes created before this build — those
/// were auto-created by the bind mount and carry no label. They are the
/// `v2-`-less numeric names, and are removed with:
///
/// ```text
/// docker volume ls -q --filter dangling=true \
///   | grep -E '^temps-sandbox-home-[0-9]+$' \
///   | xargs -r docker volume rm
/// ```
///
/// `dangling=true` is what makes that safe to run on a live host: a volume
/// still attached to a container is not listed.
pub(super) const HOME_VOLUME_LABEL: &str = "sh.temps.sandbox.home";

#[derive(Clone, Copy)]
struct DockerResourceLimits {
    cpu_cores: f64,
    memory_mb: u64,
}

impl DockerSandboxProvider {
    pub(super) async fn create_inner(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<SandboxHandle, AgentError> {
        config.validate_allow_docker(super::SandboxBackend::Docker)?;
        let (container_name, home_volume_name) = Self::sandbox_names(&config);
        // Before anything is asked of the daemon or replaced: a workspace the
        // daemon cannot mount fails the create outright, with the previous
        // sandbox (if any) still in place.
        let workspace = self
            .daemon_mounts
            .resolve(&config.host_work_dir)
            .await
            .map_err(|error| AgentError::SandboxCreationFailed {
                run_id: config.run_id,
                provider: "docker".into(),
                reason: format!(
                    "workspace '{}' of sandbox '{container_name}' cannot be mounted: {error}",
                    config.host_work_dir.display()
                ),
            })?;
        let capacity = self.capacity().await?;
        let cpu_limit = capacity.cpu_limit(
            config.run_id,
            config.cpu_limit,
            self.config.default_cpu_limit,
        )?;
        let memory_limit_mb = capacity.memory_limit(
            config.run_id,
            config.memory_limit_mb,
            self.config.default_memory_limit_mb,
        )?;
        let _access_operation = self
            .service_access
            .as_ref()
            .map(|mounts| mounts.operation(&container_name))
            .transpose()?;
        let egress_policy =
            config
                .egress_policy
                .normalized()
                .map_err(|error| AgentError::Validation {
                    message: format!("sandbox '{container_name}': {error}"),
                })?;
        // Held until the container exists: from here on this sandbox's
        // network may be in use although no container carries its name.
        // A create never overlaps a reap of the same name: wait it out.
        let _create_in_flight = loop {
            if let Some(guard) = CreateInFlight::try_new(&self.creates_in_flight, &container_name) {
                break guard;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        // Resolve the request override exactly once. Provisioning, proxy
        // environment, and HostConfig must all use this same effective value;
        // otherwise `none -> full` references a bridge that was never created
        // and `full -> none` needlessly leaves a proxy/network behind.
        let docker_network = effective_sandbox_network(
            &self.config.network_mode,
            config.network_mode.as_deref(),
            &container_name,
        );

        if let Some(mounts) = &self.service_access {
            match self
                .docker
                .inspect_container(
                    &container_name,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(info) => {
                    if !info
                        .config
                        .as_ref()
                        .and_then(|c| c.labels.as_ref())
                        .and_then(|l| l.get(SANDBOX_CONTAINER_LABEL))
                        .is_some_and(|v| v == "true")
                    {
                        return Err(AgentError::SandboxCreationFailed {
                            run_id: config.run_id,
                            provider: "docker".into(),
                            reason: format!(
                                "refuse to replace unmanaged container {container_name}"
                            ),
                        });
                    }
                    let id =
                        info.id
                            .as_deref()
                            .ok_or_else(|| AgentError::SandboxCreationFailed {
                                run_id: config.run_id,
                                provider: "docker".into(),
                                reason: "inspect previous sandbox omitted container ID".into(),
                            })?;
                    let _previous_operation = mounts.operation(id)?;
                    mounts.close(id).await?;
                    self.docker
                        .remove_container(
                            id,
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                        .map_err(|error| AgentError::SandboxCreationFailed {
                            run_id: config.run_id,
                            provider: "docker".into(),
                            reason: format!("remove previous sandbox {id}: {error}"),
                        })?;
                    if let Some(mount_id) = super::service_access::mount_id(&info) {
                        mounts.remove_detached(mount_id)?;
                    }
                }
                Err(e) if docker_error_is_not_found(&e) => {}
                Err(error) => {
                    return Err(AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".into(),
                        reason: format!("inspect previous sandbox {container_name}: {error}"),
                    })
                }
            }
        } else {
            // Remove existing container with the same name if any (leftover from crash)
            let _ = self
                .docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }

        // Resolve image: per-run override > provider config
        let default_image = self.config.resolved_image();
        let image = config
            .image
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&default_image);

        // Nothing has been created yet, so a failure up to here needs no
        // rollback. From the network on, every failure rolls back.
        self.ensure_sandbox_image(image, config.run_id).await?;
        let (uses_daemon, supports_service_access) =
            self.image_protocols(image, config.run_id).await?;

        let mut access_mount = if supports_service_access {
            match &self.service_access {
                Some(mounts) => {
                    mounts
                        .reserve_for_image(Arc::clone(&self.docker), image)
                        .await?
                }
                None => None,
            }
        } else {
            None
        };

        crate::report_create_phase(crate::CreatePhaseEvent::Started(
            crate::CreatePhase::CreateContainer,
        ));
        let mut container_config = self.sandbox_container_body(
            &config,
            image,
            docker_network.clone(),
            &home_volume_name,
            DockerResourceLimits {
                cpu_cores: cpu_limit,
                memory_mb: memory_limit_mb,
            },
            uses_daemon,
        );
        Self::mount_workspace(&mut container_config, workspace, &home_volume_name);
        if let Some(pending) = &mut access_mount {
            pending.configure(
                &mut container_config,
                Arc::clone(&self.docker),
                &container_name,
            )?;
        }
        if let Err(error) = self
            .ensure_network(
                &container_name,
                &docker_network,
                &self.config.control_plane_url,
                Some(&egress_policy),
            )
            .await
        {
            return Err(self.rollback_failed_create(&container_name, error).await);
        }

        self.ensure_labelled_home_volume(&home_volume_name).await;

        let container_id = match self
            .launch_sandbox_container(
                &container_name,
                container_config,
                config.run_id,
                uses_daemon,
            )
            .await
        {
            Ok(container_id) => container_id,
            Err(error) => {
                return Err(self.rollback_failed_create(&container_name, error).await);
            }
        };
        if let (Some(mounts), Some(pending)) = (&self.service_access, access_mount) {
            let adoption = async {
                let info = self
                    .docker
                    .inspect_container(
                        &container_id,
                        None::<bollard::query_parameters::InspectContainerOptions>,
                    )
                    .await
                    .map_err(|error| AgentError::SandboxCreationFailed {
                        run_id: config.run_id,
                        provider: "docker".into(),
                        reason: format!(
                            "inspect new service access container {container_id}: {error}"
                        ),
                    })?;
                mounts.adopt(&info, pending)
            }
            .await;
            if let Err(error) = adoption {
                return Err(self.rollback_failed_create(&container_name, error).await);
            }
        }
        tracing::info!(
            "Sandbox container {} ({}) created for run {}",
            container_name,
            &container_id[..12],
            config.run_id
        );

        Ok(SandboxHandle {
            node_id: None,
            sandbox_id: container_id,
            sandbox_name: container_name,
            work_dir: PathBuf::from(CONTAINER_WORK_DIR),
            backend: super::SandboxBackend::Docker,
            image: image.to_string(),
            conditions: Vec::new(),
        })
    }

    /// Make sure `image` is present: pull it (a current runtime preset falls
    /// back to a local build), joining a pull already running for it, such
    /// as an operator's "prepare sandboxes". Runs before anything is
    /// created, so it never rolls back. Reports the image phase and its
    /// download progress to the create's progress sink.
    async fn ensure_sandbox_image(&self, image: &str, run_id: i32) -> Result<(), AgentError> {
        use crate::{report_create_phase, CreatePhase, CreatePhaseEvent, ImagePrepareState};
        report_create_phase(CreatePhaseEvent::Started(CreatePhase::Image));
        let status = self
            .ensure_image_present(image, |status| {
                report_create_phase(CreatePhaseEvent::ImageProgress(status.clone()));
            })
            .await;
        match status.state {
            ImagePrepareState::Ready => {
                if status.already_present {
                    report_create_phase(CreatePhaseEvent::ImageAlreadyPresent);
                }
                report_create_phase(CreatePhaseEvent::Finished(CreatePhase::Image));
                Ok(())
            }
            _ => Err(AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: status.error.unwrap_or_else(|| {
                    temps_agent_core::error::image_pull_failure_reason(
                        image,
                        "the pull ended without a result",
                    )
                }),
            }),
        }
    }

    /// Whether `image` runs the in-sandbox runtime daemon, from its
    /// `sh.temps.runtime.protocol` label. Refuses a protocol other than 1.
    async fn image_protocols(&self, image: &str, run_id: i32) -> Result<(bool, bool), AgentError> {
        let image_info = self.docker.inspect_image(image).await.map_err(|error| {
            AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".into(),
                reason: format!("inspect runtime image {image}: {error}"),
            }
        })?;
        let runtime_protocol = image_info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("sh.temps.runtime.protocol"))
            .cloned();
        if runtime_protocol
            .as_deref()
            .is_some_and(|version| version != "1")
        {
            return Err(AgentError::SandboxCreationFailed { run_id, provider: "docker".into(),
                reason: format!("image {image} requires unsupported runtime protocol {runtime_protocol:?}; expected 1") });
        }
        let service_access = image_info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("sh.temps.service-access.protocol"))
            .is_some_and(|version| version == "1");
        Ok((runtime_protocol.is_some(), service_access))
    }

    /// The create request for a sandbox container: image, command,
    /// environment, labels and [`Self::sandbox_host_config`].
    fn sandbox_container_body(
        &self,
        config: &SandboxCreateConfig,
        image: &str,
        docker_network: String,
        home_volume_name: &str,
        limits: DockerResourceLimits,
        uses_daemon: bool,
    ) -> bollard::models::ContainerCreateBody {
        // The proxy variables are enforced after caller-provided values so a
        // stale or malicious configuration cannot route around the managed
        // chokepoint. Direct egress is absent at the network layer anyway;
        // these variables only tell compliant tooling how to reach the proxy.
        let env_vars = sandbox_container_environment(config.env_vars.clone(), &docker_network);
        let host_config =
            self.sandbox_host_config(config, docker_network, home_volume_name, limits);

        let mut labels = HashMap::new();
        if uses_daemon {
            labels.insert("sh.temps.runtime.protocol".into(), "1".into());
        }
        labels.insert(SANDBOX_CONTAINER_LABEL.to_string(), "true".to_string());
        labels.insert(
            "sh.temps.sandbox.run_id".to_string(),
            config.run_id.to_string(),
        );

        bollard::models::ContainerCreateBody {
            image: Some(image.to_string()),
            // Keep the container alive — exec calls run commands inside it
            cmd: Some(if uses_daemon {
                vec!["serve".to_string()]
            } else {
                vec!["sleep".to_string(), "infinity".to_string()]
            }),
            env: if env_vars.is_empty() {
                None
            } else {
                Some(env_vars)
            },
            working_dir: Some(CONTAINER_WORK_DIR.to_string()),
            host_config: Some(host_config),
            labels: Some(labels),
            ..Default::default()
        }
    }

    /// Mounts, network, resource limits and hardening of a sandbox container.
    fn sandbox_host_config(
        &self,
        config: &SandboxCreateConfig,
        docker_network: String,
        home_volume_name: &str,
        limits: DockerResourceLimits,
    ) -> bollard::models::HostConfig {
        // Map user-friendly names to Docker network modes.
        //
        // IMPORTANT: "full" used to map to docker `host` mode, which bypassed
        // container network isolation entirely and prevented sandboxes from
        // joining the shared sandbox user-defined network. That
        // broke workspace preview routing because the preview gateway resolves
        // sandbox containers via Docker's embedded DNS, which only works on
        // user-defined networks. We now route every network-enabled mode
        // through a per-sandbox isolated bridge. Public web access is available
        // exclusively through the managed egress proxy.
        //
        // All network-enabled legacy/product values resolve to the managed
        // internal bridge. There is deliberately no host-network escape hatch.
        let host_work_dir = config.host_work_dir.to_string_lossy().to_string();

        // Bind mount: only the work directory. Auth is handled via env vars
        // (CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY) — no host config mounting.
        //
        // Named volume for the sandbox user's home dir: persists claude
        // session jsonl, shell history, ~/.claude/projects, ~/.config/...
        // across container recreation. Without this, killing and recreating
        // the sandbox would lose all conversation continuity even though the
        // work_dir survives via the bind mount above.
        //
        // `home_volume_name` came from `sandbox_names`, alongside the
        // container name — `destroy` re-derives it from the container name
        // via `home_volume_name`, which is what keeps the two in sync.
        let binds = Self::container_binds(&host_work_dir, home_volume_name);

        // tmpfs mount for secrets — in-memory only, never written to disk
        // Both secret capabilities and source-import staging use explicit
        // tmpfs mounts. The latter gives imports synchronous byte/inode caps.
        let tmpfs = Self::container_tmpfs();

        bollard::models::HostConfig {
            binds: Some(binds),
            network_mode: Some(docker_network),
            tmpfs: Some(tmpfs),
            // Resource limits.
            //
            // `memory_swap == memory` disables swap usage for the container.
            // Without this, Docker's default is `memory_swap = 2 * memory`,
            // meaning each sandbox can silently page an *additional* full
            // memory-limit's worth to host swap. With N sandboxes running
            // dev servers + claude that's a fast path to host swap
            // exhaustion, terrible latency, and no OOM feedback to the
            // sandbox itself. Disabling swap turns over-limit into a clean
            // OOM-kill of the offending process (which is the correct
            // signal — the user sees `next dev` die instead of the whole
            // host dragging).
            nano_cpus: Some((limits.cpu_cores * 1_000_000_000.0) as i64),
            memory: Some(limits.memory_mb as i64 * 1024 * 1024),
            memory_swap: Some(limits.memory_mb as i64 * 1024 * 1024),
            // Security hardening
            cap_drop: Some(vec!["ALL".to_string()]),
            // Minimum caps required for the post-start ownership-normalization
            // dance (see `normalize_ownership`) and for `su temps -c ...` to
            // drop privileges into the sandbox user:
            //   CHOWN          — chown the home volume + bind-mounted workdir
            //   FOWNER         — chmod/utime files we don't own (recover path)
            //   DAC_OVERRIDE   — read/traverse dirs whose mode bits forbid it
            //                    after a prior chown left them o-rwx; without
            //                    this, even root gets EACCES on readdir() of
            //                    /home/temps when a previous run scoped it
            //   SETUID/SETGID  — `su` needs both to call setuid/setgroups when
            //                    handing the write-probe (and every later
            //                    workflow step) off to the temps user. Without
            //                    them `su` fails with "cannot set groups:
            //                    Operation not permitted" and the sandbox is
            //                    declared broken at startup.
            cap_add: Some(vec![
                "CHOWN".to_string(),
                "FOWNER".to_string(),
                "DAC_OVERRIDE".to_string(),
                "SETUID".to_string(),
                "SETGID".to_string(),
            ]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(config.pids_limit.unwrap_or(DEFAULT_PIDS_LIMIT)),
            init: Some(true),
            // Survive Docker daemon restarts (reboot, `systemctl restart docker`,
            // Mac sleep/wake). Without this the default is "no" and a daemon
            // bounce permanently kills the container, leaving the DB row stuck
            // in `running` with nothing behind it. `unless-stopped` means an
            // explicit `docker stop` (including our own pause/destroy paths)
            // still keeps the container stopped — we only auto-restart after
            // daemon-level events.
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            ..Default::default()
        }
    }

    /// Create the home volume explicitly, labelled, rather than letting the
    /// bind auto-create it unlabelled. Never fails the create.
    async fn ensure_labelled_home_volume(&self, home_volume_name: &str) {
        // Create the home volume explicitly, labelled, rather than letting
        // the bind auto-create it unlabelled — see HOME_VOLUME_LABEL for why
        // the label earns its extra call. Idempotent: Docker returns an
        // existing volume unchanged, which is what recreating a sandbox over
        // a surviving home relies on.
        //
        // Deliberately here rather than at the top of `create`: everything
        // that can fail cheaply and repeatedly (image resolution, a cold
        // pull of a caller-supplied image) has already happened, so a caller
        // looping failed creates can't mint a volume per attempt. A failure
        // after this point still strands an empty one, which is what the
        // label-filtered prune is for.
        if let Err(e) = self
            .docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(home_volume_name.to_string()),
                labels: Some(HashMap::from([(
                    HOME_VOLUME_LABEL.to_string(),
                    "true".to_string(),
                )])),
                ..Default::default()
            })
            .await
        {
            // Non-fatal: the bind still auto-creates the volume. The only
            // loss is the label, i.e. this one volume won't appear in an
            // operator's label-filtered prune.
            tracing::warn!(
                "Could not pre-create labelled home volume {}: {} — continuing",
                home_volume_name,
                e
            );
        }
    }

    /// Create, start and prepare the sandbox container, returning its ID.
    ///
    /// Every error here leaves a container (or a half-created one) behind
    /// under `container_name`: the caller must roll the create back.
    async fn launch_sandbox_container(
        &self,
        container_name: &str,
        container_config: bollard::models::ContainerCreateBody,
        run_id: i32,
        uses_daemon: bool,
    ) -> Result<String, AgentError> {
        let container = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|error| AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to create container: {error}"),
            })?;
        crate::report_create_phase(crate::CreatePhaseEvent::Finished(
            crate::CreatePhase::CreateContainer,
        ));
        crate::report_create_phase(crate::CreatePhaseEvent::Started(
            crate::CreatePhase::StartContainer,
        ));

        self.docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|error| AgentError::SandboxCreationFailed {
                run_id,
                provider: "docker".to_string(),
                reason: format!("Failed to start container: {error}"),
            })?;
        crate::report_create_phase(crate::CreatePhaseEvent::Finished(
            crate::CreatePhase::StartContainer,
        ));
        crate::report_create_phase(crate::CreatePhaseEvent::Started(
            crate::CreatePhase::WaitReady,
        ));

        // Normalize ownership of /home/temps (named volume) and
        // /home/temps/workspace (bind-mount). Both inherit uids from outside
        // the container — the home volume from Docker's anonymous-volume
        // initialisation, the workspace from the host process that ran
        // `git clone`. Without this, the sandbox user can't write either
        // tree and every subsequent command fails. Strict variant: a chown
        // failure aborts container creation rather than leaving a broken
        // sandbox that mints "Permission denied" for the rest of the run.
        self.normalize_ownership(&container.id, run_id).await?;

        if !uses_daemon {
            self.restore_ai_clis(&container.id, run_id).await;
        }
        if uses_daemon {
            self.wait_for_runtime_daemon(&container.id, container_name, run_id)
                .await?;
        }
        crate::report_create_phase(crate::CreatePhaseEvent::Finished(
            crate::CreatePhase::WaitReady,
        ));
        Ok(container.id)
    }

    /// Ensure AI CLIs are present in the home volume. Best effort: a failed
    /// restore is logged and the create continues.
    async fn restore_ai_clis(&self, container_id: &str, run_id: i32) {
        // Ensure AI CLIs are present in the home volume. Named volumes
        // persist across image rebuilds and mask the image's home dir,
        // wiping claude/codex/opencode every time the volume gets recycled.
        // Strategy: restore from /opt/claude-backup (local builds always
        // populate it), fall back to re-running the claude installer if
        // /opt/claude-backup is missing entirely (older Hub images).
        //
        // We restore both ~/.local (claude + opencode) and ~/.bun (codex)
        // because bun installs codex into its own global tree, not ~/.local.
        let restore_script = format!(
            "need_restore=0; \
             [ -x {home}/.local/bin/claude ] || need_restore=1; \
             [ -x {home}/.bun/bin/codex ] || need_restore=1; \
             [ -x {home}/.opencode/bin/opencode ] || need_restore=1; \
             if [ \"$need_restore\" = \"0\" ]; then exit 0; fi; \
             echo 'AI CLIs missing in home volume, restoring...'; \
             if [ -d /opt/claude-backup/local ]; then \
               mkdir -p {home}/.local {home}/.bun {home}/.opencode && \
               cp -a /opt/claude-backup/local/. {home}/.local/ && \
               cp -a /opt/claude-backup/bun/. {home}/.bun/ && \
               cp -a /opt/claude-backup/opencode/. {home}/.opencode/ && \
               chown -R {chown} {home}/.local {home}/.bun {home}/.opencode; \
             elif [ -d /opt/claude-backup ]; then \
               cp -a /opt/claude-backup/. {home}/.local/ && \
               chown -R {chown} {home}/.local; \
             elif command -v curl >/dev/null 2>&1; then \
               su - {user} -c 'curl -fsSL https://claude.ai/install.sh | bash' 2>&1; \
             fi",
            home = SANDBOX_HOME,
            chown = SANDBOX_CHOWN,
            user = SANDBOX_USER,
        );
        // Best-effort: a missing AI-CLI restore shouldn't fail the
        // whole sandbox creation (the bind-mount + chown above are
        // already what unblocks `git pull` and the sandbox terminal),
        // so we log the exit code via run_root_exec but ignore
        // non-zero results. The helper itself emits a tracing::error
        // line so the failure is still visible in logs.
        if let Err(e) = self
            .run_root_exec(
                container_id,
                run_id,
                "ai-cli-restore",
                vec!["sh".to_string(), "-c".to_string(), restore_script],
            )
            .await
        {
            tracing::warn!(
                "Sandbox {} ai-cli-restore step failed to launch: {} — continuing",
                container_id,
                e
            );
        }
    }

    /// Wait for the runtime daemon of a protocol-1 image to answer its
    /// health check: 20 attempts, 100 ms apart.
    async fn wait_for_runtime_daemon(
        &self,
        container_id: &str,
        container_name: &str,
        run_id: i32,
    ) -> Result<(), AgentError> {
        for _ in 0..20 {
            if matches!(
                self.run_exec_config(
                    container_id,
                    run_id,
                    "runtime-health",
                    runtime_health_exec_config(),
                )
                .await,
                Ok(0)
            ) {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Err(AgentError::SandboxCreationFailed {
            run_id,
            provider: "docker".into(),
            reason: format!(
                "runtime daemon in {container_name} did not become healthy (protocol 1)"
            ),
        })
    }

    /// Roll back every resource whose ownership was established during
    /// `create`. This is deliberately explicit instead of relying on an async
    /// `Drop`: Docker cleanup must finish before the caller can retry with the
    /// same name, and any cleanup failure must be attached to the primary
    /// creation error rather than silently leaking capacity.
    async fn rollback_failed_create(
        &self,
        container_name: &str,
        primary_error: AgentError,
    ) -> AgentError {
        let mut cleanup_errors = Vec::new();
        if let Err(error) = self
            .docker
            .remove_container(
                container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            if !docker_error_is_not_found(&error) {
                cleanup_errors.push(format!(
                    "remove partially created sandbox container '{container_name}': {error}"
                ));
            }
        }
        cleanup_errors.extend(self.remove_isolated_sandbox_network(container_name).await);

        if cleanup_errors.is_empty() {
            return primary_error;
        }
        let cleanup_context = cleanup_errors.join("; ");
        tracing::error!(
            sandbox = container_name,
            error = %cleanup_context,
            "Sandbox creation rollback was incomplete"
        );
        match primary_error {
            AgentError::SandboxCreationFailed {
                run_id,
                provider,
                reason,
            } => AgentError::SandboxCreationFailed {
                run_id,
                provider,
                reason: format!("{reason}; rollback incomplete: {cleanup_context}"),
            },
            other => AgentError::SandboxProviderUnavailable {
                provider: "docker".to_string(),
                reason: format!("{other}; sandbox creation rollback incomplete: {cleanup_context}"),
            },
        }
    }

    pub(super) fn container_name(run_id: i32) -> String {
        format!("{}{}", SANDBOX_NAME_PREFIX, run_id)
    }

    /// The container name and home-volume name for a create request.
    ///
    /// `create` must take both from here rather than computing either
    /// itself. The leak this replaced came from exactly that: create
    /// derived the volume from `config.run_id` while destroy derived it
    /// from the container name, and nothing forced the two to agree. With
    /// one function owning the pair, a future change to either name has to
    /// go through a single place, and `sandbox_names_agree_for_*` fails if
    /// they ever diverge again — without needing a Docker daemon.
    pub(super) fn sandbox_names(config: &SandboxCreateConfig) -> (String, String) {
        let container_name = config
            .container_name_override
            .clone()
            .map(|id| format!("{}{}", SANDBOX_NAME_PREFIX, id))
            .unwrap_or_else(|| Self::container_name(config.run_id));
        let home_volume_name = Self::home_volume_name(&container_name);
        (container_name, home_volume_name)
    }

    /// The `HostConfig.binds` for a sandbox container: the host work dir at
    /// `/workspace`, and the home volume at `/home/temps`.
    ///
    /// Pure, and separate from `create`, so a test can assert that the
    /// volume the container actually mounts is the one `destroy` will
    /// remove — without needing a Docker daemon. The original leak lived
    /// exactly here: `create` built this bind from one name while `destroy`
    /// computed another, and only an e2e could see it.
    pub(super) fn container_binds(host_work_dir: &str, home_volume_name: &str) -> Vec<String> {
        vec![
            format!("{}:{}", host_work_dir, CONTAINER_WORK_DIR),
            format!("{}:{}", home_volume_name, SANDBOX_HOME),
        ]
    }

    /// Put the workspace at `/workspace` from the source the daemon can
    /// actually reach.
    ///
    /// A host path is bound exactly as [`Self::container_binds`] always did.
    /// A workspace that lives in a named volume of Temps' own container is
    /// mounted from that volume, narrowed to the sandbox's directory, so the
    /// sandbox and Temps keep seeing the same files; binding the path instead
    /// would hand the sandbox an empty directory the daemon created on the
    /// host.
    pub(super) fn mount_workspace(
        container: &mut bollard::models::ContainerCreateBody,
        workspace: temps_core::daemon_mount::DaemonMountSource,
        home_volume_name: &str,
    ) {
        use temps_core::daemon_mount::DaemonMountSource;

        let host_config = container.host_config.get_or_insert_with(Default::default);
        match workspace {
            DaemonMountSource::HostPath(path) => {
                host_config.binds = Some(Self::container_binds(
                    &path.to_string_lossy(),
                    home_volume_name,
                ));
            }
            volume @ DaemonMountSource::VolumeSubpath { .. } => {
                host_config.binds = Some(vec![format!("{}:{}", home_volume_name, SANDBOX_HOME)]);
                host_config
                    .mounts
                    .get_or_insert_with(Vec::new)
                    .push(volume.into_mount(CONTAINER_WORK_DIR, false));
            }
        }
    }

    pub(super) fn container_tmpfs() -> HashMap<String, String> {
        let mut tmpfs = HashMap::new();
        tmpfs.insert(
            "/run/temps-runtime".to_string(),
            format!("size=8m,mode=0700,uid={SANDBOX_UID},gid={SANDBOX_GID}"),
        );
        tmpfs.insert(
            "/run/secrets".to_string(),
            format!("size=1m,mode=0710,gid={SANDBOX_GID}"),
        );
        tmpfs.insert(
            "/run/temps-source-import".to_string(),
            format!("size=256m,nr_inodes=5001,mode=0700,uid={SANDBOX_UID},gid={SANDBOX_GID}"),
        );
        tmpfs
    }

    /// Name of the named volume holding a sandbox's `/home/temps`.
    ///
    /// Derived from the **container** name, never from `run_id` directly.
    /// That distinction is the whole point: agent runs are named
    /// `temps-sandbox-<run_id>` while standalone sandboxes override the
    /// suffix with their opaque `public_id` label, so keying the volume on
    /// `run_id` made create and destroy disagree for every standalone
    /// sandbox — create made `temps-sandbox-home-<row.id>`, destroy tried
    /// to remove `temps-sandbox-home-<hex>`, and the real volume leaked on
    /// the host forever. Both sides now go through this one function, so
    /// they cannot drift again.
    ///
    /// Deriving from the container name also removes a cross-tenant
    /// collision: standalone sandbox row 5 and agent run 5 used to share
    /// `temps-sandbox-home-5`, i.e. one user's `~/.claude` credentials and
    /// shell history mounted into another user's sandbox. `HOME_VOLUME_SCHEME`
    /// is what makes that unreachable rather than merely unlikely — it keeps
    /// every generated name out of the pre-fix namespace, so a stranded
    /// legacy volume can never be picked up by a new sandbox either.
    pub(super) fn home_volume_name(container_name: &str) -> String {
        super::home_volume_name_for_label(
            container_name
                .strip_prefix(SANDBOX_NAME_PREFIX)
                .unwrap_or(container_name),
        )
    }
}

#[cfg(test)]
mod tests {
    //! The steps of `create_inner` against a fake Engine API, without the
    //! sandbox image or a Docker daemon. The point of most of these is the
    //! rollback: once the network exists, every failing step must leave
    //! nothing behind under the sandbox's name.

    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;
    use temps_docker_capture::{Call, FakeDocker, Method, CAPTURED_MESSAGE};

    const CUSTOM_IMAGE: &str = "registry.invalid/sandbox-create-test:1";

    fn config(run_id: i32, network_mode: Option<&str>) -> SandboxCreateConfig {
        SandboxCreateConfig {
            node_id: None,
            owner_user_id: None,
            run_id,
            container_name_override: None,
            host_work_dir: PathBuf::from("/tmp/temps-create-steps"),
            workspace_volume: None,
            image: Some(CUSTOM_IMAGE.to_string()),
            cpu_limit: None,
            memory_limit_mb: None,
            pids_limit: None,
            disk_size_mb: None,
            allow_docker: false,
            egress_policy: Default::default(),
            network_mode: network_mode.map(str::to_string),
            env_vars: HashMap::new(),
            idle_timeout: Duration::from_secs(60),
            backend: None,
        }
    }

    async fn fake_provider() -> (FakeDocker, DockerSandboxProvider) {
        let fake = FakeDocker::start()
            .await
            .expect("start the fake Engine API");
        let provider =
            DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default());
        (fake, provider)
    }

    /// Every image inspect answers with an image carrying `labels`.
    fn image_present(fake: &FakeDocker, labels: Value) {
        fake.respond(
            Method::GET,
            "/images/",
            200,
            json!({ "Config": { "Labels": labels } }),
        );
    }

    #[tokio::test]
    async fn sandbox_host_capacity_is_cached_before_any_docker_mutation() {
        let (fake, provider) = fake_provider().await;
        for run_id in [14, 15] {
            let mut cfg = config(run_id, None);
            cfg.cpu_limit = Some(4.0);
            assert!(matches!(
                provider.create_inner(cfg).await,
                Err(AgentError::SandboxCpuLimit {
                    requested: 4.0,
                    host: 2.0,
                    ..
                })
            ));
        }
        assert_eq!(fake.calls().len(), 1);
        assert_eq!(fake.calls()[0].path, "/info");
    }

    #[test]
    fn sandbox_host_capacity_preserves_explicit_limits_and_clamps_defaults(
    ) -> Result<(), AgentError> {
        for cores in [1.0, 1.5, 2.0] {
            let host = crate::SandboxHostCapacity {
                cpu_cores: cores,
                memory_mb: 1024,
            };
            assert_eq!(host.cpu_limit(42, None, 4.0)?, cores);
            assert_eq!(host.cpu_limit(42, Some(0.5), 4.0)?, 0.5);
            assert!(matches!(
                host.cpu_limit(42, Some(3.0), 4.0),
                Err(AgentError::SandboxCpuLimit { run_id: 42, .. })
            ));
            assert_eq!(host.memory_limit(42, None, 8192)?, 1024);
            assert!(host.memory_limit(42, Some(2048), 512).is_err());
        }
        Ok(())
    }

    fn describe(calls: &[Call]) -> String {
        calls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn index_of(calls: &[Call], method: &str, path: &str) -> usize {
        calls
            .iter()
            .position(|call| call.method == method && call.path == path)
            .unwrap_or_else(|| panic!("no {method} {path} in:\n{}", describe(calls)))
    }

    fn any_call(calls: &[Call], method: &str, path: &str) -> bool {
        calls
            .iter()
            .any(|call| call.method == method && call.path == path)
    }

    /// The sandbox container is force-removed by name after the call at
    /// `failed_at`, i.e. by the rollback rather than the leftover sweep
    /// every create starts with.
    fn assert_rolled_back_after(calls: &[Call], container_name: &str, failed_at: usize) {
        let removed = calls.iter().skip(failed_at + 1).any(|call| {
            call.method == "DELETE"
                && call.path == format!("/containers/{container_name}")
                && call.query.contains("force=true")
        });
        assert!(
            removed,
            "{container_name} was not removed after call {failed_at}:\n{}",
            describe(calls)
        );
    }

    fn creation_failure(error: AgentError) -> (i32, String) {
        match error {
            AgentError::SandboxCreationFailed { run_id, reason, .. } => (run_id, reason),
            other => panic!("expected SandboxCreationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_image_pull_names_the_image_and_creates_nothing() {
        let (fake, provider) = fake_provider().await;
        fake.respond(
            Method::POST,
            "/images/create",
            500,
            json!({ "message": "pull access denied" }),
        );

        let error = provider
            .create_inner(config(7101, Some("none")))
            .await
            .expect_err("the pull fails");
        let (run_id, reason) = creation_failure(error);
        assert_eq!(run_id, 7101);
        assert!(
            reason.contains(CUSTOM_IMAGE) && reason.contains("pull access denied"),
            "{reason}"
        );
        let calls = fake.calls();
        for path in ["/networks/create", "/volumes/create", "/containers/create"] {
            assert!(!any_call(&calls, "POST", path), "{}", describe(&calls));
        }
    }

    #[tokio::test]
    async fn an_image_that_cannot_be_inspected_after_its_pull_is_named() {
        let (fake, provider) = fake_provider().await;

        let error = provider
            .create_inner(config(7102, Some("none")))
            .await
            .expect_err("the image never appears");
        let (_, reason) = creation_failure(error);
        assert!(
            reason.starts_with(&format!("inspect runtime image {CUSTOM_IMAGE}:")),
            "{reason}"
        );
        assert!(!any_call(&fake.calls(), "POST", "/containers/create"));
    }

    #[tokio::test]
    async fn an_unsupported_runtime_protocol_is_refused_before_creating_anything() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({ "sh.temps.runtime.protocol": "2" }));

        let error = provider
            .create_inner(config(7103, Some("none")))
            .await
            .expect_err("protocol 2 is unsupported");
        let (_, reason) = creation_failure(error);
        assert!(reason.contains("unsupported runtime protocol"), "{reason}");
        assert!(reason.contains(CUSTOM_IMAGE), "{reason}");
        let calls = fake.calls();
        for path in ["/volumes/create", "/containers/create"] {
            assert!(!any_call(&calls, "POST", path), "{}", describe(&calls));
        }
    }

    #[tokio::test]
    async fn a_failed_network_setup_rolls_the_sandbox_back() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));
        fake.respond(
            Method::POST,
            "/networks/create",
            500,
            json!({ "message": "network setup refused" }),
        );

        let error = provider
            .create_inner(config(7104, None))
            .await
            .expect_err("the shared network cannot be created");
        match &error {
            AgentError::SandboxProviderUnavailable { reason, .. } => {
                assert!(reason.contains("network setup refused"), "{reason}");
            }
            other => panic!("expected SandboxProviderUnavailable, got {other:?}"),
        }
        let calls = fake.calls();
        let failed_at = index_of(&calls, "POST", "/networks/create");
        assert_rolled_back_after(&calls, "temps-sandbox-7104", failed_at);
        for path in ["/volumes/create", "/containers/create"] {
            assert!(!any_call(&calls, "POST", path), "{}", describe(&calls));
        }
    }

    #[tokio::test]
    async fn a_failed_container_create_is_rolled_back_and_keeps_the_primary_error() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));

        let error = provider
            .create_inner(config(7105, Some("none")))
            .await
            .expect_err("the fake refuses the create");
        let (run_id, reason) = creation_failure(error);
        assert_eq!(run_id, 7105);
        assert!(
            reason.starts_with("Failed to create container:") && reason.contains(CAPTURED_MESSAGE),
            "{reason}"
        );
        assert!(!reason.contains("rollback incomplete"), "{reason}");

        let calls = fake.calls();
        let creates = fake.creates();
        assert_eq!(creates.len(), 1);
        let host = &creates[0].body["HostConfig"];
        assert_eq!(host["NanoCpus"], 2_000_000_000);
        assert_eq!(host["Memory"], 4_294_967_296_i64);
        assert_eq!(host["MemorySwap"], host["Memory"]);
        // The leftover sweep runs first, the labelled home volume right
        // before the container.
        assert_eq!(calls[0].path, "/info");
        assert_eq!(calls[1].method, "DELETE");
        assert_eq!(calls[1].path, "/containers/temps-sandbox-7105");
        let volume = index_of(&calls, "POST", "/volumes/create");
        let failed_at = index_of(&calls, "POST", "/containers/create");
        assert!(volume < failed_at, "{}", describe(&calls));
        assert_rolled_back_after(&calls, "temps-sandbox-7105", failed_at);
    }

    #[tokio::test]
    async fn a_rollback_that_cannot_remove_the_container_says_so_in_the_error() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));
        fake.respond(
            Method::DELETE,
            "/containers/temps-sandbox-7106",
            500,
            json!({ "message": "removal of container is already in progress" }),
        );

        let error = provider
            .create_inner(config(7106, Some("none")))
            .await
            .expect_err("the fake refuses the create");
        let (_, reason) = creation_failure(error);
        assert!(
            reason.starts_with("Failed to create container:"),
            "{reason}"
        );
        assert!(
            reason.contains(
                "rollback incomplete: remove partially created sandbox container 'temps-sandbox-7106'"
            ),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn a_container_that_fails_to_start_is_rolled_back() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));
        fake.allow_create("temps-sandbox-7107");

        let error = provider
            .create_inner(config(7107, Some("none")))
            .await
            .expect_err("the fake has no start endpoint for it");
        let (_, reason) = creation_failure(error);
        assert!(reason.starts_with("Failed to start container:"), "{reason}");
        let calls = fake.calls();
        let failed_at = index_of(&calls, "POST", "/containers/fake-temps-sandbox-7107/start");
        assert_rolled_back_after(&calls, "temps-sandbox-7107", failed_at);
    }

    #[tokio::test]
    async fn a_failed_ownership_fix_rolls_the_started_container_back() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));
        fake.allow_create("temps-sandbox-7108");
        fake.respond(
            Method::POST,
            "/containers/fake-temps-sandbox-7108/start",
            204,
            Value::Null,
        );

        let error = provider
            .create_inner(config(7108, Some("none")))
            .await
            .expect_err("the fake cannot exec");
        let (_, reason) = creation_failure(error);
        assert!(
            reason.starts_with("Failed to create chown-home exec:"),
            "{reason}"
        );
        let calls = fake.calls();
        let failed_at = index_of(&calls, "POST", "/containers/fake-temps-sandbox-7108/exec");
        assert_rolled_back_after(&calls, "temps-sandbox-7108", failed_at);
    }

    #[tokio::test]
    async fn a_home_volume_that_cannot_be_labelled_does_not_stop_the_create() {
        let (fake, provider) = fake_provider().await;
        image_present(&fake, json!({}));
        fake.respond(
            Method::POST,
            "/volumes/create",
            500,
            json!({ "message": "volume plugin unavailable" }),
        );

        let error = provider
            .create_inner(config(7109, Some("none")))
            .await
            .expect_err("the fake refuses the create");
        // The create went on to the container itself.
        let (_, reason) = creation_failure(error);
        assert!(reason.contains(CAPTURED_MESSAGE), "{reason}");
        assert!(any_call(&fake.calls(), "POST", "/containers/create"));
    }

    #[tokio::test]
    async fn a_runtime_daemon_that_never_answers_names_the_sandbox() {
        let (fake, provider) = fake_provider().await;

        let error = provider
            .wait_for_runtime_daemon("fake-daemon-container", "temps-sandbox-7110", 7110)
            .await
            .expect_err("no health exec can run");
        let (run_id, reason) = creation_failure(error);
        assert_eq!(run_id, 7110);
        assert_eq!(
            reason,
            "runtime daemon in temps-sandbox-7110 did not become healthy (protocol 1)"
        );
        let attempts = fake
            .calls()
            .iter()
            .filter(|call| call.path == "/containers/fake-daemon-container/exec")
            .count();
        assert_eq!(attempts, 20);
    }

    #[tokio::test]
    async fn an_ai_cli_restore_that_cannot_launch_is_not_fatal() {
        let (fake, provider) = fake_provider().await;

        provider
            .restore_ai_clis("fake-restore-container", 7111)
            .await;

        assert!(any_call(
            &fake.calls(),
            "POST",
            "/containers/fake-restore-container/exec"
        ));
    }

    /// A containerized Temps keeps sandbox workspaces in its data volume. The
    /// sandbox must mount that volume narrowed to its own directory; binding
    /// the path would give it an empty directory on the Docker host.
    #[tokio::test]
    async fn a_workspace_in_temps_own_volume_is_mounted_from_that_volume() {
        use temps_core::daemon_mount::{
            DaemonFilesystemView, DaemonMounts, OwnMount, OwnMountSource,
        };

        let (fake, provider) = fake_provider().await;
        let provider =
            provider.with_daemon_mounts(DaemonMounts::with_view(DaemonFilesystemView::Container {
                container: "temps-app".to_string(),
                mounts: vec![OwnMount {
                    destination: PathBuf::from("/app/data"),
                    source: OwnMountSource::Volume("temps_data".to_string()),
                }],
                volume_subpath_supported: true,
                api_version: "1.47".to_string(),
            }));
        image_present(&fake, json!({}));
        let mut cfg = config(7120, Some("none"));
        cfg.host_work_dir = PathBuf::from("/app/data/standalone-sandboxes/sbx_7120");

        // The create goes on to steps this fake does not script; only the
        // container it asked for matters here.
        let _ = provider.create_inner(cfg).await;

        let creates = fake.creates();
        let create = creates.last().expect("the sandbox container was requested");
        let host = &create.body["HostConfig"];
        let home = DockerSandboxProvider::home_volume_name("temps-sandbox-7120");
        assert_eq!(host["Binds"], json!([format!("{home}:{SANDBOX_HOME}")]));
        let mounts = host["Mounts"].as_array().expect("a workspace mount");
        assert_eq!(mounts.len(), 1, "{mounts:?}");
        assert_eq!(mounts[0]["Type"], "volume");
        assert_eq!(mounts[0]["Source"], "temps_data");
        assert_eq!(mounts[0]["Target"], CONTAINER_WORK_DIR);
        assert_eq!(
            mounts[0]["VolumeOptions"]["Subpath"],
            "standalone-sandboxes/sbx_7120"
        );
    }

    /// A workspace outside every mount of Temps' own container cannot reach
    /// the daemon at all. The create must say so and touch nothing.
    #[tokio::test]
    async fn a_workspace_the_daemon_cannot_reach_fails_before_creating_anything() {
        use temps_core::daemon_mount::{DaemonFilesystemView, DaemonMounts};

        let (fake, provider) = fake_provider().await;
        let provider =
            provider.with_daemon_mounts(DaemonMounts::with_view(DaemonFilesystemView::Container {
                container: "temps-app".to_string(),
                mounts: Vec::new(),
                volume_subpath_supported: true,
                api_version: "1.47".to_string(),
            }));

        let error = provider
            .create_inner(config(7121, Some("none")))
            .await
            .expect_err("an unreachable workspace must fail the create");

        let (run_id, reason) = creation_failure(error);
        assert_eq!(run_id, 7121);
        assert!(
            reason.contains("workspace '/tmp/temps-create-steps'")
                && reason.contains("temps-sandbox-7121")
                && reason.contains("cannot be mounted"),
            "{reason}"
        );
        assert!(fake.calls().is_empty(), "{}", describe(&fake.calls()));
    }

    #[tokio::test]
    async fn the_container_body_runs_the_daemon_only_for_a_protocol_image() {
        let (_fake, provider) = fake_provider().await;
        let cfg = config(7112, Some("none"));
        let home = DockerSandboxProvider::home_volume_name("temps-sandbox-7112");

        let limits = DockerResourceLimits {
            cpu_cores: 2.0,
            memory_mb: 1024,
        };
        let daemon = provider.sandbox_container_body(
            &cfg,
            CUSTOM_IMAGE,
            "none".to_string(),
            &home,
            limits,
            true,
        );
        assert_eq!(daemon.cmd, Some(vec!["serve".to_string()]));
        let labels = daemon.labels.expect("labels");
        assert_eq!(
            labels.get("sh.temps.runtime.protocol").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            labels.get(SANDBOX_CONTAINER_LABEL).map(String::as_str),
            Some("true")
        );
        assert_eq!(
            labels.get("sh.temps.sandbox.run_id").map(String::as_str),
            Some("7112")
        );

        let plain = provider.sandbox_container_body(
            &cfg,
            CUSTOM_IMAGE,
            "none".to_string(),
            &home,
            limits,
            false,
        );
        assert_eq!(
            plain.cmd,
            Some(vec!["sleep".to_string(), "infinity".to_string()])
        );
        assert!(!plain
            .labels
            .expect("labels")
            .contains_key("sh.temps.runtime.protocol"));
        assert_eq!(plain.image.as_deref(), Some(CUSTOM_IMAGE));
        assert_eq!(plain.working_dir.as_deref(), Some(CONTAINER_WORK_DIR));

        let host = plain.host_config.expect("host config");
        assert_eq!(host.network_mode.as_deref(), Some("none"));
        assert_eq!(
            host.binds,
            Some(DockerSandboxProvider::container_binds(
                "/tmp/temps-create-steps",
                &home
            ))
        );
        // The host configuration carries the resolved limits and disables swap.
        assert_eq!(host.memory, Some(1024 * 1024 * 1024));
        assert_eq!(host.memory_swap, host.memory);
        assert_eq!(host.nano_cpus, Some(2_000_000_000));
        assert_eq!(host.pids_limit, Some(DEFAULT_PIDS_LIMIT));
        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(host.init, Some(true));
    }
}
