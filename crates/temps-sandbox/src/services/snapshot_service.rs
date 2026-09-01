// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for sandbox snapshots (ADR-037).
//!
//! Responsibilities:
//! - Quota enforcement (per-user capture cap, default 10 GiB).
//! - Deduplication of content-addressed artifacts.
//! - Lifecycle management: create / list / get / delete.
//! - Orchestrating the shred → stop → snapshot → restart cycle via the provider.
//! - Nullifying `source_sandbox_id` references when a sandbox is destroyed.
//!
//! This module never imports bollard. All provider interaction goes through
//! `Arc<dyn SandboxProvider>` (ADR-010 boundary).

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use tokio::sync::Mutex;

use temps_agents::error::AgentError;
use temps_agents::sandbox::{SandboxProvider, SnapshotArtifact, SnapshotCompanionArtifact};
use temps_entities::sandbox_snapshots;

use crate::error::SandboxSnapshotError;
use crate::services::public_id;
use crate::services::registry::StandaloneSandboxRegistry;

/// Per-user snapshot storage quota: 10 GiB, enforced during provider capture.
pub const DEFAULT_SNAPSHOT_QUOTA_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// In-process serializer for snapshot creation quota checks.
///
/// Two concurrent `create_snapshot` calls for the same user would both read
/// 0 bytes of storage, both pass the quota check, and both proceed to write —
/// a TOCTOU race. Holding this Mutex across the storage-read + row-insert
/// sequence ensures only one create runs that pair at a time. Snapshot creates
/// are rare and coarse-grained (seconds of work), so a single service-level
/// lock is acceptable.
///
/// For multi-process deployments a `SELECT ... FOR UPDATE` on a per-user row
/// would be needed; the in-process lock covers the single-binary model.
type QuotaLock = Arc<Mutex<()>>;

/// Serializes snapshot artifact publication, restore, and deletion.
///
/// Snapshot files are content-addressed and may be shared by several database
/// rows. Without one lifecycle lock, a failed creator could remove an artifact
/// another creator just reused, two deletes could both retain an orphan, or a
/// delete could remove a file while a restore is reading it. Temps currently
/// runs these services in one process, so a process-wide mutex is the simplest
/// correct ownership boundary. A multi-process deployment will need a database
/// advisory lock keyed by the content digest instead.
type ArtifactLifecycleLock = Arc<Mutex<()>>;

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct PersistedSnapshotArtifactMetadata {
    #[serde(default)]
    primary_digest: Option<String>,
    #[serde(default)]
    image_id: Option<String>,
    #[serde(default)]
    workspace: Option<SnapshotCompanionArtifact>,
}

fn artifact_metadata(
    snapshot_id: &str,
    metadata: Option<&serde_json::Value>,
) -> Result<PersistedSnapshotArtifactMetadata, SandboxSnapshotError> {
    let Some(metadata) = metadata else {
        return Ok(PersistedSnapshotArtifactMetadata::default());
    };

    serde_json::from_value(metadata.clone()).map_err(|error| {
        SandboxSnapshotError::InvalidArtifactMetadata {
            snapshot_id: snapshot_id.to_string(),
            reason: format!("decode persisted artifact fields: {}", error),
        }
    })
}

/// Service for snapshot CRUD and lifecycle management.
pub struct SnapshotService {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    /// The sandbox provider (Docker, Firecracker, …) — used for take_snapshot
    /// and create_from_snapshot. Held as Arc<dyn …> for provider boundary.
    provider: Arc<dyn SandboxProvider>,
    /// Per-user quota in bytes. Configurable via AgentSandboxSettings in v2;
    /// v1 uses this hard-coded default.
    quota_bytes: u64,
    /// Per-user lock to serialize concurrent quota-check + insert sequences
    /// and eliminate the TOCTOU race in create_snapshot.
    quota_lock: QuotaLock,
    /// Process-wide guard for the shared, content-addressed artifact lifecycle.
    artifact_lifecycle_lock: ArtifactLifecycleLock,
}

