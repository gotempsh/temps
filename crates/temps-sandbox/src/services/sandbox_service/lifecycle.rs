// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pause, resume, restart, resize, idle-timeout bookkeeping, and resolving a
//! public id to a live handle (waking a stopped workspace).

use super::*;

impl SandboxService {
    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Pause a running sandbox (non-destructive). Stops the underlying
    /// container but leaves the DB row, volumes, and filesystem intact so
    /// the user can resume later. Idempotent on already-stopped sandboxes.
    pub async fn pause_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "stopped" {
            return Ok(row);
        }
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "pause".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        match self.registry.stop(row.id, public_id_value).await {
            Ok(()) | Err(temps_ai_agents::error::AgentError::SandboxNotFound { .. }) => {}
            Err(error) => return Err(from_agent_error(public_id_value, error)),
        }
        let now = Utc::now();
        let sandbox_id = row.id;
        // A worker's "not found" above may mean an eviction or destroy
        // removed the container meanwhile: only a row still `running` is
        // moved to `stopped`, never a destroyed one.
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("stopped".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        let updated = self
            .update_row_if(
                &row,
                changes,
                Expect::Status("running"),
                Some("stopped"),
                "pause",
            )
            .await?;
        self.record_event(sandbox_id, "stopped", None).await;
        Ok(updated)
    }

