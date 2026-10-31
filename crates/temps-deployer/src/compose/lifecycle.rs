// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The deployment lifecycle of a Compose project: prepare and pull, deploy,
//! teardown, destroy and stop.

use bollard::query_parameters::RemoveContainerOptions;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};

use super::diagnostics::collect_redactable_values;
use super::exec::isolated_docker_command;
use super::resolve::ComposeCheckInput;
use super::{
    ComposeDeployFailure, ComposeDeployRequest, ComposeError, ComposeExecutor,
    ComposeServiceResult, PreparedComposeDeploy, COMPOSE_LIFECYCLE_LOCKS, COMPOSE_PROJECT_LABEL,
    COMPOSE_READY_TIMEOUT,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

impl ComposeExecutor {
    /// Serialize prepare → teardown → start → compensation for one Compose
    /// project. Superseding workflows overlap cooperatively, so without this
    /// lock an older cancellation could delete a newer workflow's candidate
    /// files or containers.
    pub async fn acquire_project_lifecycle_lock(&self, project_name: &str) -> OwnedMutexGuard<()> {
        let data_dir =
            std::fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        let key = format!("{}\0{project_name}", data_dir.display());
        let lock = {
            let mut locks = COMPOSE_LIFECYCLE_LOCKS.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(&key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }

    /// Prepare compose files on disk, build images (if required), and pull
    /// images for `image:`-based services.
    ///
    /// This is the first half of a two-phase deploy. It MUST be called while
    /// the previously-running stack is **still alive** so that image-fetch
    /// latency falls outside the downtime window. If this step fails, the
    /// caller MUST NOT call [`Self::teardown_at`] — the old stack is still
    /// serving traffic and should keep doing so.
    ///
    /// On success, returns a [`PreparedComposeDeploy`] that must be passed to
    /// [`Self::deploy_prepared`] after [`Self::teardown_at`].
    pub async fn prepare_and_pull(
        &self,
        request: &ComposeDeployRequest,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        self.prepare_and_pull_for_generation(request, &generation)
            .await
    }

    /// Variant used by workflow jobs that need deterministic cancellation
    /// cleanup. The caller supplies an internal, path-safe generation name so
    /// it can remove the candidate without touching the active generation.
    pub async fn prepare_and_pull_for_generation(
        &self,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        let result = self
            .prepare_and_pull_generation_inner(request, generation)
            .await;
        if result.is_err() {
            if let Err(cleanup_error) = self
                .cleanup_secret_generation(&request.project_name, generation)
                .await
            {
                warn!(
                    project = %request.project_name,
                    generation,
                    error = %cleanup_error,
                    "Failed to clean candidate secrets after Compose preparation failure"
                );
            }
        }
        result
    }

    async fn prepare_and_pull_generation_inner(
        &self,
        request: &ComposeDeployRequest,
        generation: &str,
    ) -> Result<PreparedComposeDeploy, ComposeError> {
        // Fail before any `docker compose` CLI invocation or bollard call --
        // this deployment writes compose files, builds/pulls images, and
        // inspects image entrypoints, none of which can succeed without a
        // local daemon. Checking first means a control-plane process reports
        // the actionable `DockerUnavailable` instead of a raw CLI failure
        // after files are already written to disk.
        self.require_docker()?;
        // Docker Engine and the Compose CLI plugin are separate packages on
        // Linux. Probe the exact isolated CLI environment used by every later
        // command before writing files, building images, or allowing the
        // workflow to stop the currently-serving stack.
        self.require_compose_v2().await?;
        // Idempotent: a request the deploy job already resolved has its
        // values substituted and its merge features flattened, so this only
        // re-checks it. A request from anywhere else gets the same pipeline.
        let resolved = self
            .resolve_configuration(&ComposeCheckInput {
                project_name: &request.project_name,
                project_dir: request.repo_dir.as_deref(),
                compose_file: request
                    .compose_path
                    .as_deref()
                    .unwrap_or("docker-compose.yml"),
                content: &request.compose_content,
                override_content: request.compose_override.as_deref(),
                excluded_services: &[],
                environment: &request.environment_vars,
                repository_env: None,
                filesystem_checks: request.repo_dir.is_some(),
                allow_cli: true,
                defer_unknown_required: false,
            })
            .await?;
        let resolved_request = ComposeDeployRequest {
            compose_content: resolved.content,
            compose_override: resolved.override_content,
            ..request.clone()
        };
        let request = &resolved_request;
        Self::validate_service_dir_name(generation)?;
        let project_dir = self.project_dir(&request.project_name);
        let project_name = request.project_name.clone();
        Self::validate_relative_path(
            request
                .compose_path
                .as_deref()
                .unwrap_or("docker-compose.yml"),
            "compose_path",
        )?;
        Self::validate_compose_path_not_generated(
            request
                .compose_path
                .as_deref()
                .unwrap_or("docker-compose.yml"),
        )?;
        Self::validate_security_exemptions(
            &request.relaxed_capability_services,
            &request.unsandboxed_services,
        )?;
        self.validate_compose_security_policy("compose file", &request.compose_content)?;
        if let Some(ref compose_override) = request.compose_override {
            self.validate_compose_security_policy("compose override", compose_override)?;
            Self::validate_compose_override_with_policy(
                &request.project_name,
                &request.compose_content,
                compose_override,
                &self.policy,
            )?;
        }

        // The build definition stays in the repository. Inline image changes
        // are allowed for image services, while BuildImage policy rejects an
        // image tag on a built service by default.
        let has_build = self.has_build_directives(&request.compose_content)
            || request
                .compose_override
                .as_deref()
                .is_some_and(|content| self.has_build_directives(content));

        // Every value that must never appear in a deployment error, including
        // the secrets this deploy is about to mount: a container that echoes
        // its own secret while crash-looping would otherwise have it captured
        // into the failure diagnostic and persisted on the deployment.
        let redact_values = collect_redactable_values(request);

        // Always use the repo checkout directory when available.
        // Compose files often reference local paths (bind mounts, configs,
        // build contexts) that only exist in the repo, not in the temps data dir.
        let effective_dir = request
            .repo_dir
            .clone()
            .unwrap_or_else(|| project_dir.clone());

        // 1. Write compose files + env overrides to disk
        self.write_compose_files(&effective_dir, request, generation)
            .await?;

        let compose_file = request
            .compose_path
            .as_deref()
            .unwrap_or("docker-compose.yml")
            .to_string();

        // Lexical path validation cannot see repository symlinks. Resolve every
        // host path from the same base directory Docker Compose uses, after the
        // checkout and compose files exist but before build/up can touch the
        // host. This closes `./data -> /` style escapes for bind mounts,
        // configs/secrets, local-driver binds, and build paths.
        Self::validate_compose_filesystem_confinement_with_policy(
            &effective_dir,
            &compose_file,
            "compose file",
            &request.compose_content,
            &self.policy,
        )?;
        if let Some(ref compose_override) = request.compose_override {
            Self::validate_compose_filesystem_confinement_with_policy(
                &effective_dir,
                &compose_file,
                "compose override",
                compose_override,
                &self.policy,
            )?;
        }

        // 2. Build images if compose file has build: directives
        if has_build {
            self.compose_build(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                &request.build_args,
                request.output_sink.as_ref(),
            )
            .await?;
        }

        // 3. Pull images for plain `image:` services BEFORE tearing down old
        // containers. Image-fetch latency (seconds to minutes on slow registries
        // or fat images) would otherwise occur inside the downtime window between
        // old-container-stop and new-container-healthy. By pulling here, while
        // the old stack is still serving traffic, we minimise the actual gap.
        // `--ignore-buildable` skips services that only have a `build:` directive
        // (those were already handled above by `compose_build`), so this call is
        // safe regardless of whether the project mixes built and pulled services.
        self.compose_pull(
            &effective_dir,
            &project_name,
            &compose_file,
            &redact_values,
            request.output_sink.as_ref(),
        )
        .await?;

        // Docker's injected init process is useful for ordinary application
        // images, but it must not sit in front of an init system already owned
        // by the image. Some init systems (notably s6-overlay) require PID 1;
        // others (Tini/dumb-init) lose their reaping role when wrapped. Inspect
        // the images we just built/pulled instead of coupling this behavior to
        // registry names or catalog templates, then rewrite only the generated
        // security override. Explicit `init: false` remains supported as a
        // user-controlled fallback when an image hides its init behind a shell
        // entrypoint that metadata cannot identify safely.
        let image_owned_init_services = self
            .detect_image_owned_init_services(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
            )
            .await?;
        self.write_security_override(&effective_dir, request, &image_owned_init_services)
            .await?;

        Ok(PreparedComposeDeploy {
            effective_dir,
            project_name,
            compose_file,
            redact_values,
            secret_generation: generation.to_string(),
            has_materialized_secrets: !request.secrets.is_empty(),
        })
    }

    /// Complete a deployment after the old stack has been torn down.
    ///
    /// This is the second half of a two-phase deploy. It expects a
    /// [`PreparedComposeDeploy`] produced by a prior call to
    /// [`Self::prepare_and_pull`] (images are already local at this point),
    /// creates the shared network if needed, starts the new containers, waits
    /// for readiness, discovers containers, and applies Temps labels.
    pub async fn deploy_prepared(
        &self,
        prepared: PreparedComposeDeploy,
        request: &ComposeDeployRequest,
    ) -> Result<Vec<ComposeServiceResult>, Box<ComposeDeployFailure>> {
        let PreparedComposeDeploy {
            effective_dir,
            project_name,
            compose_file,
            redact_values,
            secret_generation,
            has_materialized_secrets,
        } = prepared;

        // Ensure the shared Temps network exists before `up` attaches every
        // service to it (docker-compose.temps-network.yml, written above) —
        // this is the same network Temps-managed external services and
        // single-container app deployments join, so a compose service can
        // reach a Temps-managed database by name.
        self.ensure_temps_network_exists()
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;

        // 4. Run docker compose up. Images are already pulled (step 3) or built
        // (step 2), so `--pull never` would also work, but omitting `--pull`
        // entirely lets Compose fall back to its default (only pull if absent
        // locally), which is safe and avoids a redundant network round-trip.
        // If a user-provided `container_name` conflicts with an existing
        // container, let Compose report the conflict instead of deleting
        // containers outside this Temps project boundary.
        if let Err(error) = self
            .compose_up(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                request.output_sink.as_ref(),
            )
            .await
        {
            return Err(Box::new(
                self.failure_with_discovered_containers(
                    &effective_dir,
                    &project_name,
                    &compose_file,
                    request,
                    error,
                )
                .await,
            ));
        }

        // 4b. `up -d` returns as soon as containers are created/started, not
        // once they're actually ready. Wait for every service to reach
        // `running` (and `healthy`, for services that define a healthcheck)
        // so a crash-looping or slow-starting service surfaces as a failed
        // deployment instead of a false "success".
        let expectations = Self::readiness_expectations(
            &request.compose_content,
            request.compose_override.as_deref(),
            &request.routed_services,
        );
        if let Err(error) = self
            .wait_for_services_ready(
                &effective_dir,
                &project_name,
                &compose_file,
                &redact_values,
                request.ready_timeout.unwrap_or(COMPOSE_READY_TIMEOUT),
                &expectations,
            )
            .await
        {
            return Err(Box::new(
                self.failure_with_discovered_containers(
                    &effective_dir,
                    &project_name,
                    &compose_file,
                    request,
                    error,
                )
                .await,
            ));
        }

        // 5. Discover running containers. One-shot tasks that completed
        // (exit 0) are not part of the running deployment: registering them
        // would make an exited migration container the stack's "main"
        // container and a target for routing and monitoring.
        let containers = self
            .discover_containers(&effective_dir, &project_name, &compose_file)
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;
        let containers: Vec<ComposeServiceResult> = containers
            .into_iter()
            .filter(|container| {
                let completed = container.status == "exited"
                    && expectations.why_long_running(&container.service_name).is_none();
                if completed {
                    info!(
                        project = %project_name,
                        service = %container.service_name,
                        "Compose service completed as a one-shot task; not registering it as a running container"
                    );
                }
                !completed
            })
            .collect();

        // 5b. Verify the ownership labels written by the generated override.
        // Never register containers cleanup cannot prove belong to this stack.
        for container in &containers {
            self.verify_labels(
                &container.container_id,
                &request.labels,
                &container.service_name,
            )
            .await
            .map_err(|error| Box::new(ComposeDeployFailure::without_containers(error)))?;
        }

        if let Err(error) = self
            .prune_secret_generations(
                &project_name,
                has_materialized_secrets.then_some(secret_generation.as_str()),
            )
            .await
        {
            // The candidate is already mounted by healthy replacement
            // containers. Turning an obsolete-generation janitor failure into
            // a deploy error would make the caller tear those containers down
            // after the old stack is gone. Keep serving and retry removal on a
            // later successful deployment.
            warn!(
                project = %project_name,
                generation = %secret_generation,
                error = %error,
                "Compose deployed successfully, but obsolete secret generation cleanup was deferred"
            );
        }

        info!(
            project = %project_name,
            services = containers.len(),
            "Compose stack deployed"
        );

        Ok(containers)
    }

    /// Deploy a compose stack: write files, pull images, start containers,
    /// wait for every service to become ready, then discover and label them.
    /// Returns one result per service. Fails (rather than reporting a false
    /// success) if a service never reaches `running`/`healthy` within
    /// [`COMPOSE_READY_TIMEOUT`].
    ///
    /// This is a thin wrapper over [`Self::prepare_and_pull`] followed by
    /// [`Self::deploy_prepared`]. Callers that need to tear down an existing
    /// stack between the two phases (the common redeploy path) should call
    /// `prepare_and_pull` and `deploy_prepared` directly so that image-fetch
    /// latency falls outside the downtime window.
    pub async fn deploy(
        &self,
        request: ComposeDeployRequest,
    ) -> Result<Vec<ComposeServiceResult>, ComposeError> {
        let prepared = self.prepare_and_pull(&request).await?;
        self.deploy_prepared(prepared, &request)
            .await
            .map_err(|failure| failure.error)
    }

    /// Tear down containers before a redeploy. Preserves volumes (database data,
    /// uploads, etc.) so they survive between deployments.
    ///
    /// Passes `remove_secrets: false` because an upcoming `deploy_prepared()`
    /// call will consume the secrets that `prepare_and_pull()` just materialized.
    /// Deleting them here would cause the new containers to start with missing
    /// or empty secret files.
    pub async fn teardown_for_redeploy(&self, project_name: &str) -> Result<(), ComposeError> {
        self.teardown_at(project_name, None, None, &HashMap::new(), false)
            .await
    }

    /// Tear down a Compose stack from the exact directory used for `up`.
    /// Uploaded/Git deployments run Compose inside their checkout rather than
    /// the data-dir fallback, so compensation must retain that location.
    ///
    /// `remove_secrets` controls whether the on-disk directory of materialized
    /// secret files (`data_dir/compose-secrets/<project_name>/`) is deleted.
    ///
    /// **HAZARD**: In a `prepare_and_pull()` → `teardown_at()` → `deploy_prepared()`
    /// sequence the secrets have just been materialized and `deploy_prepared()`
    /// is about to bind-mount them into new containers. Passing `remove_secrets:
    /// true` in that sequence deletes the files that the new containers need,
    /// causing them to start with missing or empty secrets — a silent credential
    /// loss that only surfaces at container runtime. Always pass `false` when
    /// an immediate `deploy_prepared()` follows.
    ///
    /// Pass `true` only when the deploy attempt is definitively over: compensating
    /// cleanup on failure, zero-services edge-case teardown, cancellation cleanup,
    /// or final destruction of a project/environment.
    pub async fn teardown_at(
        &self,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
        _environment_vars: &HashMap<String, String>,
        remove_secrets: bool,
    ) -> Result<(), ComposeError> {
        if let Some(compose_path) = compose_path {
            Self::validate_relative_path(compose_path, "compose_path")?;
        }
        // Only delete the plaintext secret files when this teardown is the last
        // step — i.e. the deploy attempt is over and nothing downstream will
        // consume the files that were just materialized. When `remove_secrets`
        // is false, the caller is about to call `deploy_prepared()` and those
        // files must remain on disk so the new containers can bind-mount them.
        // See the hazard note on `teardown_at`'s doc comment.
        if remove_secrets {
            self.remove_secrets_dir(project_name).await;
        }

        let project_dir = repo_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.project_dir(project_name));

        if !project_dir.exists() {
            // Git deployments run Compose from a per-deployment checkout that
            // is gone by the time a later deployment stops the stack (for
            // example when moving it to another node), so a missing directory
            // does not mean nothing is running. Every container Compose
            // started carries the project label: remove those instead.
            if !self.docker_available() {
                debug!(project = %project_name, "Project directory does not exist and no Docker daemon is attached, nothing to tear down");
                return Ok(());
            }
            debug!(project = %project_name, "Project directory does not exist, removing the project's containers by label");
            return self.remove_labeled_containers(project_name).await;
        }

        // Fail before spawning `docker compose down` -- a control-plane
        // process with no daemon would otherwise see a raw CLI failure
        // instead of this typed, actionable error.
        self.require_docker()?;

        let compose_file = compose_path
            .map(ToString::to_string)
            .unwrap_or_else(|| self.find_compose_file(&project_dir));

        // down WITHOUT --volumes: removes containers and networks, keeps volumes
        let mut command = isolated_docker_command();
        command.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut command, &project_dir, &compose_file);
        command
            .args(["down", "--remove-orphans", "--timeout", "30"])
            .current_dir(&project_dir)
            .kill_on_drop(true);
        let output = Self::bounded_command_output(
            command,
            std::time::Duration::from_secs(35),
            project_name,
            "docker compose down",
        )
        .await?;

        if !output.status.success() {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose down failed with status {}", output.status),
            });
        }

        info!(project = %project_name, "Compose stack torn down (volumes preserved)");
        Ok(())
    }

    /// Fully destroy a compose stack including all volumes and data.
    /// Used when deleting a project/environment permanently.
    pub async fn destroy(&self, project_name: &str) -> Result<(), ComposeError> {
        // Fail before any `docker compose down` CLI invocation or the
        // bollard-based label sweep below -- a control-plane process with no
        // daemon has nothing here to destroy.
        self.require_docker()?;

        let project_dir = self.project_dir(project_name);

        // Secrets live outside `project_dir` (see `secrets_root`), so removing
        // the work directory below does not cover them.
        self.remove_secrets_dir(project_name).await;

        // `docker compose down` only works from the exact directory/file the
        // stack was `up`'d from. Git-backed deployments run Compose from an
        // ephemeral checkout under a per-deployment temp dir (cleaned up as
        // soon as the deploy job finishes) rather than `project_dir`, so this
        // directory-based teardown is frequently a no-op for them -- it must
        // never be the only cleanup path. Volumes/networks/containers Compose
        // creates always carry the `com.docker.compose.project` label
        // regardless of which directory `up` ran from, so the label-based
        // sweep below is the one step that's reliable independent of
        // compose_path/repo_dir.
        if project_dir.exists() {
            let compose_file = self.find_compose_file(&project_dir);

            // down WITH --volumes: removes everything including persistent data
            let mut command = isolated_docker_command();
            command.args(["compose", "-p", project_name]);
            Self::append_compose_input_args(&mut command, &project_dir, &compose_file);
            command
                .args(["down", "--remove-orphans", "--volumes", "--timeout", "30"])
                .current_dir(&project_dir);
            let output = Self::bounded_command_output(
                command,
                std::time::Duration::from_secs(35),
                project_name,
                "docker compose destroy",
            )
            .await?;

            if !output.status.success() {
                warn!(project = %project_name, status = %output.status, "docker compose down failed (falling back to label-based cleanup)");
            }

            // Clean up work directory
            if let Err(e) = tokio::fs::remove_dir_all(&project_dir).await {
                warn!(project = %project_name, error = %e, "Failed to clean up project directory");
            }
        } else {
            debug!(project = %project_name, "Project directory does not exist, relying on label-based cleanup");
        }

        self.destroy_labeled_resources(project_name).await?;

        info!(project = %project_name, "Compose stack destroyed (volumes removed)");
        Ok(())
    }

    /// Remove every Docker network and volume Compose labeled with
    /// `com.docker.compose.project=<project_name>`. Unlike `docker compose
    /// down`, this needs no compose file or working directory -- Compose
    /// attaches this label to every resource it creates regardless of which
    /// directory `up` ran from, so it is the only cleanup step that reliably
    /// works for git-backed deployments whose checkout directory is long gone
    /// by the time a project/environment is deleted. Containers are expected
    /// to already be removed by the caller (deployment_containers-driven
    /// cleanup); this only sweeps what that leaves behind.
    async fn destroy_labeled_resources(&self, project_name: &str) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let label_filter = format!("{COMPOSE_PROJECT_LABEL}={project_name}");
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![label_filter]);

        let networks = docker
            .list_networks(Some(
                bollard::query_parameters::ListNetworksOptionsBuilder::new()
                    .filters(&filters)
                    .build(),
            ))
            .await
            .map_err(|e| {
                ComposeError::Docker(format!("list_networks for '{project_name}': {e}"))
            })?;
        for network in networks {
            let Some(id) = network.id.or(network.name) else {
                continue;
            };
            if let Err(e) = docker.remove_network(&id).await {
                warn!(project = %project_name, network = %id, error = %e, "Failed to remove Compose-managed network");
            }
        }

        let volumes = docker
            .list_volumes(Some(
                bollard::query_parameters::ListVolumesOptionsBuilder::new()
                    .filters(&filters)
                    .build(),
            ))
            .await
            .map_err(|e| ComposeError::Docker(format!("list_volumes for '{project_name}': {e}")))?;
        for volume in volumes.volumes.unwrap_or_default() {
            if let Err(e) = docker
                .remove_volume(
                    &volume.name,
                    Some(
                        bollard::query_parameters::RemoveVolumeOptionsBuilder::new()
                            .force(true)
                            .build(),
                    ),
                )
                .await
            {
                warn!(project = %project_name, volume = %volume.name, error = %e, "Failed to remove Compose-managed volume");
            }
        }

        Ok(())
    }

    /// `docker compose down` without a Compose file: stop and remove every
    /// container labelled with `project_name`, keeping its volumes.
    async fn remove_labeled_containers(&self, project_name: &str) -> Result<(), ComposeError> {
        let docker = self.require_docker()?;
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{COMPOSE_PROJECT_LABEL}={project_name}")],
        )]);
        let containers = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| {
                ComposeError::Docker(format!("list_containers for '{project_name}': {e}"))
            })?;
        let mut failures = Vec::new();
        for container in containers {
            let Some(id) = container.id else {
                continue;
            };
            if let Err(error) = docker
                .stop_container(
                    &id,
                    // Compose's default stop timeout.
                    Some(bollard::query_parameters::StopContainerOptions {
                        t: Some(10),
                        signal: None,
                    }),
                )
                .await
            {
                // Already stopped (304) or gone (404) is fine; removal below
                // forces whatever is left.
                debug!(project = %project_name, container = %id, %error, "Stopping a Compose container before removal");
            }
            match docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: false,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(())
                | Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(error) => failures.push(format!("remove {id}: {error}")),
            }
        }
        if failures.is_empty() {
            info!(project = %project_name, "Compose stack torn down by label (volumes preserved)");
            Ok(())
        } else {
            Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!(
                    "could not remove {} container(s) of the stack: {}",
                    failures.len(),
                    failures.join("; ")
                ),
            })
        }
    }

    /// Stop a compose stack without removing volumes.
    pub async fn stop(&self, project_name: &str) -> Result<(), ComposeError> {
        self.stop_at(project_name, None, None).await
    }

    pub async fn stop_at(
        &self,
        project_name: &str,
        repo_dir: Option<&Path>,
        compose_path: Option<&str>,
    ) -> Result<(), ComposeError> {
        // Fail before spawning `docker compose stop`.
        self.require_docker()?;

        let project_dir = repo_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.project_dir(project_name));
        let compose_file = compose_path
            .map(ToString::to_string)
            .unwrap_or_else(|| self.find_compose_file(&project_dir));
        let mut command = isolated_docker_command();
        command.args(["compose", "-p", project_name]);
        Self::append_compose_input_args(&mut command, &project_dir, &compose_file);
        command
            .args(["stop", "--timeout", "30"])
            .current_dir(&project_dir);
        let output = Self::bounded_command_output(
            command,
            std::time::Duration::from_secs(35),
            project_name,
            "docker compose stop",
        )
        .await?;

        if !output.status.success() {
            return Err(ComposeError::CommandFailed {
                project: project_name.to_string(),
                reason: format!("docker compose stop failed with status {}", output.status),
            });
        }

        Ok(())
    }

    // --- Internal methods ---
}

#[cfg(test)]
mod tests;