impl SnapshotService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        registry: Arc<StandaloneSandboxRegistry>,
        provider: Arc<dyn SandboxProvider>,
    ) -> Self {
        Self {
            db,
            registry,
            provider,
            quota_bytes: DEFAULT_SNAPSHOT_QUOTA_BYTES,
            quota_lock: Arc::new(Mutex::new(())),
            artifact_lifecycle_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_quota(mut self, quota_bytes: u64) -> Self {
        self.quota_bytes = quota_bytes;
        self
    }

    /// Acquire exclusive access to shared snapshot artifacts.
    ///
    /// The restore handler holds this guard from artifact resolution through
    /// provider restore so deletion cannot invalidate a verified path between
    /// those two operations. Capture and delete acquire the same guard inside
    /// this service.
    pub async fn acquire_artifact_lifecycle(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.artifact_lifecycle_lock.clone().lock_owned().await
    }

    // ── Create ────────────────────────────────────────────────────────────────

    /// Create a snapshot row in `creating` state, then call the provider.
    /// On success, transitions the row to `ready` and returns the model.
    /// On failure, transitions to `failed` (row kept for audit).
    ///
    /// Security protocol (ADR-037 §4):
    /// 1. Shred `/etc/temps/credential-daemon.env` via exec_as_root WHILE the
    ///    sandbox is still running — any failure hard-aborts the snapshot.
    /// 2. Stop the sandbox (quiesce for filesystem consistency).
    /// 3. Call provider.take_snapshot (backend-specific capture and export).
    ///
    /// The sandbox is restarted afterwards unless it was already stopped on entry.
    pub async fn create_snapshot(
        &self,
        sandbox_internal_id: i32,
        sandbox_public_id: &str,
        user_id: i32,
        project_id: Option<i32>,
        label: Option<String>,
    ) -> Result<sandbox_snapshots::Model, SandboxSnapshotError> {
        // ── Quota check (serialized to close TOCTOU race) ────────────────────
        // Hold the service-level lock across the storage-read + row-insert so
        // two concurrent creates can't both pass the quota check and both write.
        //
        // TOCTOU close strategy: we cap concurrent `creating` rows per user to
        // exactly 1. The lock alone prevents in-process concurrent races, but
        // between the lock release (after insert) and the `take_snapshot` call
        // (which runs outside the lock and can take many seconds), another
        // request could enter the critical section with 0 bytes of storage used
        // (because the creating row hasn't been finalized yet). By rejecting any
        // new create while a `creating` row already exists for this user we
        // close that window completely with a single cheap COUNT query, at the
        // cost of serializing snapshot creation per user (which is already the
        // expected access pattern — users rarely snapshot the same sandbox
        // twice simultaneously, and if they do, the second call returns 422
        // immediately rather than silently racing past the quota).
        let (row, max_snapshot_bytes) = {
            let _quota_guard = self.quota_lock.lock().await;

            // Reject if any in-flight creating row exists for this user.
            // This closes the TOCTOU window: a creating row that hasn't been
            // finalized yet (size_bytes = 0) cannot be counted in the byte
            // total, so without this guard a flood of sequential requests
            // could each pass the quota check before any of them commits.
            let in_flight_count = sandbox_snapshots::Entity::find()
                .filter(sandbox_snapshots::Column::UserId.eq(user_id))
                .filter(sandbox_snapshots::Column::Status.eq("creating"))
                .count(self.db.as_ref())
                .await
                .map_err(SandboxSnapshotError::Database)?;

            if in_flight_count > 0 {
                return Err(SandboxSnapshotError::SnapshotInProgress { user_id });
            }

            let storage = self.storage_summary(user_id).await?;
            if storage.total_bytes >= self.quota_bytes {
                return Err(SandboxSnapshotError::QuotaExceeded {
                    user_id,
                    used_bytes: storage.total_bytes,
                    quota_bytes: self.quota_bytes,
                });
            }

            // ── Create the placeholder row ────────────────────────────────────
            let public_id = public_id::generate_with_prefix("snap");
            let now = Utc::now();

            let active = sandbox_snapshots::ActiveModel {
                public_id: Set(public_id.clone()),
                user_id: Set(user_id),
                project_id: Set(project_id),
                source_sandbox_id: Set(Some(sandbox_internal_id)),
                label: Set(label.clone()),
                status: Set("creating".to_string()),
                backend: Set(String::new()), // filled in after provider returns
                content_digest: Set(String::new()), // filled in after
                content_path: Set(String::new()), // filled in after
                size_bytes: Set(0),
                image_ref: Set(None),
                metadata: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };

            let row = active
                .insert(self.db.as_ref())
                .await
                .map_err(SandboxSnapshotError::Database)?;
            (row, self.quota_bytes.saturating_sub(storage.total_bytes))
            // _quota_guard drops here — next create can now enter the critical section
        };

        // Keep publication and database finalization atomic relative to other
        // snapshot creates, restores, and deletes. Providers publish immutable
        // content-addressed files before this method finalizes their rows.
        let _artifact_lifecycle_guard = self.acquire_artifact_lifecycle().await;

        // ── Get the sandbox handle ────────────────────────────────────────────
        let handle = match self
            .registry
            .get(sandbox_internal_id, sandbox_public_id)
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                let reason = format!("sandbox registry lookup failed: {}", error);
                let _ = self.mark_failed(row.id, &reason).await;
                return Err(SandboxSnapshotError::SandboxNotFound {
                    sandbox_id: sandbox_public_id.to_string(),
                });
            }
        };

        // ── Stop the sandbox (quiesce for commit consistency) ─────────────────
        let was_running = {
            use temps_entities::sandboxes;
            let sb = match sandboxes::Entity::find_by_id(sandbox_internal_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(sandbox)) => sandbox,
                Ok(None) => {
                    let reason = "sandbox database row disappeared during snapshot creation";
                    let _ = self.mark_failed(row.id, reason).await;
                    return Err(SandboxSnapshotError::SandboxNotFound {
                        sandbox_id: sandbox_public_id.to_string(),
                    });
                }
                Err(error) => {
                    let reason = format!("load sandbox status: {}", error);
                    let _ = self.mark_failed(row.id, &reason).await;
                    return Err(SandboxSnapshotError::Database(error));
                }
            };
            sb.status == "running"
        };

        // ── Guard: v1 only supports running sandboxes ─────────────────────────
        // The shred step (exec_as_root) requires a live container. Stopped
        // sandboxes would fail with a Docker-level "container is not running"
        // error that would surface as ScrubFailed — a misleading error that
        // implies a security issue rather than a lifecycle constraint.
        // Returning a clear SandboxNotRunning here lets the caller fix the
        // actual problem (resume the sandbox) instead of debugging a false
        // ScrubFailed. ADR-037 v1: only running sandboxes are supported.
        if !was_running {
            let _ = self
                .mark_failed(
                    row.id,
                    "snapshot rejected: sandbox is not running (v1 requires a running sandbox)",
                )
                .await;
            return Err(SandboxSnapshotError::SandboxNotRunning {
                sandbox_id: sandbox_public_id.to_string(),
            });
        }

        // ── Step 1 of security protocol: shred credential file BEFORE stop ────
        // The credential-daemon env file contains the git-provider token; it
        // must be removed while the container is still running (exec_as_root
        // requires a live container). Failure is a hard abort — never proceed
        // to commit if the file can't be confirmed absent.
        let shred_result = self
            .provider
            .exec_as_root(
                &handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    // shred overwrites before unlinking; fall back to rm -f.
                    // Either way, `test ! -f` verifies the file is gone.
                    "shred -u /etc/temps/credential-daemon.env 2>/dev/null \
                     || rm -f /etc/temps/credential-daemon.env; \
                     test ! -f /etc/temps/credential-daemon.env"
                        .to_string(),
                ],
                Default::default(),
                None,
            )
            .await;

        match shred_result {
            Err(e) => {
                let reason = format!(
                    "exec_as_root failed while shredding credential-daemon.env: {}",
                    e
                );
                let _ = self.mark_failed(row.id, &reason).await;
                return Err(SandboxSnapshotError::ScrubFailed {
                    sandbox_id: sandbox_public_id.to_string(),
                    reason,
                });
            }
            Ok(result) if result.exit_code != 0 => {
                let reason = format!(
                    "credential-daemon.env shred exited with code {} \
                     (file may still be present — snapshot aborted)",
                    result.exit_code
                );
                let _ = self.mark_failed(row.id, &reason).await;
                return Err(SandboxSnapshotError::ScrubFailed {
                    sandbox_id: sandbox_public_id.to_string(),
                    reason,
                });
            }
            Ok(_) => {
                tracing::debug!(
                    sandbox_id = %sandbox_public_id,
                    "snapshot: credential-daemon.env shredded successfully"
                );
            }
        }

        if was_running {
            if let Err(error) = self.provider.stop(&handle).await {
                let reason = format!("stop sandbox before snapshot: {}", error);
                let _ = self.mark_failed(row.id, &reason).await;
                return Err(SandboxSnapshotError::Provider(error));
            }
        }

        // ── Call provider to take the snapshot ────────────────────────────────
        let artifact_result = self
            .provider
            .take_snapshot(&handle, label, max_snapshot_bytes)
            .await;

        // Restart the sandbox regardless of whether snapshot succeeded
        if was_running {
            if let Err(e) = self.provider.start(&handle).await {
                tracing::warn!(
                    sandbox_id = %sandbox_public_id,
                    "snapshot: failed to restart sandbox after snapshot: {}",
                    e
                );
            }
        }

        match artifact_result {
            Err(AgentError::SnapshotSizeLimitExceeded { .. }) => {
                let reason = format!(
                    "snapshot exceeded the remaining quota of {} bytes",
                    max_snapshot_bytes
                );
                let _ = self.mark_failed(row.id, &reason).await;
                Err(SandboxSnapshotError::QuotaExceeded {
                    user_id,
                    used_bytes: self.quota_bytes,
                    quota_bytes: self.quota_bytes,
                })
            }
            Err(e) => {
                // Transition to failed, keep the row for audit.
                let _ = self.mark_failed(row.id, &e.to_string()).await;
                Err(SandboxSnapshotError::Provider(e))
            }
            Ok(artifact) => {
                if artifact.size_bytes > max_snapshot_bytes {
                    let reason = format!(
                        "snapshot produced {} bytes, exceeding the remaining quota of {} bytes",
                        artifact.size_bytes, max_snapshot_bytes
                    );
                    self.compensate_failed_capture(row.id, &artifact, &reason)
                        .await;
                    return Err(SandboxSnapshotError::QuotaExceeded {
                        user_id,
                        used_bytes: self
                            .quota_bytes
                            .saturating_sub(max_snapshot_bytes)
                            .saturating_add(artifact.size_bytes),
                        quota_bytes: self.quota_bytes,
                    });
                }

                // ── Check deduplication ───────────────────────────────────────
                let existing = sandbox_snapshots::Entity::find()
                    .filter(
                        sandbox_snapshots::Column::ContentDigest
                            .eq(artifact.content_digest.clone()),
                    )
                    .filter(sandbox_snapshots::Column::Status.eq("ready"))
                    .one(self.db.as_ref())
                    .await;
                let existing = match existing {
                    Ok(existing) => existing,
                    Err(error) => {
                        let reason = format!("query snapshot deduplication index: {}", error);
                        self.compensate_failed_capture(row.id, &artifact, &reason)
                            .await;
                        return Err(SandboxSnapshotError::Database(error));
                    }
                };

                if let Some(dup) = existing {
                    let mut deduplicated = artifact.clone();
                    deduplicated.content_path = Path::new(&dup.content_path).to_path_buf();
                    let duplicate_metadata =
                        match artifact_metadata(&dup.public_id, dup.metadata.as_ref()) {
                            Ok(metadata) => metadata,
                            Err(error) => {
                                self.compensate_failed_capture(
                                    row.id,
                                    &artifact,
                                    &error.to_string(),
                                )
                                .await;
                                return Err(error);
                            }
                        };
                    deduplicated.primary_digest = duplicate_metadata
                        .primary_digest
                        .unwrap_or_else(|| dup.content_digest.clone());
                    deduplicated.image_id = duplicate_metadata.image_id;
                    // Same content already on disk — remove the duplicate file
                    // if the provider wrote a different one.
                    if artifact.content_path != Path::new(&dup.content_path) {
                        // Use tokio::fs to avoid blocking on disk I/O.
                        let _ = tokio::fs::remove_file(&artifact.content_path).await;
                    }
                    if let Some(workspace) = &artifact.workspace {
                        let duplicate_workspace = duplicate_metadata.workspace;
                        if duplicate_workspace
                            .as_ref()
                            .map(|stored| &stored.content_path)
                            != Some(&workspace.content_path)
                        {
                            let _ = tokio::fs::remove_file(&workspace.content_path).await;
                        }
                        deduplicated.workspace = duplicate_workspace;
                    }
                    // Update the creating row to point at the existing artifact.
                    return match self
                        .finalize_row(row.id, &deduplicated, dup.content_path.clone())
                        .await
                    {
                        Ok(updated) => Ok(updated),
                        Err(error) => {
                            self.compensate_failed_capture(
                                row.id,
                                &deduplicated,
                                &error.to_string(),
                            )
                            .await;
                            Err(error)
                        }
                    };
                }

                // ── Finalize the row ──────────────────────────────────────────
                match self
                    .finalize_row(
                        row.id,
                        &artifact,
                        artifact.content_path.to_string_lossy().to_string(),
                    )
                    .await
                {
                    Ok(final_row) => Ok(final_row),
                    Err(error) => {
                        self.compensate_failed_capture(row.id, &artifact, &error.to_string())
                            .await;
                        Err(error)
                    }
                }
            }
        }
    }

    /// Mark a post-provider failure and reclaim the artifact only when the
    /// database proves no ready snapshot references its logical digest.
    ///
    /// If the reference query itself fails, retain the immutable files and
    /// image tag for a later reconciler rather than risk deleting an artifact
    /// shared by a ready row. The lifecycle mutex held by `create_snapshot`
    /// prevents the reference count from changing during this compensation.
    async fn compensate_failed_capture(
        &self,
        row_id: i32,
        artifact: &SnapshotArtifact,
        reason: &str,
    ) {
        let _ = self.mark_failed(row_id, reason).await;

        let ready_references = sandbox_snapshots::Entity::find()
            .filter(sandbox_snapshots::Column::ContentDigest.eq(artifact.content_digest.clone()))
            .filter(sandbox_snapshots::Column::Status.eq("ready"))
            .count(self.db.as_ref())
            .await;
        match ready_references {
            Ok(0) => {
                if let Err(error) = tokio::fs::remove_file(&artifact.content_path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            snapshot_id = row_id,
                            path = %artifact.content_path.display(),
                            "snapshot compensation: failed to remove primary artifact: {}",
                            error
                        );
                    }
                }
                if let Some(workspace) = &artifact.workspace {
                    if let Err(error) = tokio::fs::remove_file(&workspace.content_path).await {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(
                                snapshot_id = row_id,
                                path = %workspace.content_path.display(),
                                "snapshot compensation: failed to remove workspace artifact: {}",
                                error
                            );
                        }
                    }
                }
                if let Some(image_ref) = artifact.image_ref.as_deref() {
                    if let Err(error) = self.provider.delete_image(image_ref).await {
                        tracing::warn!(
                            snapshot_id = row_id,
                            image_ref,
                            "snapshot compensation: failed to remove image: {}",
                            error
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    snapshot_id = row_id,
                    digest = %artifact.content_digest,
                    "snapshot compensation retained artifact because reference lookup failed: {}",
                    error
                );
            }
        }
    }

    async fn mark_failed(&self, row_id: i32, reason: &str) -> Result<(), SandboxSnapshotError> {
        tracing::warn!(snapshot_id = row_id, reason = %reason, "snapshot failed");
        let mut active: sandbox_snapshots::ActiveModel =
            sandbox_snapshots::Entity::find_by_id(row_id)
                .one(self.db.as_ref())
                .await
                .map_err(SandboxSnapshotError::Database)?
                .ok_or_else(|| SandboxSnapshotError::NotFound {
                    snapshot_id: row_id.to_string(),
                })?
                .into();
        active.status = Set("failed".to_string());
        active.updated_at = Set(Utc::now());
        active
            .update(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;
        Ok(())
    }

    async fn finalize_row(
        &self,
        row_id: i32,
        artifact: &SnapshotArtifact,
        content_path: String,
    ) -> Result<sandbox_snapshots::Model, SandboxSnapshotError> {
        let mut active: sandbox_snapshots::ActiveModel =
            sandbox_snapshots::Entity::find_by_id(row_id)
                .one(self.db.as_ref())
                .await
                .map_err(SandboxSnapshotError::Database)?
                .ok_or_else(|| SandboxSnapshotError::NotFound {
                    snapshot_id: row_id.to_string(),
                })?
                .into();

        active.status = Set("ready".to_string());
        active.backend = Set(artifact.backend.to_string());
        active.content_digest = Set(artifact.content_digest.clone());
        active.content_path = Set(content_path);
        active.size_bytes = Set(artifact.size_bytes as i64);
        active.image_ref = Set(artifact.image_ref.clone());
        active.metadata = Set(Some(serde_json::json!(PersistedSnapshotArtifactMetadata {
            primary_digest: Some(artifact.primary_digest.clone()),
            image_id: artifact.image_id.clone(),
            workspace: artifact.workspace.clone(),
        })));
        active.updated_at = Set(Utc::now());

        let model = active
            .update(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;
        Ok(model)
    }

    // ── List ──────────────────────────────────────────────────────────────────

    /// List snapshots owned by `user_id`. Filters by status and project_id.
    pub async fn list_snapshots(
        &self,
        user_id: i32,
        project_id: Option<i32>,
        status: Option<String>,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<sandbox_snapshots::Model>, u64), SandboxSnapshotError> {
        let page_size = page_size.clamp(1, 100);
        let mut query = sandbox_snapshots::Entity::find()
            .filter(sandbox_snapshots::Column::UserId.eq(user_id))
            .filter(sandbox_snapshots::Column::Status.ne("deleted"));

        if let Some(pid) = project_id {
            query = query.filter(sandbox_snapshots::Column::ProjectId.eq(pid));
        }
        if let Some(s) = status {
            query = query.filter(sandbox_snapshots::Column::Status.eq(s));
        }

        let paginator = query
            .order_by_desc(sandbox_snapshots::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator
            .num_items()
            .await
            .map_err(SandboxSnapshotError::Database)?;
        let items = paginator
            .fetch_page(page.saturating_sub(1))
            .await
            .map_err(SandboxSnapshotError::Database)?;

        Ok((items, total))
    }

    // ── Get ───────────────────────────────────────────────────────────────────

    pub async fn get_snapshot(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<sandbox_snapshots::Model, SandboxSnapshotError> {
        let row = sandbox_snapshots::Entity::find()
            .filter(sandbox_snapshots::Column::PublicId.eq(public_id))
            .one(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?
            .ok_or_else(|| SandboxSnapshotError::NotFound {
                snapshot_id: public_id.to_string(),
            })?;

        if row.user_id != user_id {
            return Err(SandboxSnapshotError::NotFound {
                snapshot_id: public_id.to_string(),
            });
        }

        Ok(row)
    }

    // ── Delete ────────────────────────────────────────────────────────────────

    /// Soft-delete a snapshot row. Removes the artifact from disk if no other
    /// `ready` row shares the `content_digest`.
    pub async fn delete_snapshot(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<(), SandboxSnapshotError> {
        // The row reference-count check, soft delete, and artifact removal are
        // one lifecycle operation. This also excludes a restore that is reading
        // the artifact and a creator that is publishing/finalizing it.
        let _artifact_lifecycle_guard = self.acquire_artifact_lifecycle().await;
        let row = self.get_snapshot(user_id, public_id).await?;

        if row.status == "deleted" {
            return Ok(()); // Idempotent
        }

        let metadata = artifact_metadata(public_id, row.metadata.as_ref())?;
        let workspace = metadata.workspace;

        // Check if another ready row shares the same digest.
        let sharing_count = sandbox_snapshots::Entity::find()
            .filter(sandbox_snapshots::Column::ContentDigest.eq(row.content_digest.clone()))
            .filter(sandbox_snapshots::Column::Status.eq("ready"))
            .filter(sandbox_snapshots::Column::Id.ne(row.id))
            .count(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;

        // Soft-delete the row first (so the file removal is the side-effect,
        // not the source of truth).
        let mut active: sandbox_snapshots::ActiveModel = row.clone().into();
        active.status = Set("deleted".to_string());
        active.updated_at = Set(Utc::now());
        active
            .update(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;

        // Remove the tarball only when no other row references this digest.
        // Use tokio::fs to avoid blocking the async runtime on disk I/O.
        if sharing_count == 0 && !row.content_path.is_empty() {
            if let Err(e) = tokio::fs::remove_file(&row.content_path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        snapshot_id = %public_id,
                        path = %row.content_path,
                        "snapshot delete: failed to remove tarball: {}",
                        e
                    );
                }
            }
            if let Some(workspace) = workspace {
                if let Err(e) = tokio::fs::remove_file(&workspace.content_path).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            snapshot_id = %public_id,
                            path = %workspace.content_path.display(),
                            "snapshot delete: failed to remove workspace artifact: {}",
                            e
                        );
                    }
                }
            }
        }

        // The content-addressed Docker tag is shared with deduplicated rows,
        // just like the files. Remove it only with the final ready reference.
        if sharing_count == 0 {
            if let Some(ref image_ref) = row.image_ref {
                if !image_ref.is_empty() {
                    if let Err(e) = self.provider.delete_image(image_ref).await {
                        tracing::warn!(
                            snapshot_id = %public_id,
                            image_ref = %image_ref,
                            "snapshot delete: failed to remove Docker image (best-effort): {}",
                            e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    // ── Storage summary ───────────────────────────────────────────────────────

    /// Total snapshot bytes on disk for a user (over all `ready` snapshots).
    pub async fn storage_summary(
        &self,
        user_id: i32,
    ) -> Result<StorageSummary, SandboxSnapshotError> {
        // Fetch all ready snapshot rows for this user and sum client-side.
        // For typical snapshot counts (< thousands) this is fine; a DB-side
        // SUM can be added via raw SQL in a follow-up if needed.
        let rows = sandbox_snapshots::Entity::find()
            .filter(sandbox_snapshots::Column::UserId.eq(user_id))
            .filter(sandbox_snapshots::Column::Status.eq("ready"))
            .all(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;

        let total_bytes: u64 = rows.iter().map(|r| r.size_bytes as u64).sum();
        let snapshot_count = rows.len() as u32;

        // Available disk — best effort (falls back to 0 if the snaps dir doesn't exist yet).
        let available_disk_bytes = available_disk_space();

        Ok(StorageSummary {
            total_bytes,
            snapshot_count,
            quota_bytes: self.quota_bytes,
            available_disk_bytes,
        })
    }

    // ── Nullify source_sandbox_id on sandbox destroy ──────────────────────────

    /// Called by `SandboxService::destroy_sandbox` to nullify `source_sandbox_id`
    /// on any snapshot that references the sandbox being destroyed. This avoids
    /// dangling integer references without a FK that would cascade deletes.
    pub async fn nullify_source_sandbox(
        &self,
        sandbox_internal_id: i32,
    ) -> Result<(), SandboxSnapshotError> {
        use sea_orm::prelude::*;
        use sea_orm::sea_query::Expr;

        sandbox_snapshots::Entity::update_many()
            .col_expr(
                sandbox_snapshots::Column::SourceSandboxId,
                Expr::value(sea_orm::Value::Int(None)),
            )
            .filter(sandbox_snapshots::Column::SourceSandboxId.eq(sandbox_internal_id))
            .exec(self.db.as_ref())
            .await
            .map_err(SandboxSnapshotError::Database)?;
        Ok(())
    }

    /// Resolve a snapshot to a `SnapshotArtifact` for use with
    /// `provider.create_from_snapshot`. Verifies the caller owns the snapshot,
    /// that it is `ready`, and that the artifact file exists.
    pub async fn resolve_for_restore(
        &self,
        user_id: i32,
        public_id: &str,
        target_backend: Option<&str>,
    ) -> Result<SnapshotArtifact, SandboxSnapshotError> {
        let row = self.get_snapshot(user_id, public_id).await?;

        if row.status != "ready" {
            return Err(SandboxSnapshotError::NotReady {
                snapshot_id: public_id.to_string(),
                status: row.status.clone(),
            });
        }

        // Cross-backend restore check.
        let target_backend = target_backend.unwrap_or(&row.backend);
        if row.backend != target_backend {
            return Err(SandboxSnapshotError::CrossBackendRestore {
                snapshot_backend: row.backend.clone(),
                target_backend: target_backend.to_string(),
            });
        }

        let content_path = std::path::PathBuf::from(&row.content_path);

        // Fast O(1) digest-consistency check: snapshot files are stored as
        // `<digest>.tar`, so the path stem must equal the stored content_digest.
        // Run this before the exists check so callers get the more specific
        // DigestMismatch error when the row is corrupted, rather than the
        // generic ArtifactMissing. A mismatch means the DB row and the
        // artifact path have drifted — either from manual intervention or a
        // dedup bug — and restoring from this artifact would be unsafe.
        if !row.content_digest.is_empty() {
            let path_stem = content_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if path_stem != row.content_digest.as_str() {
                return Err(SandboxSnapshotError::DigestMismatch {
                    expected: row.content_digest.clone(),
                    actual: path_stem.to_string(),
                });
            }
        }

        // Use async I/O for the existence check (consistent with the rest of
        // this crate — blocking fs calls on the async runtime are forbidden).
        if !tokio::fs::try_exists(&content_path)
            .await
            .map_err(SandboxSnapshotError::Io)?
        {
            return Err(SandboxSnapshotError::ArtifactMissing {
                path: row.content_path.clone(),
            });
        }

        let metadata = artifact_metadata(public_id, row.metadata.as_ref())?;
        if row.backend == "docker"
            && (metadata.primary_digest.is_none()
                || metadata.image_id.is_none()
                || metadata.workspace.is_none())
        {
            return Err(SandboxSnapshotError::LegacyArtifactUnsupported {
                snapshot_id: public_id.to_string(),
                reason: "Docker snapshots created before workspace companions and immutable image IDs were recorded cannot be restored safely; create a new snapshot from the source sandbox"
                    .to_string(),
            });
        }
        let workspace = metadata.workspace;
        if let Some(workspace) = &workspace {
            if !tokio::fs::try_exists(&workspace.content_path)
                .await
                .map_err(SandboxSnapshotError::Io)?
            {
                return Err(SandboxSnapshotError::ArtifactMissing {
                    path: workspace.content_path.to_string_lossy().to_string(),
                });
            }
        }

        let backend: temps_agents::sandbox::SandboxBackend =
            row.backend
                .parse()
                .map_err(|_| SandboxSnapshotError::NotFound {
                    snapshot_id: public_id.to_string(),
                })?;

        Ok(SnapshotArtifact {
            content_path,
            content_digest: row.content_digest.clone(),
            primary_digest: metadata
                .primary_digest
                .unwrap_or_else(|| row.content_digest.clone()),
            size_bytes: row.size_bytes as u64,
            backend,
            image_ref: row.image_ref.clone(),
            image_id: metadata.image_id,
            workspace,
        })
    }
}

/// Storage summary returned by `GET /v1/sandbox-snapshots/storage-summary`.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct StorageSummary {
    /// Total bytes used by all `ready` snapshots for this user.
    pub total_bytes: u64,
    /// Number of `ready` snapshots.
    pub snapshot_count: u32,
    /// Per-user quota in bytes.
    pub quota_bytes: u64,
    /// Available bytes on the snapshots filesystem, or `null` when the
    /// platform check is not yet implemented (deferred — see `available_disk_space()`).
    /// API consumers MUST treat `null` as "unknown" rather than "zero bytes
    /// available". A `Some(0)` would incorrectly block snapshot creation.
    pub available_disk_bytes: Option<u64>,
}

/// Best-effort check of available disk space on the snapshots directory.
///
/// Returns `None` while the real `statvfs`-based implementation is deferred
/// (requires the `nix` crate or a platform-specific syscall). Callers MUST
/// treat `None` as "unknown" — not "zero bytes available" — so the UI can
/// display "unknown" rather than incorrectly blocking snapshot creation.
///
/// A future version should use `nix::sys::statvfs::statvfs` or equivalent
/// to return `Some(available_bytes)`.
fn available_disk_space() -> Option<u64> {
    // Implementation deferred: no platform-independent statvfs in std.
    // Return None so API consumers see "unknown" rather than "0 bytes".
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_agents::error::AgentError;
    use temps_agents::sandbox::{
        SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
        SnapshotArtifact,
    };
    use temps_entities::{sandbox_snapshots, sandboxes};

    // ── Test provider ─────────────────────────────────────────────────────────

    /// A minimal fake provider that supports the full create_snapshot
    /// lifecycle (exec, stop, take_snapshot, start).
    ///
    /// Knobs:
    /// - `fail_take_snapshot`: simulates a provider-level snapshot failure.
    /// - `fail_exec_as_root`: simulates a shred/exec failure.
    /// - `fail_delete_image`: simulates a best-effort image cleanup failure
    ///   (used to verify delete_snapshot still returns Ok on image removal errors).
    struct FakeSnapshotProvider {
        fail_take_snapshot: bool,
        fail_size_limit: bool,
        fail_exec_as_root: bool,
        fail_stop: bool,
        fail_recover: bool,
        fail_delete_image: bool,
        artifact: Option<SnapshotArtifact>,
        delete_image_calls: Arc<AtomicUsize>,
    }

    impl FakeSnapshotProvider {
        fn new() -> Self {
            Self {
                fail_take_snapshot: false,
                fail_size_limit: false,
                fail_exec_as_root: false,
                fail_stop: false,
                fail_recover: false,
                fail_delete_image: false,
                artifact: None,
                delete_image_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    fn fake_snap_handle_named(name: &str) -> SandboxHandle {
        SandboxHandle {
            sandbox_id: format!("container-{}", name),
            sandbox_name: format!("temps-sandbox-{}", name),
            work_dir: std::path::PathBuf::from("/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeSnapshotProvider {
        async fn create(&self, _config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            Ok(fake_snap_handle_named("test"))
        }

        async fn exec(
            &self,
            handle: &SandboxHandle,
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<temps_agents::ai_cli::OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            if self.fail_exec_as_root {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: "shred failed (fake provider)".into(),
                });
            }
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(true)
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(vec![])
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn destroy(&self, _handle: &SandboxHandle, _purge: bool) -> Result<(), AgentError> {
            Ok(())
        }

        async fn stop(&self, handle: &SandboxHandle) -> Result<(), AgentError> {
            if self.fail_stop {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: "stop failed (fake provider)".into(),
                });
            }
            Ok(())
        }

        async fn start(&self, _handle: &SandboxHandle) -> Result<(), AgentError> {
            Ok(())
        }

        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }

        /// Required to make registry.get() succeed (in-memory miss → provider lookup).
        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            if self.fail_recover {
                return Ok(None);
            }
            Ok(Some(fake_snap_handle_named(container_name)))
        }

        fn name(&self) -> &str {
            "fake_snap"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".into()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".into())
        }

        async fn take_snapshot(
            &self,
            handle: &SandboxHandle,
            _label: Option<String>,
            max_size_bytes: u64,
        ) -> Result<SnapshotArtifact, AgentError> {
            if self.fail_size_limit {
                return Err(AgentError::SnapshotSizeLimitExceeded {
                    sandbox_id: handle.sandbox_id.clone(),
                    stage: "fake capture".to_string(),
                    max_size_bytes,
                });
            }
            if self.fail_take_snapshot {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: handle.sandbox_id.clone(),
                    reason: "take_snapshot: provider failed (fake)".into(),
                });
            }
            if let Some(artifact) = &self.artifact {
                return Ok(artifact.clone());
            }
            Ok(SnapshotArtifact {
                content_path: std::path::PathBuf::from("/tmp/test-snap.tar"),
                content_digest: "sha256fakedigest1234567890abcdef01234567".to_string(),
                primary_digest: "sha256fakedigest1234567890abcdef01234567".to_string(),
                size_bytes: 4096,
                backend: SandboxBackend::Docker,
                image_ref: Some("temps-snapshot/test:latest".to_string()),
                image_id: Some("sha256:fake".to_string()),
                workspace: None,
            })
        }

        async fn delete_image(&self, image_ref: &str) -> Result<(), AgentError> {
            self.delete_image_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_delete_image {
                return Err(AgentError::SandboxExecFailed {
                    run_id: 0,
                    sandbox_id: image_ref.to_string(),
                    reason: "delete_image: provider failed (fake knob)".into(),
                });
            }
            Ok(())
        }
    }

    /// Build a SnapshotService that uses the given provider.
    fn make_service_with_provider<P: SandboxProvider + 'static>(
        db: Arc<DatabaseConnection>,
        provider: P,
    ) -> SnapshotService {
        let provider_arc = Arc::new(provider) as Arc<dyn SandboxProvider>;
        let registry = Arc::new(StandaloneSandboxRegistry::new(provider_arc.clone()));
        SnapshotService::new(db, registry, provider_arc)
    }

    fn make_sandbox_row(id: i32, public_id: &str, status: &str) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id,
            public_id: public_id.to_string(),
            user_id: Some(1),
            agent_run_id: None,
            name: format!("sbx-{}", id),
            status: status.to_string(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: Some("docker".into()),
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::seconds(3600),
            preview_password_hash: None,
            preview_password_hint: None,
            lifecycle: "ephemeral".to_string(),
            project_id: None,
            source_repo_url: None,
        }
    }

    #[test]
    fn default_quota_is_10_gib() {
        assert_eq!(DEFAULT_SNAPSHOT_QUOTA_BYTES, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn storage_summary_struct_has_expected_fields() {
        let s = StorageSummary {
            total_bytes: 1024,
            snapshot_count: 1,
            quota_bytes: DEFAULT_SNAPSHOT_QUOTA_BYTES,
            available_disk_bytes: None,
        };
        assert_eq!(s.total_bytes, 1024);
        assert_eq!(s.snapshot_count, 1);
        assert_eq!(s.quota_bytes, DEFAULT_SNAPSHOT_QUOTA_BYTES);
    }

    #[test]
    fn snapshot_artifact_metadata_round_trip() {
        let expected = SnapshotCompanionArtifact {
            content_path: std::path::PathBuf::from("/data/snapshots/digest.workspace.tar"),
            content_digest: "workspace-digest".to_string(),
            size_bytes: 2048,
        };
        let metadata = serde_json::json!({
            "primary_digest": "primary-digest",
            "image_id": "sha256:image-id",
            "workspace": expected,
        });

        let persisted =
            artifact_metadata("snap_test", Some(&metadata)).expect("valid artifact metadata");
        assert_eq!(persisted.primary_digest.as_deref(), Some("primary-digest"));
        assert_eq!(persisted.image_id.as_deref(), Some("sha256:image-id"));
        let decoded = persisted.workspace.expect("workspace companion");

        assert_eq!(decoded.content_digest, "workspace-digest");
        assert_eq!(decoded.size_bytes, 2048);
        assert_eq!(
            decoded.content_path,
            std::path::PathBuf::from("/data/snapshots/digest.workspace.tar")
        );
    }

    #[test]
    fn malformed_workspace_metadata_returns_contextual_error() {
        let metadata = serde_json::json!({
            "workspace": {
                "content_path": "/data/snapshots/workspace.tar",
                "size_bytes": "not-a-number"
            }
        });

        let result = artifact_metadata("snap_broken", Some(&metadata));

        assert!(matches!(
            result,
            Err(SandboxSnapshotError::InvalidArtifactMetadata { snapshot_id, .. })
                if snapshot_id == "snap_broken"
        ));
    }

    fn make_snapshot_model(
        id: i32,
        public_id: &str,
        user_id: i32,
        status: &str,
        size_bytes: i64,
    ) -> sandbox_snapshots::Model {
        let now = Utc::now();
        sandbox_snapshots::Model {
            id,
            public_id: public_id.to_string(),
            user_id,
            project_id: None,
            source_sandbox_id: None,
            label: None,
            status: status.to_string(),
            backend: "docker".to_string(),
            content_digest: format!("sha256_{}", id),
            content_path: format!("/data/snapshots/sha256_{}.tar", id),
            size_bytes,
            image_ref: Some(format!("temps-snapshot/snap_{}", id)),
            metadata: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_fake_artifact(directory: &Path, size_bytes: u64) -> SnapshotArtifact {
        let primary = directory.join("snapshot.tar");
        let workspace = directory.join("snapshot.workspace.tar");
        std::fs::write(&primary, b"primary").expect("write fake primary artifact");
        std::fs::write(&workspace, b"workspace").expect("write fake workspace artifact");
        SnapshotArtifact {
            content_path: primary,
            content_digest: "fake-logical-digest".to_string(),
            primary_digest: "fake-primary-digest".to_string(),
            size_bytes,
            backend: SandboxBackend::Docker,
            image_ref: Some("temps-snapshot/fake-logical-digest:latest".to_string()),
            image_id: Some("sha256:fake".to_string()),
            workspace: Some(SnapshotCompanionArtifact {
                content_path: workspace,
                content_digest: "fake-workspace-digest".to_string(),
                size_bytes: 9,
            }),
        }
    }

    fn make_service(db: Arc<DatabaseConnection>) -> SnapshotService {
        use crate::services::registry::StandaloneSandboxRegistry;
        use temps_agents::sandbox::local::LocalSandboxProvider;
        let provider = Arc::new(LocalSandboxProvider::new());
        let registry = Arc::new(StandaloneSandboxRegistry::new(provider));
        SnapshotService::new(
            db,
            registry,
            Arc::new(temps_agents::sandbox::local::LocalSandboxProvider::new()),
        )
    }

    // ── get_snapshot ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_snapshot_returns_not_found_when_missing() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.get_snapshot(1, "snap_nonexistent").await;
        assert!(
            matches!(result, Err(SandboxSnapshotError::NotFound { .. })),
            "expected NotFound, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn get_snapshot_returns_not_found_for_wrong_owner() {
        // DB returns a row owned by user 99, but caller is user 1.
        let model = make_snapshot_model(1, "snap_aabbccdd11223344", 99, "ready", 1_000_000);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.get_snapshot(1, "snap_aabbccdd11223344").await;
        assert!(
            matches!(result, Err(SandboxSnapshotError::NotFound { .. })),
            "wrong-owner access must look like NotFound, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn get_snapshot_success() {
        let model = make_snapshot_model(1, "snap_aabbccdd11223344", 42, "ready", 5_000_000);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model.clone()]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.get_snapshot(42, "snap_aabbccdd11223344").await;
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let row = result.unwrap();
        assert_eq!(row.public_id, "snap_aabbccdd11223344");
        assert_eq!(row.user_id, 42);
    }

    // ── storage_summary ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn storage_summary_sums_ready_snapshots() {
        let rows = vec![
            make_snapshot_model(1, "snap_one1111111122222222", 7, "ready", 1_000_000_000),
            make_snapshot_model(2, "snap_two2222222233333333", 7, "ready", 500_000_000),
        ];

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![rows])
                .into_connection(),
        );
        let svc = make_service(db);

        let summary = svc.storage_summary(7).await.unwrap();
        assert_eq!(summary.total_bytes, 1_500_000_000);
        assert_eq!(summary.snapshot_count, 2);
        assert_eq!(summary.quota_bytes, DEFAULT_SNAPSHOT_QUOTA_BYTES);
    }

    #[tokio::test]
    async fn storage_summary_returns_zero_for_no_snapshots() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .into_connection(),
        );
        let svc = make_service(db);

        let summary = svc.storage_summary(99).await.unwrap();
        assert_eq!(summary.total_bytes, 0);
        assert_eq!(summary.snapshot_count, 0);
    }

    // ── quota enforcement ────────────────────────────────────────────────────

    #[test]
    fn storage_summary_quota_exceeded_detected_at_boundary() {
        // Simulate a service with a tiny quota and verify the comparison works.
        let s = StorageSummary {
            total_bytes: 100,
            snapshot_count: 1,
            quota_bytes: 100,
            available_disk_bytes: None,
        };
        // At exactly the quota the check should trigger (used >= quota).
        assert!(
            s.total_bytes >= s.quota_bytes,
            "at-quota case must be treated as exceeded"
        );
    }

    // ── resolve_for_restore ──────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_for_restore_rejects_non_ready_snapshot() {
        let model = make_snapshot_model(1, "snap_creating00001111", 5, "creating", 0);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc
            .resolve_for_restore(5, "snap_creating00001111", Some("docker"))
            .await;
        assert!(
            matches!(result, Err(SandboxSnapshotError::NotReady { .. })),
            "expected NotReady, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn resolve_for_restore_rejects_cross_backend() {
        let model = make_snapshot_model(1, "snap_docker000011112222", 5, "ready", 1_000_000);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        // The snapshot is docker but we are asking for firecracker.
        let result = svc
            .resolve_for_restore(5, "snap_docker000011112222", Some("firecracker"))
            .await;
        assert!(
            matches!(
                result,
                Err(SandboxSnapshotError::CrossBackendRestore { .. })
            ),
            "expected CrossBackendRestore, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn resolve_for_restore_infers_firecracker_backend_when_unspecified() {
        let digest = format!(
            "firecracker-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let artifact_dir = std::env::temp_dir().join(&digest);
        let artifact_path = artifact_dir.join(format!("{}.ext4", digest));
        tokio::fs::create_dir_all(&artifact_dir)
            .await
            .expect("create artifact directory");
        tokio::fs::write(&artifact_path, b"firecracker-rootfs")
            .await
            .expect("write artifact");

        let mut model = make_snapshot_model(1, "snap_firecracker11112222", 5, "ready", 20);
        model.backend = "firecracker".to_string();
        model.content_digest = digest;
        model.content_path = artifact_path.to_string_lossy().to_string();
        model.image_ref = None;

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let artifact = svc
            .resolve_for_restore(5, "snap_firecracker11112222", None)
            .await
            .expect("resolve Firecracker snapshot without an explicit backend");

        assert_eq!(artifact.backend, SandboxBackend::Firecracker);
        assert!(artifact.image_ref.is_none());
        assert!(artifact.workspace.is_none());

        let _ = tokio::fs::remove_dir_all(artifact_dir).await;
    }

    #[tokio::test]
    async fn resolve_for_restore_rejects_legacy_docker_snapshot_without_integrity_metadata() {
        let artifact_dir = tempfile::tempdir().expect("create legacy artifact directory");
        let digest = "legacy-docker-digest";
        let artifact_path = artifact_dir.path().join(format!("{}.tar", digest));
        std::fs::write(&artifact_path, b"legacy-image-tar").expect("write legacy Docker artifact");

        let mut model = make_snapshot_model(1, "snap_legacydocker11112222", 5, "ready", 16);
        model.content_digest = digest.to_string();
        model.content_path = artifact_path.to_string_lossy().to_string();
        model.metadata = None;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc
            .resolve_for_restore(5, "snap_legacydocker11112222", Some("docker"))
            .await;

        assert!(
            matches!(
                &result,
                Err(SandboxSnapshotError::LegacyArtifactUnsupported { .. })
            ),
            "expected legacy artifact error, got {:?}",
            result
        );
    }

    /// Minor 1: DigestMismatch is returned when the stored content_path stem
    /// does not match the stored content_digest. This detects DB/artifact
    /// drift (e.g. manual file moves or a dedup bug) before any file I/O.
    ///
    /// The check runs BEFORE the exists check so we don't need a real file.
    #[tokio::test]
    async fn resolve_for_restore_rejects_mismatched_digest() {
        // Build a model where content_digest and the path stem disagree.
        let mut model = make_snapshot_model(1, "snap_digest_mismatch1111", 5, "ready", 1_000);
        // Overwrite with a path whose stem doesn't match the content_digest.
        model.content_digest = "sha256correctdigest".to_string();
        model.content_path = "/data/snapshots/sha256WRONGDIGEST.tar".to_string();

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc
            .resolve_for_restore(5, "snap_digest_mismatch1111", Some("docker"))
            .await;

        assert!(
            matches!(result, Err(SandboxSnapshotError::DigestMismatch { .. })),
            "expected DigestMismatch when path stem and content_digest disagree, got {:?}",
            result
        );
        // Confirm the expected/actual fields are populated.
        if let Err(SandboxSnapshotError::DigestMismatch { expected, actual }) = result {
            assert_eq!(expected, "sha256correctdigest");
            assert_eq!(actual, "sha256WRONGDIGEST");
        }
    }

    // ── nullify_source_sandbox ────────────────────────────────────────────────

    #[tokio::test]
    async fn nullify_source_sandbox_succeeds_on_empty_result() {
        // MockDatabase: update_many returns a MockExecResult.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                }])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.nullify_source_sandbox(42).await;
        assert!(
            result.is_ok(),
            "nullify_source_sandbox should succeed: {:?}",
            result
        );
    }

    // ── create_snapshot ───────────────────────────────────────────────────────

    /// Build a MockDatabase COUNT row for the `creating` rows check.
    ///
    /// The TOCTOU fix added a `COUNT(*)` query as the very first query inside
    /// the quota lock. All create_snapshot tests must prepend this mock row.
    /// `n` is the count to return (0 = no in-flight snapshot, 1+ = reject).
    fn make_creating_count_row(n: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut row = std::collections::BTreeMap::new();
        row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(n)));
        row
    }

    /// Major 6: over-quota rejection. No provider interaction needed — the
    /// service short-circuits at the storage_summary check before touching
    /// the registry.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0 (no in-flight snapshot)
    ///   2. storage_summary SELECT → row at quota
    #[tokio::test]
    async fn create_snapshot_rejects_over_quota() {
        // storage_summary: one ready row at exactly the default quota
        let at_quota = make_snapshot_model(
            1,
            "snap_existing1111111122222222",
            7,
            "ready",
            DEFAULT_SNAPSHOT_QUOTA_BYTES as i64,
        );

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![vec![at_quota]])
                .into_connection(),
        );

        let svc = make_service_with_provider(db, FakeSnapshotProvider::new())
            .with_quota(DEFAULT_SNAPSHOT_QUOTA_BYTES);

        let result = svc
            .create_snapshot(42, "sbx_aabbccddeeff0011", 7, None, None)
            .await;

        assert!(
            matches!(
                result,
                Err(SandboxSnapshotError::QuotaExceeded { user_id: 7, .. })
            ),
            "at-quota create must be rejected with QuotaExceeded, got {:?}",
            result
        );
    }

    /// Major 6: over-quota — even slightly under the byte boundary passes,
    /// confirming the comparison is `>=` not `>`.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0
    ///   2. storage_summary SELECT → row just under quota
    #[tokio::test]
    async fn create_snapshot_rejects_at_quota_boundary() {
        let nearly_full = make_snapshot_model(
            2,
            "snap_nearlyfull111122223333",
            8,
            "ready",
            (DEFAULT_SNAPSHOT_QUOTA_BYTES - 1) as i64,
        );

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![vec![nearly_full]])
                .into_connection(),
        );

        let svc = make_service_with_provider(db, FakeSnapshotProvider::new())
            .with_quota(DEFAULT_SNAPSHOT_QUOTA_BYTES);

        // quota_bytes - 1 < quota_bytes → should NOT be rejected
        let result = svc
            .create_snapshot(99, "sbx_notarealid0000001", 8, None, None)
            .await;

        // The test proves quota check passed; the DB has no further mocks so
        // it will panic at the registry lookup (no sandboxes::Model). We only
        // care that it did NOT return QuotaExceeded.
        assert!(
            !matches!(result, Err(SandboxSnapshotError::QuotaExceeded { .. })),
            "one byte under quota must not be rejected as exceeded"
        );
    }

    #[tokio::test]
    async fn create_snapshot_registry_failure_marks_row_failed() {
        let creating = make_snapshot_model(1, "snap_missing11112222", 8, "creating", 0);
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: true,
            fail_delete_image: false,
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider);

        let result = svc
            .create_snapshot(99, "sbx_missing11112222", 8, None, None)
            .await;

        assert!(matches!(
            result,
            Err(SandboxSnapshotError::SandboxNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn create_snapshot_missing_sandbox_row_marks_snapshot_failed() {
        let creating = make_snapshot_model(1, "snap_dbmissing11112222", 8, "creating", 0);
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![Vec::<sandboxes::Model>::new()])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );
        let svc = make_service_with_provider(db, FakeSnapshotProvider::new());

        let result = svc
            .create_snapshot(100, "sbx_dbmissing11112222", 8, None, None)
            .await;

        assert!(matches!(
            result,
            Err(SandboxSnapshotError::SandboxNotFound { .. })
        ));
    }

    /// Major 6: when exec_as_root (shred) fails, create_snapshot returns
    /// ScrubFailed and marks the row failed. This also validates C1 — the
    /// hard-abort on shred failure.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0
    ///   2. storage_summary SELECT (empty → under quota)
    ///   3. INSERT new row RETURNING
    ///   4. sandboxes::find_by_id (for was_running)
    ///   5. mark_failed: find_by_id(row.id) RETURNING
    ///   6. mark_failed: UPDATE RETURNING
    #[tokio::test]
    async fn create_snapshot_shred_failure_returns_scrub_failed() {
        let creating = make_snapshot_model(1, "snap_shred000011112222", 3, "creating", 0);
        let sandbox = make_sandbox_row(10, "sbx_shred000011112222", "running");
        let failed = {
            let mut m = creating.clone();
            m.status = "failed".to_string();
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary — empty
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 3. INSERT row RETURNING
                .append_query_results(vec![vec![creating.clone()]])
                // 4. sandboxes::find_by_id
                .append_query_results(vec![vec![sandbox]])
                // 5. mark_failed find_by_id
                .append_query_results(vec![vec![creating]])
                // 6. mark_failed UPDATE RETURNING
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );

        let provider = FakeSnapshotProvider {
            fail_exec_as_root: true,
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider);

        let result = svc
            .create_snapshot(10, "sbx_shred000011112222", 3, None, None)
            .await;

        assert!(
            matches!(result, Err(SandboxSnapshotError::ScrubFailed { .. })),
            "exec_as_root failure must return ScrubFailed (C1), got {:?}",
            result
        );
    }

    /// Major 6: when `take_snapshot` fails, the service marks the row failed
    /// and returns a Provider error (never proceeds to commit).
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0
    ///   2. storage_summary SELECT (empty)
    ///   3. INSERT row RETURNING
    ///   4. sandboxes::find_by_id (running)
    ///   5. mark_failed: find_by_id RETURNING
    ///   6. mark_failed: UPDATE RETURNING
    #[tokio::test]
    async fn create_snapshot_provider_failure_marks_row_failed() {
        let creating = make_snapshot_model(1, "snap_provfail11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(20, "sbx_provfail11112222", "running");
        let failed = {
            let mut m = creating.clone();
            m.status = "failed".to_string();
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 3. INSERT
                .append_query_results(vec![vec![creating.clone()]])
                // 4. sandboxes lookup
                .append_query_results(vec![vec![sandbox]])
                // 5. mark_failed find_by_id
                .append_query_results(vec![vec![creating]])
                // 6. mark_failed update
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );

        let provider = FakeSnapshotProvider {
            fail_take_snapshot: true,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider);

        let result = svc
            .create_snapshot(20, "sbx_provfail11112222", 5, None, None)
            .await;

        assert!(
            matches!(result, Err(SandboxSnapshotError::Provider(_))),
            "take_snapshot failure must return Provider error, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn create_snapshot_stop_failure_marks_row_failed() {
        let creating = make_snapshot_model(1, "snap_stopfail11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(22, "sbx_stopfail11112222", "running");
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![sandbox]])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: true,
            fail_recover: false,
            fail_delete_image: false,
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider);

        let result = svc
            .create_snapshot(22, "sbx_stopfail11112222", 5, None, None)
            .await;

        assert!(matches!(result, Err(SandboxSnapshotError::Provider(_))));
    }

    #[tokio::test]
    async fn create_snapshot_size_limit_returns_quota_exceeded_and_marks_row_failed() {
        let creating = make_snapshot_model(1, "snap_sizefail11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(21, "sbx_sizefail11112222", "running");
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![sandbox]])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: true,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider).with_quota(8192);

        let result = svc
            .create_snapshot(21, "sbx_sizefail11112222", 5, None, None)
            .await;

        assert!(matches!(
            result,
            Err(SandboxSnapshotError::QuotaExceeded {
                user_id: 5,
                quota_bytes: 8192,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn oversized_provider_success_marks_failed_and_removes_unreferenced_artifacts() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 101);
        let primary_path = artifact.content_path.clone();
        let workspace_path = artifact.workspace.as_ref().unwrap().content_path.clone();
        let creating = make_snapshot_model(1, "snap_oversized11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(23, "sbx_oversized11112222", "running");
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![sandbox]])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: Some(artifact),
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider).with_quota(100);

        let result = svc
            .create_snapshot(23, "sbx_oversized11112222", 5, None, None)
            .await;

        assert!(matches!(
            result,
            Err(SandboxSnapshotError::QuotaExceeded { .. })
        ));
        assert!(!primary_path.exists());
        assert!(!workspace_path.exists());
    }

    #[tokio::test]
    async fn dedup_query_failure_marks_failed_and_removes_unreferenced_artifacts() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 10);
        let primary_path = artifact.content_path.clone();
        let workspace_path = artifact.workspace.as_ref().unwrap().content_path.clone();
        let creating = make_snapshot_model(1, "snap_deduperror11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(24, "sbx_deduperror11112222", "running");
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![sandbox]])
                .append_query_errors([sea_orm::DbErr::Custom(
                    "dedup query unavailable".to_string(),
                )])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: Some(artifact),
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider).with_quota(100);

        let result = svc
            .create_snapshot(24, "sbx_deduperror11112222", 5, None, None)
            .await;

        assert!(matches!(result, Err(SandboxSnapshotError::Database(_))));
        assert!(!primary_path.exists());
        assert!(!workspace_path.exists());
    }

    #[tokio::test]
    async fn finalization_failure_marks_failed_and_removes_unreferenced_artifacts() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 10);
        let primary_path = artifact.content_path.clone();
        let workspace_path = artifact.workspace.as_ref().unwrap().content_path.clone();
        let creating = make_snapshot_model(1, "snap_finalizeerr11112222", 5, "creating", 0);
        let sandbox = make_sandbox_row(25, "sbx_finalizeerr11112222", "running");
        let failed = {
            let mut model = creating.clone();
            model.status = "failed".to_string();
            model
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_results(vec![vec![sandbox]])
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                .append_query_results(vec![vec![creating.clone()]])
                .append_query_errors([sea_orm::DbErr::Custom(
                    "snapshot finalization unavailable".to_string(),
                )])
                .append_query_results(vec![vec![creating]])
                .append_query_results(vec![vec![failed]])
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .into_connection(),
        );
        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: false,
            artifact: Some(artifact),
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider).with_quota(100);

        let result = svc
            .create_snapshot(25, "sbx_finalizeerr11112222", 5, None, None)
            .await;

        assert!(matches!(result, Err(SandboxSnapshotError::Database(_))));
        assert!(!primary_path.exists());
        assert!(!workspace_path.exists());
    }

    /// Major 6: happy path — create_snapshot succeeds end-to-end.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0
    ///   2. storage_summary SELECT (empty)
    ///   3. INSERT row RETURNING (creating)
    ///   4. sandboxes::find_by_id (running)
    ///   5. dedup check (none)
    ///   6. finalize_row find_by_id RETURNING
    ///   7. finalize_row UPDATE RETURNING (ready)
    #[tokio::test]
    async fn create_snapshot_happy_path() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 4096);
        let creating = make_snapshot_model(1, "snap_happypath111222", 6, "creating", 0);
        let sandbox = make_sandbox_row(30, "sbx_happypath1112222", "running");
        let ready = {
            let mut m = creating.clone();
            m.status = "ready".to_string();
            m.size_bytes = 4096;
            m.backend = "docker".to_string();
            m.content_digest = "sha256fakedigest1234567890abcdef01234567".to_string();
            m.content_path = "/tmp/test-snap.tar".to_string();
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 3. INSERT
                .append_query_results(vec![vec![creating.clone()]])
                // 4. sandboxes lookup
                .append_query_results(vec![vec![sandbox]])
                // 5. dedup check → no match
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 6. finalize find_by_id
                .append_query_results(vec![vec![creating]])
                // 7. finalize UPDATE
                .append_query_results(vec![vec![ready.clone()]])
                .into_connection(),
        );

        let provider = FakeSnapshotProvider {
            artifact: Some(artifact),
            ..FakeSnapshotProvider::new()
        };
        let svc = make_service_with_provider(db.clone(), provider);

        let result = svc
            .create_snapshot(30, "sbx_happypath1112222", 6, None, None)
            .await;

        assert!(result.is_ok(), "happy path must succeed, got {:?}", result);
        let row = result.unwrap();
        assert_eq!(row.status, "ready");
        assert_eq!(row.size_bytes, 4096);
        drop(svc);

        let db = Arc::try_unwrap(db).expect("snapshot service released mock database");
        let transaction_log = format!("{:?}", db.into_transaction_log());
        assert!(transaction_log.contains("primary_digest"));
        assert!(transaction_log.contains("sha256:fake"));
        assert!(transaction_log.contains("workspace"));
        assert!(transaction_log.contains("fake-workspace-digest"));
    }

    /// Major 6: dedup — when the artifact digest matches an existing ready
    /// row, the duplicate file is removed and the creating row is updated to
    /// point at the existing artifact instead of writing a new one.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0
    ///   2. storage_summary SELECT (empty)
    ///   3. INSERT row RETURNING (creating)
    ///   4. sandboxes::find_by_id (running)
    ///   5. dedup check → existing ready row with same digest
    ///   6. finalize_row find_by_id RETURNING (creating)
    ///   7. finalize_row UPDATE RETURNING (ready, pointing at existing path)
    #[tokio::test]
    async fn create_snapshot_dedup_reuses_existing_artifact() {
        let creating = make_snapshot_model(2, "snap_dedup000011112222", 9, "creating", 0);
        let sandbox = make_sandbox_row(40, "sbx_dedup000011112222", "running");

        // The existing ready row shares the digest that FakeSnapshotProvider returns
        let existing_ready = {
            let mut m = make_snapshot_model(1, "snap_existing11112222", 9, "ready", 4096);
            m.content_digest = "sha256fakedigest1234567890abcdef01234567".to_string();
            m.content_path = "/data/snapshots/existing.tar".to_string();
            m
        };

        let deduped_ready = {
            let mut m = creating.clone();
            m.status = "ready".to_string();
            m.content_digest = "sha256fakedigest1234567890abcdef01234567".to_string();
            m.content_path = "/data/snapshots/existing.tar".to_string(); // reuses existing path
            m.size_bytes = 4096;
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 3. INSERT creating row
                .append_query_results(vec![vec![creating.clone()]])
                // 4. sandboxes lookup
                .append_query_results(vec![vec![sandbox]])
                // 5. dedup check → existing ready row
                .append_query_results(vec![vec![existing_ready]])
                // 6. finalize find_by_id
                .append_query_results(vec![vec![creating]])
                // 7. finalize UPDATE RETURNING
                .append_query_results(vec![vec![deduped_ready.clone()]])
                .into_connection(),
        );

        let svc = make_service_with_provider(db, FakeSnapshotProvider::new());

        let result = svc
            .create_snapshot(40, "sbx_dedup000011112222", 9, None, None)
            .await;

        assert!(result.is_ok(), "dedup path must succeed, got {:?}", result);
        let row = result.unwrap();
        // Should point at the existing artifact path
        assert_eq!(row.content_path, "/data/snapshots/existing.tar");
    }

    // ── list_snapshots ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_snapshots_returns_paginated_results() {
        use sea_orm::Value;
        use std::collections::BTreeMap;

        let rows = vec![
            make_snapshot_model(1, "snap_aaa0000011112222", 10, "ready", 100_000),
            make_snapshot_model(2, "snap_bbb0000011112222", 10, "ready", 200_000),
        ];

        // Sea-ORM's paginator `num_items()` executes `SELECT COUNT(*) AS
        // num_items ...` and reads the result as `Value::BigInt`.
        let mut count_row: BTreeMap<String, Value> = BTreeMap::new();
        count_row.insert("num_items".to_string(), Value::BigInt(Some(2)));

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // First query: num_items() → count row.
                .append_query_results(vec![vec![count_row]])
                // Second query: fetch_page() → actual rows.
                .append_query_results(vec![rows])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.list_snapshots(10, None, None, 1, 20).await;
        assert!(
            result.is_ok(),
            "list_snapshots should succeed: {:?}",
            result
        );
        let (items, total) = result.unwrap();
        assert_eq!(total, 2);
        assert_eq!(items.len(), 2);
    }

    // ── HIGH: TOCTOU quota race ───────────────────────────────────────────────

    /// While one snapshot is in `creating` status a second create request for
    /// the same user must be rejected with SnapshotInProgress (409). This
    /// closes the TOCTOU window: creating rows have size_bytes = 0, so without
    /// this guard a sequential flood would each pass the quota check before any
    /// of them finalize.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows for user → 1 (in-flight row exists)
    ///   (no further DB calls — returns SnapshotInProgress immediately)
    #[tokio::test]
    async fn create_snapshot_rejected_while_creating_row_in_flight() {
        // MockDatabase: the COUNT query for `creating` rows returns 1.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_creating_count_row(1)]])
                .into_connection(),
        );

        let svc = make_service_with_provider(db, FakeSnapshotProvider::new());

        let result = svc
            .create_snapshot(1, "sbx_inflight0000001122", 42, None, None)
            .await;

        assert!(
            matches!(
                result,
                Err(SandboxSnapshotError::SnapshotInProgress { user_id: 42 })
            ),
            "second create while one is creating must return SnapshotInProgress(42), got {:?}",
            result
        );
    }

    // ── MEDIUM: stopped-sandbox guard ────────────────────────────────────────

    /// When the sandbox is stopped (`was_running = false`), create_snapshot
    /// must return SandboxNotRunning immediately, before attempting the shred
    /// exec. Without this guard the exec would fail with "container is not
    /// running" and surface as ScrubFailed — a misleading error.
    ///
    /// DB sequence:
    ///   1. COUNT creating rows → 0 (no in-flight snapshot)
    ///   2. storage_summary SELECT (empty → under quota)
    ///   3. INSERT creating row RETURNING
    ///   4. sandboxes::find_by_id → status = "stopped"
    ///   5. mark_failed find_by_id RETURNING (creating)
    ///   6. mark_failed UPDATE RETURNING (failed)
    #[tokio::test]
    async fn create_snapshot_returns_not_running_for_stopped_sandbox() {
        let creating = make_snapshot_model(1, "snap_stopped00011112222", 11, "creating", 0);
        let sandbox_stopped = make_sandbox_row(50, "sbx_stopped00011112222", "stopped");
        let failed = {
            let mut m = creating.clone();
            m.status = "failed".to_string();
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. COUNT creating rows → 0
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                // 2. storage_summary
                .append_query_results(vec![Vec::<sandbox_snapshots::Model>::new()])
                // 3. INSERT
                .append_query_results(vec![vec![creating.clone()]])
                // 4. sandboxes lookup → stopped
                .append_query_results(vec![vec![sandbox_stopped]])
                // 5. mark_failed find_by_id
                .append_query_results(vec![vec![creating]])
                // 6. mark_failed UPDATE
                .append_query_results(vec![vec![failed]])
                .into_connection(),
        );

        let svc = make_service_with_provider(db, FakeSnapshotProvider::new());

        let result = svc
            .create_snapshot(50, "sbx_stopped00011112222", 11, None, None)
            .await;

        assert!(
            matches!(result, Err(SandboxSnapshotError::SandboxNotRunning { .. })),
            "stopped sandbox must return SandboxNotRunning, not ScrubFailed; got {:?}",
            result
        );
    }

    // ── MINOR: IDOR regression tests ─────────────────────────────────────────

    /// delete_snapshot called with a snapshot owned by a different user must
    /// return NotFound (same behaviour as get_snapshot), not actually delete
    /// the row. This mirrors the existing get_snapshot_returns_not_found_for_wrong_owner
    /// test but exercises the delete_snapshot code path end-to-end.
    #[tokio::test]
    async fn delete_snapshot_returns_not_found_for_wrong_owner() {
        // Row is owned by user 99, caller is user 1.
        let model = make_snapshot_model(1, "snap_wrongowner111222", 99, "ready", 1_000_000);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc.delete_snapshot(1, "snap_wrongowner111222").await;
        assert!(
            matches!(result, Err(SandboxSnapshotError::NotFound { .. })),
            "wrong-owner delete must look like NotFound, got {:?}",
            result
        );
    }

    /// resolve_for_restore called with a snapshot owned by a different user
    /// must return NotFound (IDOR guard). Mirrors delete_snapshot_returns_not_found_for_wrong_owner.
    #[tokio::test]
    async fn resolve_for_restore_returns_not_found_for_wrong_owner() {
        // Row is owned by user 77, caller is user 1.
        let model = make_snapshot_model(2, "snap_wrongowner777888", 77, "ready", 2_000_000);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .into_connection(),
        );
        let svc = make_service(db);

        let result = svc
            .resolve_for_restore(1, "snap_wrongowner777888", Some("docker"))
            .await;
        assert!(
            matches!(result, Err(SandboxSnapshotError::NotFound { .. })),
            "wrong-owner resolve_for_restore must look like NotFound, got {:?}",
            result
        );
    }

    // ── NIT: delete_image best-effort-on-failure ──────────────────────────────

    /// When the provider's delete_image call fails, delete_snapshot must still
    /// return Ok (soft-deleted the row). The image removal is explicitly
    /// best-effort and must never fail the delete operation.
    ///
    /// DB sequence:
    ///   1. get_snapshot SELECT → ready row with image_ref
    ///   2. COUNT sharing_count → 0 (no other row shares digest)
    ///   3. soft-delete UPDATE RETURNING
    ///   (delete_image call errors — result is Ok regardless)
    #[tokio::test]
    async fn delete_snapshot_succeeds_even_when_delete_image_fails() {
        let mut model = make_snapshot_model(1, "snap_imgfail00011112222", 5, "ready", 1_000_000);
        model.image_ref = Some("temps-snapshot/my-snap:latest".to_string());

        // COUNT query for sharing check
        use sea_orm::Value;
        use std::collections::BTreeMap;
        let mut count_row: BTreeMap<String, Value> = BTreeMap::new();
        count_row.insert("num_items".to_string(), Value::BigInt(Some(0)));

        let deleted = {
            let mut m = model.clone();
            m.status = "deleted".to_string();
            m
        };

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. get_snapshot SELECT
                .append_query_results(vec![vec![model]])
                // 2. sharing_count COUNT
                .append_query_results(vec![vec![count_row]])
                // 3. soft-delete UPDATE RETURNING
                .append_query_results(vec![vec![deleted]])
                .into_connection(),
        );

        let provider = FakeSnapshotProvider {
            fail_take_snapshot: false,
            fail_size_limit: false,
            fail_exec_as_root: false,
            fail_stop: false,
            fail_recover: false,
            fail_delete_image: true, // provider will fail image cleanup
            artifact: None,
            delete_image_calls: Arc::new(AtomicUsize::new(0)),
        };
        let svc = make_service_with_provider(db, provider);

        let result = svc.delete_snapshot(5, "snap_imgfail00011112222").await;
        assert!(
            result.is_ok(),
            "delete_snapshot must return Ok even when delete_image fails (best-effort), got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn delete_last_reference_removes_primary_and_workspace_artifacts() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 10);
        let primary_path = artifact.content_path.clone();
        let workspace_path = artifact.workspace.as_ref().unwrap().content_path.clone();
        let mut model = make_snapshot_model(1, "snap_deletefiles11112222", 5, "ready", 10);
        model.content_digest = artifact.content_digest.clone();
        model.content_path = primary_path.to_string_lossy().to_string();
        model.image_ref = None;
        model.metadata = Some(serde_json::json!(PersistedSnapshotArtifactMetadata {
            primary_digest: Some(artifact.primary_digest),
            image_id: artifact.image_id,
            workspace: artifact.workspace,
        }));
        let deleted = {
            let mut row = model.clone();
            row.status = "deleted".to_string();
            row
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .append_query_results(vec![vec![make_creating_count_row(0)]])
                .append_query_results(vec![vec![deleted]])
                .into_connection(),
        );
        let svc = make_service(db);

        svc.delete_snapshot(5, "snap_deletefiles11112222")
            .await
            .expect("delete final artifact reference");

        assert!(!primary_path.exists());
        assert!(!workspace_path.exists());
    }

    #[tokio::test]
    async fn delete_shared_reference_retains_primary_and_workspace_artifacts() {
        let artifact_dir = tempfile::tempdir().expect("create fake artifact directory");
        let artifact = make_fake_artifact(artifact_dir.path(), 10);
        let primary_path = artifact.content_path.clone();
        let workspace_path = artifact.workspace.as_ref().unwrap().content_path.clone();
        let image_ref = artifact.image_ref.clone();
        let mut model = make_snapshot_model(1, "snap_retainfiles11112222", 5, "ready", 10);
        model.content_digest = artifact.content_digest.clone();
        model.content_path = primary_path.to_string_lossy().to_string();
        model.image_ref = image_ref;
        model.metadata = Some(serde_json::json!(PersistedSnapshotArtifactMetadata {
            primary_digest: Some(artifact.primary_digest),
            image_id: artifact.image_id,
            workspace: artifact.workspace,
        }));
        let deleted = {
            let mut row = model.clone();
            row.status = "deleted".to_string();
            row
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![model]])
                .append_query_results(vec![vec![make_creating_count_row(1)]])
                .append_query_results(vec![vec![deleted]])
                .into_connection(),
        );
        let delete_image_calls = Arc::new(AtomicUsize::new(0));
        let provider = FakeSnapshotProvider {
            delete_image_calls: delete_image_calls.clone(),
            ..FakeSnapshotProvider::new()
        };
        let svc = make_service_with_provider(db, provider);

        svc.delete_snapshot(5, "snap_retainfiles11112222")
            .await
            .expect("delete shared artifact reference");

        assert!(primary_path.exists());
        assert!(workspace_path.exists());
        assert_eq!(
            delete_image_calls.load(Ordering::SeqCst),
            0,
            "shared Docker tag must remain until the final ready reference is deleted"
        );
    }

    #[tokio::test]
    async fn artifact_lifecycle_lock_serializes_operations() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = make_service(db);

        let first_guard = svc.acquire_artifact_lifecycle().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            svc.acquire_artifact_lifecycle(),
        )
        .await;
        assert!(
            blocked.is_err(),
            "a second artifact lifecycle operation must wait for the first"
        );

        drop(first_guard);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            svc.acquire_artifact_lifecycle(),
        )
        .await
        .expect("artifact lifecycle lock should become available after release");
    }
}
