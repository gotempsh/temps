// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only backup lookups, listing, fetching, open alerts and live size.

use super::*;

impl BackupService {
    pub async fn list_backups(
        &self,
        s3_source_id: i32,
    ) -> Result<Vec<temps_entities::backups::Model>, BackupError> {
        let backups = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::S3SourceId.eq(s3_source_id))
            .order_by_desc(temps_entities::backups::Column::StartedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(backups)
    }

    /// Return every unresolved backup alert, newest first. The schedule JOIN
    /// is owned by the service layer so handlers do not access the database
    /// directly. Raw database failures are logged here and converted to a
    /// stable client-safe error.
    pub async fn list_open_backup_alerts(&self) -> Result<Vec<BackupAlertEntry>, BackupError> {
        let sql = r#"
SELECT
    a.id,
    a.kind,
    a.severity,
    a.schedule_id,
    s.name             AS schedule_name,
    s.s3_source_id     AS schedule_s3_source_id,
    a.message,
    a.opened_at
FROM backup_alerts a
LEFT JOIN backup_schedules s ON s.id = a.schedule_id
WHERE a.resolved_at IS NULL
ORDER BY a.opened_at DESC
"#;

        BackupAlertEntry::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|db_error| {
            error!(error = %db_error, "failed to query open backup alerts");
            BackupError::Internal {
                message: "Failed to list open backup alerts".to_string(),
            }
        })
    }

    /// Get a backup by ID
    pub async fn get_backup(&self, backup_id: &str) -> Result<Option<Backup>, BackupError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let model = temps_entities::backups::Entity::find()
            .filter(temps_entities::backups::Column::BackupId.eq(backup_id.to_string()))
            .one(self.db.as_ref())
            .await?;

        Ok(model)
    }

    /// Best-effort progress size for a running backup.
    ///
    /// Computed by listing the backup's S3 location and summing the
    /// reported sizes. Returns `None` for non-running backups (their
    /// `size_bytes` is authoritative once finished) and for backups
    /// whose `s3_location` isn't usable yet (engine still warming up).
    /// Errors talking to S3 are downgraded to `None` and logged — the
    /// detail page is best-effort.
    pub async fn compute_live_size(&self, backup: &Backup) -> Option<i64> {
        if backup.state != "running" {
            return None;
        }
        if backup.s3_location.is_empty() {
            return None;
        }

        let s3_source = match temps_entities::s3_sources::Entity::find_by_id(backup.s3_source_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(src)) => src,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    "Failed to load s3_source {} for live size: {}",
                    backup.s3_source_id, e
                );
                return None;
            }
        };

        let s3_client = match self.create_s3_client(&s3_source).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Failed to build S3 client for live-size lookup on backup {}: {}",
                    backup.id, e
                );
                return None;
            }
        };

        // The location can be either an `s3://bucket/key` URL (WAL-G,
        // cluster) or a bucket-relative key (pg_dump, mongodump). Try the
        // URL form first; fall back to treating the value as a key.
        let bucket = &s3_source.bucket_name;
        let key = if let Some((url_bucket, url_key)) =
            temps_providers::externalsvc::s3_util::parse_s3_url(&backup.s3_location)
        {
            // Sanity: only list inside our configured bucket. WAL-G's
            // prefix should always live in this same bucket.
            if &url_bucket != bucket {
                debug!(
                    "live size: s3:// bucket {} != configured bucket {}, listing anyway",
                    url_bucket, bucket
                );
            }
            url_key
        } else {
            backup.s3_location.trim_start_matches('/').to_string()
        };

        // Append a trailing slash if the key looks like a prefix (no file
        // extension). list_objects_v2 doesn't care, but this matches what
        // the engines pass elsewhere.
        let prefix = if key.ends_with('/') || key.contains('.') {
            key
        } else {
            format!("{}/", key)
        };

        match temps_providers::externalsvc::s3_util::list_total_size(&s3_client, bucket, &prefix)
            .await
        {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(e) => {
                debug!(
                    "live size lookup failed for backup {} ({}): {}",
                    backup.id, prefix, e
                );
                None
            }
        }
    }
}
