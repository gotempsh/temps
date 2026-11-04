// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creating a sandbox: request validation, placement, the provider create,
//! source seeding and cleanup of a failed create.

use super::*;

/// `metadata.source` of a sandbox created through `POST /sandboxes`.
pub(super) const API_SANDBOX_SOURCE: &str = "api";

/// First key of the two-key Postgres advisory lock serializing the per-user
/// sandbox cap (the second key is the user id).
const USER_SANDBOX_CAP_LOCK_NAMESPACE: i32 = 0x7365_6263;
/// How long removing the container of a failed worker create may take: the
/// same per-node deadline as an eviction's container destroy.
const CREATE_CLEANUP_DEADLINE: std::time::Duration = EVICTION_DESTROY_DEADLINE;

impl SandboxService {
    /// Create a new standalone sandbox. Inserts the DB row first (to get
    /// the internal ID the provider indexes by), then asks the provider
    /// to create the container. On provider failure the DB row is marked
    /// "destroyed" so list doesn't show zombie entries.
    /// Report sandboxes this process is still creating as `starting`: their
    /// rows say `running` from the insert on, before any container exists.
    pub fn mark_creates_in_progress(&self, summaries: &mut [SandboxSummary]) {
        for summary in summaries.iter_mut() {
            if summary.status == "running" && self.creates_in_progress.contains(&summary.public_id)
            {
                summary.status = "starting".to_string();
            }
        }
    }