    /// Resume a paused sandbox. Restarts the container and bumps
    /// `expires_at` to `now + timeout_secs` so the user gets a fresh
    /// idle window. Idempotent on already-running sandboxes.
    pub async fn resume_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let mut row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "running" {
            // A row can say `running` for a VM that died with the Temps service
            // or the host. Resume is what the "VM not running" error tells the
            // caller to run, so it must not be a no-op for that row: when the
            // provider proves the VM is down, reconcile the row and boot it.
            // Anything short of proof (alive, or the check failed) keeps the
            // idempotent answer.
            match self.registry.probe(row.id, public_id_value).await {
                VmProbe::Down(cause) if self.mark_vm_lost(&row, &cause, "resume").await => {
                    row.status = "stopped".to_string();
                }
                VmProbe::Alive(_) | VmProbe::Down(_) | VmProbe::Unknown(_) => return Ok(row),
            }
        }
        if row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "resume".into(),
            });
        }
        self.registry
            .start(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        self.enforce_stored_egress_policy(&row).await?;
        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let sandbox_id = row.id;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("running".to_string()),
            last_activity_at: Set(now),
            expires_at: Set(new_expires),
            ..Default::default()
        };
        let updated = self
            .update_row_if(
                &row,
                changes,
                Expect::Status("stopped"),
                Some("running"),
                "resume",
            )
            .await?;
        self.record_event(sandbox_id, "resumed", None).await;
        Ok(updated)
    }

    /// Restart a running sandbox in-place (stop + start). Filesystem and
    /// volumes survive. Rejected on stopped sandboxes (use resume instead).
    pub async fn restart_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "restart".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        self.registry
            .restart(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        self.enforce_stored_egress_policy(&row).await?;
        let now = Utc::now();
        let sandbox_id = row.id;
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            last_activity_at: Set(now),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "restart")
            .await?;
        self.record_event(sandbox_id, "restarted", None).await;
        Ok(updated)
    }

    /// Grow the sandbox's root disk to `new_size_mb`. Firecracker only —
    /// the resize is done offline (stop → grow ext4 → start), so the VM
    /// reboots but its filesystem and data survive. Grow-only. Records a
    /// `resized` event and persists the new size in `metadata`.
    pub async fn resize_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
        new_size_mb: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        const MIN_DISK_MB: u64 = 256;
        const MAX_DISK_MB: u64 = 64 * 1024;
        if !(MIN_DISK_MB..=MAX_DISK_MB).contains(&new_size_mb) {
            return Err(SandboxError::Validation {
                message: format!(
                    "disk_size_mb must be between {} and {}",
                    MIN_DISK_MB, MAX_DISK_MB
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.backend.as_deref() != Some("firecracker") {
            return Err(SandboxError::Validation {
                message: "disk resize is only supported on Firecracker sandboxes".into(),
            });
        }
        let old_mb = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("disk_size_mb"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1024);
        let sandbox_id = row.id;

        self.registry
            .resize_disk(row.id, public_id_value, new_size_mb)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;

        // Persist the new size into metadata (preserving ports).
        let mut meta = row
            .metadata
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        meta.insert("disk_size_mb".into(), serde_json::json!(new_size_mb));
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            metadata: Set(Some(serde_json::Value::Object(meta))),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "resize")
            .await?;

        self.record_event(
            sandbox_id,
            "resized",
            Some(serde_json::json!({ "from_mb": old_mb, "to_mb": new_size_mb })),
        )
        .await;
        Ok(updated)
    }

    /// Extend the sandbox's `expires_at` by `extra_secs`. Does not
    /// change `timeout_secs` — just pushes the deadline forward. Used
    /// by the SDK's `extendTimeout()` so long-running operations can
    /// keep the sandbox alive without recreating it.
    pub async fn extend_timeout(
        &self,
        public_id_value: &str,
        user_id: i32,
        extra_secs: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        if extra_secs == 0 {
            return Err(SandboxError::Validation {
                message: "extra_secs must be greater than zero".into(),
            });
        }
        if extra_secs > MAX_TIMEOUT_SECS {
            return Err(SandboxError::Validation {
                message: format!(
                    "extra_secs {} exceeds maximum of {}",
                    extra_secs, MAX_TIMEOUT_SECS
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let sandbox_id = row.id;
        let new_expires = row.expires_at + chrono::Duration::seconds(extra_secs as i64);
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            expires_at: Set(new_expires),
            last_activity_at: Set(Utc::now()),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "extend timeout")
            .await?;
        self.record_event(
            sandbox_id,
            "timeout_extended",
            Some(serde_json::json!({
                "extra_secs": extra_secs,
                "expires_at": new_expires.to_rfc3339(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Record activity on a sandbox: bump `last_activity_at` **and** push
    /// `expires_at` out to `now + timeout_secs`.
    ///
    /// Moving `expires_at` here is what makes `timeout_secs` an *idle*
    /// timeout, which is what the column has always been documented as
    /// (and what the API calls it). Before this, `expires_at` was set once
    /// at create and never moved by activity, so a sandbox in continuous
    /// use was still stopped at its original deadline.
    ///
    /// The deadline is materialised in the indexed column rather than
    /// computed by the sweeper (`last_activity_at + timeout_secs`) on
    /// purpose: the sweep query rides the partial index on
    /// `(expires_at) WHERE status = 'running'`, and a predicate over an
    /// expression would not use it.
    ///
    /// Called on every exec and filesystem operation, so it must stay a
    /// single UPDATE with no read-modify-write. `timeout_secs` is taken
    /// from the caller (who already has the row) to avoid a SELECT here.
    ///
    /// **The deadline only ever moves forward.** `extend_timeout` (the SDK's
    /// `extendTimeout()`) pushes `expires_at` past `now + timeout_secs`
    /// precisely so a long operation can outlive the idle window — and the
    /// first thing that operation does is exec, which lands here. Assigning
    /// the idle deadline unconditionally would silently undo the extension
    /// the caller just paid for, so the write is `CASE WHEN`-guarded in SQL:
    /// still one statement, still no read-modify-write, but never a
    /// regression. `last_activity_at` is always bumped — it records activity,
    /// not a deadline.
    ///
    /// Swallows DB errors: activity bumps are best-effort, and failing a
    /// user's exec because a bookkeeping write lost a race would be worse
    /// than a sandbox expiring slightly early.
    pub async fn touch(&self, sandbox_id: i32, timeout_secs: i32) {
        let now = Utc::now();
        let deadline = idle_deadline(now, timeout_secs);
        let result = sandboxes::Entity::update_many()
            .col_expr(sandboxes::Column::LastActivityAt, Expr::value(now))
            .col_expr(
                sandboxes::Column::ExpiresAt,
                Expr::cust_with_values(
                    "CASE WHEN expires_at < $1 THEN $2 ELSE expires_at END",
                    [deadline, deadline],
                ),
            )
            .filter(sandboxes::Column::Id.eq(sandbox_id))
            .exec(self.db.as_ref())
            .await;
        if let Err(e) = result {
            tracing::debug!(
                "touch: failed to bump last_activity_at for {}: {}",
                sandbox_id,
                e
            );
        }
    }

    /// Load + authorize + return the internal ID **without** waking a
    /// suspended workspace.
    ///
    /// For endpoints that only need to prove ownership and then answer from
    /// the row or from in-memory state — preview URLs, job listings, job
    /// status. Those are reads; booting a container as a side effect of a
    /// `GET` is a surprise that costs real memory on a 3 vCPU / 4 GB host, and
    /// in the job cases it is pure waste: the in-memory job table was emptied
    /// when the container stopped, so waking it cannot change the answer.
    ///
    /// Use [`Self::resolve_id`] on paths that genuinely need the container
    /// running (exec, filesystem, terminal).
    pub async fn resolve_id_no_wake(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let id = row.id;
        Ok((row, id))
    }

    /// Load + authorize + return the internal ID, or a typed error that
    /// already includes the public ID. Exec/fs modules call this first.
    /// Explicitly stopped ephemeral sandboxes return `InvalidState` (→ HTTP
    /// 409); durable workspaces wake transparently when either the row or the
    /// provider reports that the runtime is stopped.
    ///
    /// **Wakes a suspended workspace as a side effect** (ADR-036), so only
    /// call it where the container must actually be running.
    pub async fn resolve_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if self.creates_in_progress.contains(public_id_value) {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: "starting".to_string(),
                operation: "run exec or filesystem operations until its create finishes \
                            (the first sandbox on a node builds its image, which can take \
                            several minutes); retry when its status is running"
                    .to_string(),
            });
        }
        let lifecycle = SandboxLifecycle::from_row(&row);
        if row.status == "stopped" {
            // Workspaces (ADR-036) are places a human comes back to, so a
            // suspended one wakes instead of erroring. Ephemeral sandboxes
            // keep the 409: their lifetime belongs to whoever created them,
            // and `@vercel/sandbox` consumers rely on a stopped sandbox
            // staying stopped rather than silently restarting under them.
            if lifecycle == SandboxLifecycle::Workspace {
                return self.wake_workspace(row).await;
            }
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: format!(
                    "run exec or filesystem operations until it is resumed \
                     (`bunx @temps-sdk/cli sandbox resume {public_id_value}`)"
                ),
            });
        }

        // Docker (or another provider) can stop a container without passing
        // through SandboxService: daemon pressure, an operator `docker kill`,
        // or the application harness hard deadline are all legitimate ways to
        // reach that state. In those cases the DB row still says `running`.
        // Trusting only the row makes the first application-workspace access
        // fail in `registry.get()` before the later harness recovery path gets
        // a chance to start the container.
        //
        // Only workspaces auto-recover. Ephemeral sandboxes retain their
        // caller-owned lifecycle semantics, and `registry.get()` below the
        // service layer will still return a typed not-found error for them.
        if row.status == "running" && lifecycle == SandboxLifecycle::Workspace {
            match self.registry.get(row.id, public_id_value).await {
                Ok(_) => {}
                Err(
                    temps_ai_agents::error::AgentError::SandboxNotFound { .. }
                    | temps_ai_agents::error::AgentError::SandboxNotRunning { .. },
                ) => {
                    tracing::warn!(
                        sandbox_id = public_id_value,
                        internal_id = row.id,
                        "Workspace row says running but its provider runtime is stopped; waking it on access"
                    );
                    return self.wake_workspace(row).await;
                }
                Err(error) => return Err(from_agent_error(public_id_value, error)),
            }
        }
        let id = row.id;
        Ok((row, id))
    }

    /// Start a suspended workspace's container and reconcile its row to
    /// `running`. Called from [`Self::resolve_id`] on the access path for both
    /// an explicitly stopped row and a stale `running` row whose provider
    /// runtime has died, so the caller experiences recovery as a slow first
    /// request rather than a failure.
    ///
    /// A wake is a container start, not a create: the image is local, the
    /// home volume exists, and the work dir is still populated on the
    /// host. Nothing is re-cloned and nothing is lost.
    ///
    /// If the provider can't start it, the row stays `stopped` and the
    /// caller gets the provider's error — deliberately not swallowed. A
    /// workspace that silently fails to wake and then reports "not found"
    /// on every subsequent call is exactly the debugging dead end
    /// self-hosted users can't escape.
    async fn wake_workspace(
        &self,
        row: sandboxes::Model,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let public_id_value = row.public_id.clone();
        let sandbox_id = row.id;
        tracing::info!(
            "Waking workspace sandbox {} (internal {}) on access",
            public_id_value,
            sandbox_id
        );
        // Bounded: a wake happens inline on a user's request, and a wedged
        // container runtime would otherwise hang that request forever with no
        // way for the caller to tell a slow start from a dead daemon.
        match tokio::time::timeout(
            WAKE_TIMEOUT,
            self.registry.start(sandbox_id, &public_id_value),
        )
        .await
        {
            Ok(result) => result.map_err(|e| from_agent_error(&public_id_value, e))?,
            Err(_) => {
                return Err(SandboxError::InvalidState {
                    sandbox_id: public_id_value.clone(),
                    state: "stopped".to_string(),
                    operation: format!(
                        "wake timed out after {}s; the container runtime did not \
                         start the workspace. Check the daemon, then retry",
                        WAKE_TIMEOUT.as_secs()
                    ),
                });
            }
        }
        self.enforce_stored_egress_policy(&row).await?;

        let now = Utc::now();
        let new_expires = idle_deadline(now, row.timeout_secs);
        let changes = sandboxes::ActiveModel {
            id: Set(row.id),
            status: Set("running".to_string()),
            last_activity_at: Set(now),
            expires_at: Set(new_expires),
            ..Default::default()
        };
        let updated = self
            .update_row_if(&row, changes, Expect::Live, None, "wake")
            .await?;
        self.record_event(sandbox_id, "woken", None).await;
        Ok((updated, sandbox_id))
    }

    /// Build a typed provider error into a `SandboxError` carrying the
    /// public ID. Thin wrapper — module-private modules call this.
    pub(crate) fn provider_err(
        public_id_value: &str,
        err: temps_ai_agents::error::AgentError,
    ) -> SandboxError {
        from_agent_error(public_id_value, err)
    }
}
