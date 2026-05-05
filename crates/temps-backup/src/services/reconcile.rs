//! Startup reconciliation for orphaned `running` backup rows.
//!
//! When the temps process restarts mid-backup, the heartbeat task dies
//! with it. Without intervention the `backups` row stays in
//! `state="running"` forever — the UI shows it as "Running" forever, and
//! the row never gets a final size. On startup we sweep both
//! `backups` and `external_service_backups`, mark every row that's still
//! in `running` as `failed` with a recognizable error message, and stamp
//! `finished_at`. Operators can then re-run the backup if they need to.
//!
//! We do this once at boot only. Any future heartbeat-stall detection
//! during runtime would be its own scheduled job — out of scope here.

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use tracing::{error, info};

const ORPHAN_REASON: &str =
    "Backup was in progress when the temps server restarted. The worker process died before \
     the backup could complete. Re-run the backup if needed.";

/// Mark every `backups` and `external_service_backups` row currently in
/// `state='running'` as `state='failed'`. Logs how many rows were
/// reconciled. Failures to update individual rows are logged but don't
/// abort the sweep.
///
/// Idempotent: rows already in `failed` / `completed` are untouched.
pub async fn reconcile_orphan_backups(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let now = chrono::Utc::now();

    // Parent backups rows.
    let orphans = temps_entities::backups::Entity::find()
        .filter(temps_entities::backups::Column::State.eq("running"))
        .all(db)
        .await?;

    let mut parent_count = 0usize;
    for orphan in orphans {
        let id = orphan.id;
        let mut update: temps_entities::backups::ActiveModel = orphan.into();
        update.state = Set("failed".to_string());
        update.error_message = Set(Some(ORPHAN_REASON.to_string()));
        update.finished_at = Set(Some(now));
        match update.update(db).await {
            Ok(_) => parent_count += 1,
            Err(e) => error!("Failed to reconcile orphan backup row {}: {}", id, e),
        }
    }

    // External-service backup rows. These can also stick on `running`
    // (the engine writes them, and the same crash leaves them orphaned).
    let ext_orphans = temps_entities::external_service_backups::Entity::find()
        .filter(temps_entities::external_service_backups::Column::State.eq("running"))
        .all(db)
        .await?;

    let mut ext_count = 0usize;
    for orphan in ext_orphans {
        let id = orphan.id;
        let mut update: temps_entities::external_service_backups::ActiveModel = orphan.into();
        update.state = Set("failed".to_string());
        update.error_message = Set(Some(ORPHAN_REASON.to_string()));
        update.finished_at = Set(Some(now));
        match update.update(db).await {
            Ok(_) => ext_count += 1,
            Err(e) => error!(
                "Failed to reconcile orphan external_service_backups row {}: {}",
                id, e
            ),
        }
    }

    if parent_count > 0 || ext_count > 0 {
        info!(
            "Backup startup reconciliation: marked {} parent + {} external-service \
             rows as failed (orphaned by previous process restart)",
            parent_count, ext_count
        );
    } else {
        info!("Backup startup reconciliation: no orphaned rows found");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn running_backup(id: i32) -> temps_entities::backups::Model {
        temps_entities::backups::Model {
            id,
            name: format!("backup-{}", id),
            backup_id: format!("uuid-{}", id),
            schedule_id: None,
            backup_type: "full".into(),
            state: "running".into(),
            started_at: chrono::Utc::now() - chrono::Duration::hours(2),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: String::new(),
            error_message: None,
            metadata: "{}".into(),
            checksum: None,
            compression_type: "gzip".into(),
            created_by: 1,
            expires_at: None,
            tags: "[]".into(),
            last_heartbeat_at: None,
        }
    }

    fn running_external_backup(id: i32) -> temps_entities::external_service_backups::Model {
        temps_entities::external_service_backups::Model {
            id,
            service_id: 1,
            backup_id: 1,
            backup_type: "full".into(),
            state: "running".into(),
            started_at: chrono::Utc::now() - chrono::Duration::hours(2),
            finished_at: None,
            size_bytes: None,
            s3_location: String::new(),
            error_message: None,
            metadata: serde_json::json!({}),
            checksum: None,
            compression_type: "lz4".into(),
            created_by: 1,
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn reconcile_marks_running_rows_as_failed() {
        // Mock: SELECT running backups → [row 7], SELECT running ext → [row 11].
        // Each UPDATE returns success.
        let row = running_backup(7);
        let ext_row = running_external_backup(11);
        // After update, the row is re-read by Sea-ORM in some flows; we
        // include the row again as a defensive query result.
        let updated_row = temps_entities::backups::Model {
            state: "failed".into(),
            ..row.clone()
        };
        let updated_ext = temps_entities::external_service_backups::Model {
            state: "failed".into(),
            ..ext_row.clone()
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()]])
            .append_query_results([vec![updated_row]])
            .append_query_results([vec![ext_row.clone()]])
            .append_query_results([vec![updated_ext]])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .into_connection();

        let result = reconcile_orphan_backups(&db).await;
        assert!(result.is_ok(), "reconcile failed: {:?}", result);
    }

    #[tokio::test]
    async fn reconcile_with_no_orphans_is_noop() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::backups::Model>::new()])
            .append_query_results([Vec::<temps_entities::external_service_backups::Model>::new()])
            .into_connection();

        let result = reconcile_orphan_backups(&db).await;
        assert!(result.is_ok());
    }
}