    /// [`Self::create_sandbox`] in a task of its own, so it always reaches its
    /// success or cleanup path, even when the caller stops waiting.
    ///
    /// The first sandbox on a node can spend minutes building its image. An
    /// HTTP client or proxy that gives up drops the request's future, and a
    /// create cancelled there left the row "running" with no backend while
    /// the worker tore down the container it had just made, because the
    /// control plane stopped waiting. `hold` is dropped when the create ends:
    /// a lock the caller must keep for exactly that long.
    pub fn spawn_create<H: Send + 'static>(
        self: &Arc<Self>,
        user_id: i32,
        req: CreateSandboxRequest,
        hold: H,
    ) -> tokio::task::JoinHandle<Result<sandboxes::Model, SandboxError>> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let result = service.create_sandbox(user_id, req).await;
            drop(hold);
            result
        })
    }

    pub async fn create_sandbox(
        &self,
        user_id: i32,
        mut req: CreateSandboxRequest,
    ) -> Result<sandboxes::Model, SandboxError> {
        let lifecycle = match req.lifecycle.as_deref() {
            None => SandboxLifecycle::default(),
            Some(value) => SandboxLifecycle::parse(value)?,
        };
        let egress_policy =
            req.egress_policy
                .normalized()
                .map_err(|error| SandboxError::Validation {
                    message: format!("egress_policy: {error}"),
                })?;

        if req.from_snapshot_artifact.is_some() && req.source.is_some() {
            return Err(SandboxError::Validation {
                message: "source cannot be combined with from_snapshot; the snapshot already owns the workspace contents"
                    .to_string(),
            });
        }

        // Resolve the effective seed source before anything is created.
        // An explicit `source` always wins — `project_id` only supplies a
        // default, so a caller can create a workspace attributed to a
        // project while seeding it from a fork or a different branch.
        let source = if req.from_snapshot_artifact.is_some() {
            // project_id remains valid attribution, but must not implicitly
            // clone over the workspace restored from the snapshot.
            None
        } else {
            match (req.source.clone(), req.project_id) {
                // Caller-supplied: already validated by the handler, which owns
                // the request-shape rules too. Re-checking here would duplicate a
                // DNS lookup on every create for no gain.
                (Some(explicit), _) => Some(explicit),
                // Project-derived: the handler never saw this URL, so it is
                // validated here — the layer that produced it. `projects.git_url`
                // is checked for IP literals on the project side but never
                // resolved, so a name pointing at the metadata endpoint would
                // otherwise pass; and a row predating that guard can still carry
                // `user:password@`, which would land in `source_repo_url` and be
                // echoed back out of the API.
                (None, Some(project_id)) => {
                    let derived = self.source_from_project(project_id).await?;
                    if let Some(source) = derived.as_ref() {
                        validate_resolved_source(source).await?;
                    }
                    derived
                }
                (None, None) => None,
            }
        };

        let source_repo_url = source.as_ref().and_then(|s| match s {
            SandboxSource::Git { url, .. } => Some(url.clone()),
            SandboxSource::Tarball { .. } => None,
        });

        let timeout = req
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let public_id_value = public_id::generate();
        let name = req.name.clone().unwrap_or_else(|| public_id_value.clone());
        let managed_application_id = managed_application_id_from_name(&name);
        if managed_application_id.is_some() && req.host_work_dir_override.is_none() {
            return Err(SandboxError::Validation {
                message: format!(
                    "sandbox names beginning with '{APPLICATION_WORKSPACE_NAME_PREFIX}' are reserved for managed application workspaces"
                ),
            });
        }
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(timeout as i64);

        // Validate the requested backend before any DB/container work.
        // Only the public backends are accepted — "local" is a dev
        // fallback, never a caller choice. `None` = host default (docker
        // unless the operator changed it), so existing clients see no
        // behavior change.
        let backend = parse_requested_backend(req.backend.as_deref())?;
        validate_allow_docker(req.allow_docker, backend)?;

        // ADR-048: decide which node hosts the sandbox before anything is
        // created. Explicit requests are validated against the operator
        // allow-list and never fall back to another node.
        let node_id = self.place_sandbox(&req).await?;

        // Operator limits (HTTP create path only). The settings are read once
        // and fail closed: an unreadable section must not turn into "no
        // limits". Host memory is only known for the control plane.
        let operator_limits = if req.enforce_operator_limits {
            let settings = self
                .platform_config
                .get_agent_sandbox_settings()
                .await
                .map_err(|source| SandboxError::LimitSettings { user_id, source })?;
            let host_memory_mb = if node_id.is_none() {
                create_policy::control_plane_memory_mb()
            } else {
                None
            };
            // On the control plane the provider says what it defaults to; a
            // worker applies the control plane's configured sandbox
            // defaults (Docker only, ADR-048) and Docker's PID default.
            let defaults = if node_id.is_none() {
                self.registry
                    .provider_arc()
                    .default_resource_limits(backend)
            } else {
                temps_sandbox_providers::DefaultResourceLimits {
                    cpu_limit: Some(settings.cpu_limit),
                    memory_limit_mb: Some(settings.memory_limit_mb),
                    pids_limit: Some(temps_sandbox_providers::docker::DEFAULT_PIDS_LIMIT),
                    cpu_whole_cores: false,
                }
            };
            // The host-memory check sees only what the caller sent; the
            // clamp then fills in omitted limits, and the maximums are
            // checked against the result.
            create_policy::check_host_memory(&req, host_memory_mb)?;
            create_policy::clamp_omitted_limits(&mut req, &settings, defaults);
            create_policy::pin_omitted_memory_to_host(&mut req, defaults, host_memory_mb);
            create_policy::check_requested_limits(&req, &settings, defaults)?;
            Some(settings)
        } else {
            None
        };

        if node_id.is_some() {
            if req.from_snapshot_artifact.is_some() {
                return Err(SandboxError::Validation {
                    message: "restoring a snapshot onto a worker node is not available yet; \
                              create the sandbox on the control plane (node: \"control-plane\")"
                        .to_string(),
                });
            }
            if let Some(b) =
                backend.filter(|b| !temps_sandbox_providers::WORKER_SANDBOX_BACKENDS.contains(b))
            {
                return Err(SandboxError::Validation {
                    message: placement::worker_backend_unavailable(b),
                });
            }
        }

        // Fail closed if the caller asked for a backend this host can't
        // provide (e.g. `firecracker` on a Docker-only host). Isolation
        // level is a security property — silently downgrading to Docker
        // would be worse than a clear error.
        if let (Some(b), None) = (backend, node_id) {
            if !self.registry.provider_arc().supports_backend(b) {
                return Err(SandboxError::Validation {
                    message: backend_unavailable_message(b),
                });
            }
        }

        // Validate + hash the optional preview password *before* any
        // container/workdir work starts. A caller passing junk should fail
        // fast with a 400 rather than leaving an orphan container behind.
        let preview = match req.preview_password.as_deref() {
            Some(pw) => {
                crate::services::preview_password::validate(pw)
                    .map_err(|message| SandboxError::Validation { message })?;
                let hp =
                    crate::services::preview_password::hash_password(pw).map_err(|reason| {
                        SandboxError::PasswordHashFailed {
                            sandbox_id: public_id_value.clone(),
                            reason,
                        }
                    })?;
                Some(hp)
            }
            None => None,
        };

        let metadata_value = {
            let mut meta = serde_json::Map::new();
            meta.insert("allow_docker".into(), serde_json::json!(req.allow_docker));
            if !req.ports.is_empty() {
                meta.insert("ports".into(), serde_json::json!(req.ports));
            }
            if let Some(disk) = req.disk_size_mb {
                meta.insert("disk_size_mb".into(), serde_json::json!(disk));
            }
            // Marks the sandboxes a caller asked for through the API, the
            // only ones the per-user cap counts. Sandboxes the platform
            // creates on a user's behalf (AI chat, autofix) carry no mark.
            if req.enforce_operator_limits {
                meta.insert("source".into(), serde_json::json!(API_SANDBOX_SOURCE));
            }
            if let Some(application_id) = managed_application_id {
                meta.insert(
                    "managed_application_id".into(),
                    serde_json::json!(application_id),
                );
                if let Some(host_work_dir) = req.host_work_dir_override.as_ref() {
                    meta.insert(
                        "managed_host_work_dir".into(),
                        serde_json::json!(host_work_dir.to_string_lossy()),
                    );
                }
            }
            (!meta.is_empty()).then_some(serde_json::Value::Object(meta))
        };
        // Per-user cap: count and insert in one transaction holding a
        // per-user advisory lock, so a burst of simultaneous creates cannot
        // overshoot it, on this control plane or on another replica sharing
        // the database. The lock is released when the transaction ends.
        let cap_txn = match operator_limits
            .as_ref()
            .filter(|settings| settings.max_sandboxes_per_user.is_some())
        {
            Some(settings) => {
                let txn = self.db.begin().await?;
                txn.execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT pg_advisory_xact_lock($1, $2)",
                    [USER_SANDBOX_CAP_LOCK_NAMESPACE.into(), user_id.into()],
                ))
                .await?;
                let live = Self::live_standalone_sandboxes(&txn, user_id).await?;
                // Returning here drops the transaction, which rolls it back.
                create_policy::check_user_sandbox_cap(user_id, live, settings)?;
                Some(txn)
            }
            None => None,
        };
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(Some(user_id)),
            name: Set(name.clone()),
            status: Set("running".to_string()),
            image: Set(req.image.clone()),
            // The real directory inside the container, not a nominal
            // "/workspace" — that path does not exist in the sandbox image
            // and never has. The agent-run path above already used this
            // constant; this one was hardcoded and wrong, which stayed
            // invisible because `exec` and the filesystem ops take the work
            // dir from the provider handle rather than from this row. It
            // only surfaced once the terminal used the row's value as a
            // PTY cwd and every shell died with ENOENT.
            work_dir: Set(temps_sandbox_providers::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(metadata_value),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(expires_at),
            preview_password_hash: Set(preview.as_ref().map(|p| p.hash.clone())),
            preview_password_hint: Set(preview.as_ref().map(|p| p.hint.clone())),
            preview_access: Set(crate::services::preview_access::initial_access(
                preview.is_some(),
            )),
            lifecycle: Set(lifecycle.as_str().to_string()),
            project_id: Set(req.project_id),
            source_repo_url: Set(source_repo_url),
            node_id: Set(node_id),
            // Part of the create config below, so the sandbox enforces it
            // from its first instant; the row is marked destroyed if the
            // create fails.
            egress_policy: Set(sandboxes::StoredEgressPolicy(egress_policy.clone())),
            ..Default::default()
        };
        let inserted = match cap_txn {
            Some(txn) => match active.insert(&txn).await {
                Ok(row) => txn.commit().await.map(|()| row),
                Err(e) => Err(e),
            },
            None => active.insert(self.db.as_ref()).await,
        };
        let mut row = inserted.map_err(|e| {
            // The chosen node was removed between placement and insert.
            if node_id.is_some()
                && matches!(
                    e.sql_err(),
                    Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_))
                )
            {
                SandboxError::NodeNotFound {
                    node: node_id.map(|id| id.to_string()).unwrap_or_default(),
                }
            } else {
                e.into()
            }
        })?;
        let _create_in_progress = self.creates_in_progress.begin(&public_id_value);

        // Allocate host-side working directory. A worker node derives and
        // creates its own work directory, so nothing is made here for it.
        let using_trusted_work_dir = req.host_work_dir_override.is_some();
        let host_work_dir = req
            .host_work_dir_override
            .clone()
            .unwrap_or_else(|| self.data_root.join(&public_id_value));
        let work_dir_ready = if node_id.is_some() {
            Ok(())
        } else {
            tokio::fs::create_dir_all(&host_work_dir).await
        };
        if let Err(e) = work_dir_ready {
            // Roll back the DB row so a failed-to-create sandbox doesn't
            // linger as a "running" record with no container.
            self.mark_destroyed(row.id).await.ok();
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create work dir: {}", e),
            });
        }

        let container_label = container_label_for(&public_id_value).to_string();

        let config = SandboxCreateConfig {
            node_id,
            owner_user_id: None,
            run_id: row.id,
            container_name_override: Some(container_label.clone()),
            host_work_dir,
            workspace_volume: None,
            image: req.image.clone(),
            cpu_limit: req.cpu_limit,
            memory_limit_mb: req.memory_limit_mb,
            pids_limit: req.pids_limit,
            disk_size_mb: req.disk_size_mb,
            allow_docker: req.allow_docker,
            egress_policy,
            network_mode: None,
            env_vars: req.env,
            idle_timeout: Duration::from_secs(timeout),
            backend,
        };

        // Choose create path: snapshot restore vs normal create.
        let create_result = if let Some(ref artifact) = req.from_snapshot_artifact {
            // ADR-037: restore from snapshot. The provider ensures the image
            // is loaded into the daemon before creating the container.
            self.registry.create_from_snapshot(artifact, config).await
        } else {
            self.registry.create(config).await
        };

        let handle = match create_result {
            Ok(h) => h,
            // The worker refused because the label already names a live
            // sandbox there. That container is not this request's: never
            // destroy it, only retire this row.
            Err(e @ temps_agent_core::error::AgentError::SandboxConflictOnNode { .. }) => {
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(from_agent_error(&public_id_value, e));
            }
            Err(e) => {
                // Tear the container down before touching the work dir.
                // A provider `create` can fail *after* the container is
                // running — the ownership-normalisation step propagates its
                // error — and containers carry `restart_policy: unless-stopped`,
                // so skipping this leaves a live container holding the
                // caller's env vars, unreachable through the API once the
                // row is destroyed, with its /workspace deleted underneath
                // it. Same order as the seeding-failure arm below.
                self.remove_failed_create(&row, "the create failed").await;
                // The work dir was already created above; without this the
                // row goes to "destroyed" and no later `destroy_sandbox`
                // can ever reach the directory again.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                if e.image_pull_failure().is_some() || e.network_pool_exhaustion().is_some() {
                    // The image could not be pulled, or the host has no
                    // address space left for the sandbox's network: report
                    // which, so the caller can tell a missing tag or a full
                    // host from a sandbox runtime fault.
                    return Err(from_agent_error(&public_id_value, e));
                }
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: e.to_string(),
                });
            }
        };

        if node_id.is_some() {
            self.confirm_worker_create(&row, user_id).await?;
        }

        // Persist the *effective* backend + image the provider actually
        // used. When the request omitted them, the host default / backend
        // default decided; the handle carries the real answers. Stored so
        // the API/UI show what actually booted (e.g. "alpine:3.20") instead
        // of a vague "platform default".
        let effective_image =
            (req.image.is_none() && !handle.image.is_empty()).then(|| handle.image.clone());
        if let Err(e) = self
            .record_backend(row.id, handle.backend, effective_image.clone())
            .await
        {
            tracing::warn!(
                "failed to record backend/image for sandbox {}: {}",
                public_id_value,
                e
            );
        }
        // Mirror the recorded values into the in-memory row so the create
        // response matches what a later GET returns (record_backend wrote
        // them to the DB after the initial insert).
        row.backend = Some(handle.backend.to_string());
        if let Some(image) = effective_image {
            row.image = Some(image);
        }

        // If the caller asked us to seed the work dir, run the clone /
        // extract now. On failure we tear the sandbox down so the user
        // isn't left with a half-initialized container that's billing
        // their timeout budget.
        if let Some(source) = source {
            if let Err(e) = self
                .seed_source(row.id, &public_id_value, user_id, &source)
                .await
            {
                tracing::warn!(
                    "Seeding source into sandbox {} failed: {} — destroying",
                    public_id_value,
                    e
                );
                self.remove_failed_create(&row, "seeding its source failed")
                    .await;
                // Seeding runs after the clone/extract, so by here the work
                // dir can already hold a full repository — the largest
                // single thing this service puts on disk.
                if !using_trusted_work_dir {
                    self.remove_work_dir(&public_id_value).await;
                }
                self.mark_destroyed(row.id).await.ok();
                return Err(e);
            }
            self.record_event(
                row.id,
                "source_seeded",
                Some(serde_json::json!({ "type": source_kind(&source) })),
            )
            .await;
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "backend": row.backend,
                "image": row.image,
                "disk_size_mb": req.disk_size_mb,
                "allow_docker": req.allow_docker,
                "node_id": node_id,
            })),
        )
        .await;
        self.record_driver_conditions(row.id, &handle).await;

        tracing::info!(
            "Created standalone sandbox {} (internal {}) for user {}",
            public_id_value,
            row.id,
            user_id
        );
        Ok(row)
    }

    /// An eviction can destroy a worker sandbox's row while its container
    /// is still being created: it finds nothing to remove yet and marks the
    /// row destroyed. Placement also only checks the eviction cordon before
    /// the row exists, so an eviction that started in between has not
    /// listed this row and would leave it behind. Called once the container
    /// exists: if the row is gone (or can't be read), or its node is being
    /// evicted, remove what was just created so no container outlives its
    /// row and an eviction always leaves its node empty. An eviction that
    /// starts after this check lists the row and removes it.
    pub(super) async fn confirm_worker_create(
        &self,
        row: &sandboxes::Model,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        if let Some(node_id) = row.node_id {
            if self.evicting_nodes.contains(node_id) {
                self.remove_failed_create(row, "its node started being evicted during the create")
                    .await;
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: format!(
                        "the sandbox's node (id {node_id}) started evicting its sandboxes while \
                         this one was being created; create it again to place it on another node"
                    ),
                });
            }
        }
        let still_live = sandboxes::Entity::find_by_id(row.id)
            .one(self.db.as_ref())
            .await
            .map(|r| r.is_some_and(|r| r.status != "destroyed"));
        let reason = match still_live {
            Ok(true) => return Ok(()),
            Ok(false) => "the sandbox was destroyed while it was being created \
                          (its node was evicted); create it again"
                .to_string(),
            // Can't tell whether the row survived: fail closed, like every
            // other failure after the container exists.
            Err(e) => {
                format!("could not confirm the sandbox after creating it ({e}); create it again")
            }
        };
        self.remove_failed_create(row, "its row was destroyed or unreadable after the create")
            .await;
        self.mark_destroyed(row.id).await.ok();
        Err(SandboxError::CreateFailed { user_id, reason })
    }

    /// Remove the container of a sandbox whose create failed. A worker
    /// node gets the same per-node deadline as an eviction's container
    /// destroy: a node that accepts the connection but never answers must
    /// not hold the create request for the provider's whole lifecycle
    /// timeout. When the container cannot be confirmed gone, the error log
    /// carries what an operator needs to remove it by hand (node, sandbox,
    /// cleanup command); nothing else tracks it once the row is destroyed.
    pub(super) async fn remove_failed_create(&self, row: &sandboxes::Model, why: &str) {
        let removed = match row.node_id {
            None => self
                .registry
                .destroy(row.id, &row.public_id)
                .await
                .map_err(|e| e.to_string()),
            Some(_) => match tokio::time::timeout(
                CREATE_CLEANUP_DEADLINE,
                self.registry.destroy(row.id, &row.public_id),
            )
            .await
            {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err(format!(
                    "the node did not answer within {}s",
                    CREATE_CLEANUP_DEADLINE.as_secs()
                )),
            },
        };
        let Err(reason) = removed else {
            return;
        };
        let node = match row.node_id {
            Some(id) => placement::node_names(self.db.as_ref(), [id])
                .await
                .ok()
                .and_then(|mut names| names.remove(&id))
                .unwrap_or_else(|| format!("id {id}")),
            None => placement::CONTROL_PLANE_NAME.to_string(),
        };
        tracing::error!(
            sandbox_id = %row.public_id,
            internal_id = row.id,
            node_id = ?row.node_id,
            node = %node,
            cleanup_command = %node_cleanup_command(&row.public_id),
            reason = %bounded_reason(&reason),
            "sandbox create cleanup: {why}, and the sandbox's container could not be \
             confirmed removed; it may still be running on node '{node}'. Run the \
             cleanup command on that node to remove it"
        );
    }
}

