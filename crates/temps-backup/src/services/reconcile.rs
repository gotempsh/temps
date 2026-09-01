// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reconciliation for orphaned `running` backup rows at server boot.
//!
//! When the temps process restarts mid-backup, the in-process task that
//! was driving the engine dies with it. Without intervention the
//! `backups` row stays in `state="running"` forever — the UI shows it as
//! "Running" forever, and the row never gets a final size.
//!
//! On boot we mark every `running` row in `backups` and
//! `external_service_backups` as `failed`, stamping `finished_at` from
//! `started_at + grace`. That's safe because the runtime is the source
//! of truth: anything the DB thinks is running but isn't in the new
//! process's executor map is definitively dead.
//!
//! No mid-run "stalled" sweep. The temps process during a backup is
//! parked awaiting Docker/S3 I/O — its liveness has zero correlation
//! with the actual backup's progress, so process-side heartbeats are
//! theater. The wall-clock `max_runtime_secs` timeout in
//! `BackupExecutor` covers the case where a backup genuinely hangs.

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use tracing::{error, info};

/// Grace period added to `started_at` when stamping `finished_at` on a
/// reconciled row. Small enough that the displayed duration isn't
/// literally zero, but not enough to be misleading.
const ORPHAN_GRACE: chrono::Duration = chrono::Duration::seconds(30);

/// Sweep at boot. Marks every `running` row as `failed`.
pub async fn reconcile_orphan_backups(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    let (parent_count, ext_count) = fail_running_backups(db).await?;

    if parent_count > 0 || ext_count > 0 {
        info!(
            parent_count,
            ext_count,
            "Backup startup reconciliation: marked rows as failed (orphaned by previous process restart)"
        );
    } else {
        info!("Backup startup reconciliation: no orphaned rows found");
    }
    Ok(())
}

async fn fail_running_backups(db: &DatabaseConnection) -> Result<(usize, usize), sea_orm::DbErr> {
    let now = Utc::now();

    // ---- Parent `backups` ----------------------------------------------
    let candidates = temps_entities::backups::Entity::find()
        .filter(temps_entities::backups::Column::State.eq("running"))
        .all(db)
        .await?;

    let mut parent_count = 0usize;
    for row in candidates {
        let id = row.id;
        let finished_at = (row.started_at + ORPHAN_GRACE).min(now);
        let message = build_message(row.started_at, finished_at);

        let mut update: temps_entities::backups::ActiveModel = row.into();
        update.state = Set("failed".to_string());
        update.error_message = Set(Some(message));
        update.finished_at = Set(Some(finished_at));
        match update.update(db).await {
            Ok(_) => parent_count += 1,
            Err(e) => error!("Failed to reconcile orphan backup row {}: {}", id, e),
        }
    }

    // ---- Child `external_service_backups` ------------------------------
    let ext_candidates = temps_entities::external_service_backups::Entity::find()
        .filter(temps_entities::external_service_backups::Column::State.eq("running"))
        .all(db)
        .await?;

    let mut ext_count = 0usize;
    for row in ext_candidates {
        let id = row.id;
        let finished_at = (row.started_at + ORPHAN_GRACE).min(now);
        let message = build_message(row.started_at, finished_at);

        let mut update: temps_entities::external_service_backups::ActiveModel = row.into();
        update.state = Set("failed".to_string());
        update.error_message = Set(Some(message));
        update.finished_at = Set(Some(finished_at));
        match update.update(db).await {
            Ok(_) => ext_count += 1,
            Err(e) => error!(
                "Failed to reconcile orphan external_service_backups row {}: {}",
                id, e
            ),
        }
    }

    Ok((parent_count, ext_count))
}

fn build_message(started_at: chrono::DateTime<Utc>, finished_at: chrono::DateTime<Utc>) -> String {
    format!(
        "The temps server was restarted while this backup was running. \
         The backup runner will not resume it automatically — please re-trigger the backup. \
         (started {}, marked failed at {})",
        started_at.to_rfc3339(),
        finished_at.to_rfc3339(),
    )
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
            schedule_run_id: None,
            backup_type: "full".into(),
            state: "running".into(),
            started_at: Utc::now() - chrono::Duration::hours(2),
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
        }
    }

    fn running_external_backup(id: i32) -> temps_entities::external_service_backups::Model {
        temps_entities::external_service_backups::Model {
            id,
            service_id: 1,
            backup_id: 1,
            backup_type: "full".into(),
            state: "running".into(),
            started_at: Utc::now() - chrono::Duration::hours(2),
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

    #[test]
    fn build_message_includes_started_at() {
        let started = Utc::now() - chrono::Duration::hours(1);
        let finished = started + ORPHAN_GRACE;
        let msg = build_message(started, finished);
        assert!(msg.contains("restarted"));
        assert!(msg.contains(&started.to_rfc3339()));
    }

    #[tokio::test]
    async fn reconcile_marks_running_rows_as_failed() {
        let row = running_backup(7);
        let ext_row = running_external_backup(11);
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
