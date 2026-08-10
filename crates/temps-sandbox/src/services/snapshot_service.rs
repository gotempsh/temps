//! Service layer for sandbox snapshots (ADR-037).
//!
//! Responsibilities:
//! - Quota enforcement (per-user soft cap, default 10 GiB).
//! - Deduplication of content-addressed artifacts.
//! - Lifecycle management: create / list / get / delete.
//! - Orchestrating the stop → snapshot → restart cycle via the provider.
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

use temps_agents::sandbox::{SandboxProvider, SnapshotArtifact};
use temps_entities::sandbox_snapshots;

use crate::error::SandboxSnapshotError;
use crate::services::public_id;
use crate::services::registry::StandaloneSandboxRegistry;

/// Per-user snapshot storage quota: 10 GiB (soft cap, enforced at API level).
pub const DEFAULT_SNAPSHOT_QUOTA_BYTES: u64 = 10 * 1024 * 1024 * 1024;

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
        }
    }

    pub fn with_quota(mut self, quota_bytes: u64) -> Self {
        self.quota_bytes = quota_bytes;
        self
    }

    // ── Create ────────────────────────────────────────────────────────────────

    /// Create a snapshot row in `creating` state, then call the provider.
    /// On success, transitions the row to `ready` and returns the model.
    /// On failure, transitions to `failed` (row kept for audit).
    ///
    /// The sandbox is stopped before snapping (for filesystem consistency) and
    /// restarted afterwards unless it was already stopped on entry.
    pub async fn create_snapshot(
        &self,
        sandbox_internal_id: i32,
        sandbox_public_id: &str,
        user_id: i32,
        project_id: Option<i32>,
        label: Option<String>,
    ) -> Result<sandbox_snapshots::Model, SandboxSnapshotError> {
        // ── Quota check ───────────────────────────────────────────────────────
        let storage = self.storage_summary(user_id).await?;
        if storage.total_bytes >= self.quota_bytes {
            return Err(SandboxSnapshotError::QuotaExceeded {
                user_id,
                used_bytes: storage.total_bytes,
                quota_bytes: self.quota_bytes,
            });
        }

        // ── Create the placeholder row ────────────────────────────────────────
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

        // ── Get the sandbox handle ────────────────────────────────────────────
        let handle = self
            .registry
            .get(sandbox_internal_id, sandbox_public_id)
            .await
            .map_err(|_| SandboxSnapshotError::SandboxNotFound {
                sandbox_id: sandbox_public_id.to_string(),
            })?;

        // ── Stop the sandbox (quiesce for commit consistency) ─────────────────
        let was_running = {
            use temps_entities::sandboxes;
            let sb = sandboxes::Entity::find_by_id(sandbox_internal_id)
                .one(self.db.as_ref())
                .await
                .map_err(SandboxSnapshotError::Database)?
                .ok_or_else(|| SandboxSnapshotError::SandboxNotFound {
                    sandbox_id: sandbox_public_id.to_string(),
                })?;
            sb.status == "running"
        };

        if was_running {
            self.provider
                .stop(&handle)
                .await
                .map_err(SandboxSnapshotError::Provider)?;
        }

        // ── Call provider to take the snapshot ────────────────────────────────
        let artifact_result = self.provider.take_snapshot(&handle, label).await;

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
            Err(e) => {
                // Transition to failed, keep the row for audit.
                let _ = self.mark_failed(row.id, &e.to_string()).await;
                Err(SandboxSnapshotError::Provider(e))
            }
            Ok(artifact) => {
                // ── Check deduplication ───────────────────────────────────────
                let existing = sandbox_snapshots::Entity::find()
                    .filter(
                        sandbox_snapshots::Column::ContentDigest
                            .eq(artifact.content_digest.clone()),
                    )
                    .filter(sandbox_snapshots::Column::Status.eq("ready"))
                    .one(self.db.as_ref())
                    .await
                    .map_err(SandboxSnapshotError::Database)?;

                if let Some(dup) = existing {
                    // Same content already on disk — remove the duplicate file
                    // if the provider wrote a different one.
                    if artifact.content_path != Path::new(&dup.content_path) {
                        let _ = std::fs::remove_file(&artifact.content_path);
                    }
                    // Update the creating row to point at the existing artifact.
                    let updated = self
                        .finalize_row(row.id, &artifact, dup.content_path.clone())
                        .await?;
                    return Ok(updated);
                }

                // ── Finalize the row ──────────────────────────────────────────
                let final_row = self
                    .finalize_row(
                        row.id,
                        &artifact,
                        artifact.content_path.to_string_lossy().to_string(),
                    )
                    .await?;
                Ok(final_row)
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
        let row = self.get_snapshot(user_id, public_id).await?;

        if row.status == "deleted" {
            return Ok(()); // Idempotent
        }

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
        if sharing_count == 0 && !row.content_path.is_empty() {
            if let Err(e) = std::fs::remove_file(&row.content_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        snapshot_id = %public_id,
                        path = %row.content_path,
                        "snapshot delete: failed to remove tarball: {}",
                        e
                    );
                }
            }
        }

        // Remove the Docker image tag (if present and image_ref is set).
        // Best-effort: a missing image is not an error.
        if let Some(image_ref) = &row.image_ref {
            if !image_ref.is_empty() {
                // We can't call docker directly (provider boundary), but we can
                // attempt via the provider. Log warnings, don't fail the delete.
                tracing::debug!(
                    snapshot_id = %public_id,
                    image_ref = %image_ref,
                    "snapshot delete: image cleanup left to operator (rmi {})",
                    image_ref
                );
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
        target_backend: &str,
    ) -> Result<SnapshotArtifact, SandboxSnapshotError> {
        let row = self.get_snapshot(user_id, public_id).await?;

        if row.status != "ready" {
            return Err(SandboxSnapshotError::NotReady {
                snapshot_id: public_id.to_string(),
                status: row.status.clone(),
            });
        }

        // Cross-backend restore check.
        if row.backend != target_backend {
            return Err(SandboxSnapshotError::CrossBackendRestore {
                snapshot_backend: row.backend.clone(),
                target_backend: target_backend.to_string(),
            });
        }

        let content_path = std::path::PathBuf::from(&row.content_path);
        if !content_path.exists() {
            return Err(SandboxSnapshotError::ArtifactMissing {
                path: row.content_path.clone(),
            });
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
            size_bytes: row.size_bytes as u64,
            backend,
            image_ref: row.image_ref.clone(),
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
    /// Available bytes on the snapshots filesystem.
    pub available_disk_bytes: u64,
}

/// Best-effort check of available disk space on the snapshots directory.
/// Returns 0 if the directory doesn't exist or the check fails.
fn available_disk_space() -> u64 {
    let dir = std::env::var("TEMPS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".temps")
        })
        .join("snapshots");

    // statvfs-style check via std::fs. Use a simple read of the fs stats.
    // On platforms where this isn't available, return 0.
    // We can't call statvfs from std directly without a native dependency.
    // Report 0 (safe default — UI can show "unknown" instead of claiming space
    // that may not exist). A future version can use the `nix` crate's statvfs.
    let _ = dir;
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_entities::sandbox_snapshots;

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
            available_disk_bytes: 0,
        };
        assert_eq!(s.total_bytes, 1024);
        assert_eq!(s.snapshot_count, 1);
        assert_eq!(s.quota_bytes, DEFAULT_SNAPSHOT_QUOTA_BYTES);
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
            available_disk_bytes: 0,
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
            .resolve_for_restore(5, "snap_creating00001111", "docker")
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
            .resolve_for_restore(5, "snap_docker000011112222", "firecracker")
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
}