/// Guest privilege escalation is a creation-time, explicit backend opt-in.
pub(super) fn validate_allow_docker(
    allow_docker: bool,
    backend: Option<temps_sandbox_providers::SandboxBackend>,
) -> Result<(), SandboxError> {
    if allow_docker && backend != Some(temps_sandbox_providers::SandboxBackend::Microsandbox) {
        return Err(SandboxError::Validation {
            message: "allow_docker requires backend=microsandbox; Docker inside Firecracker is not supported yet".into(),
        });
    }
    Ok(())
}

/// Parse the backend a create request asked for. `None` = host default.
pub(super) fn parse_requested_backend(
    requested: Option<&str>,
) -> Result<Option<temps_sandbox_providers::SandboxBackend>, SandboxError> {
    use temps_sandbox_providers::SandboxBackend;
    match requested {
        None => Ok(None),
        Some("docker") => Ok(Some(SandboxBackend::Docker)),
        Some("firecracker") => Ok(Some(SandboxBackend::Firecracker)),
        Some("microsandbox") => Ok(Some(SandboxBackend::Microsandbox)),
        Some(other) => Err(SandboxError::Validation {
            message: format!(
                "unknown backend '{}' (expected \"docker\", \"firecracker\" or \"microsandbox\")",
                other
            ),
        }),
    }
}

/// Why an explicitly requested backend was rejected on this host. For
/// microsandbox the host probe says exactly what is missing (platform,
/// hypervisor, or runtime) and where to fix it.
pub(super) fn backend_unavailable_message(
    backend: temps_sandbox_providers::SandboxBackend,
) -> String {
    match backend {
        temps_sandbox_providers::SandboxBackend::Microsandbox => {
            let capability = temps_sandbox_providers::microsandbox::microsandbox_capability(
                &temps_sandbox_providers::microsandbox::host_data_dir(),
            );
            match capability.reason {
                Some(reason) => {
                    // Name the command with the server's own data directory:
                    // a bare `temps microsandbox setup` from another shell
                    // can install the runtime somewhere the server never reads.
                    let fix = capability
                        .setup_command
                        .map(|command| format!("; to fix, run `{command}` on the server host"))
                        .unwrap_or_default();
                    format!(
                        "backend 'microsandbox' is not available on this host: {}{} (see {})",
                        reason,
                        fix,
                        temps_sandbox_providers::microsandbox::SETUP_PATH
                    )
                }
                // Ready now but not registered: the server started before
                // the runtime was installed.
                None => "backend 'microsandbox' is not available on this host: the runtime \
                         was installed after the server started; restart temps to enable it"
                    .to_string(),
            }
        }
        temps_sandbox_providers::SandboxBackend::Firecracker => {
            // Registration is decided once, at startup, from the server's own
            // data directory. Name that directory and the command that
            // provisions it: a bare `temps firecracker setup` from a shell
            // without the server's TEMPS_DATA_DIR provisions a different one.
            let data_dir = temps_sandbox_providers::microsandbox::host_data_dir();
            let command = temps_sandbox_providers::microsandbox::command_with_data_dir(
                "temps firecracker setup",
                &data_dir,
            );
            format!(
                "backend 'firecracker' is not available on this host: it is not provisioned \
                 under {} (or the server has not been restarted since setup); run `{}` on \
                 the server host, then restart temps",
                data_dir.join("firecracker").display(),
                command
            )
        }
        temps_sandbox_providers::SandboxBackend::Docker
        | temps_sandbox_providers::SandboxBackend::Local => {
            format!("backend '{}' is not available on this host", backend)
        }
    }
}
