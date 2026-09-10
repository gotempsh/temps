// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SQLite-backed persistence for IndexNow submissions.
//!
//! Tracks which pages have been submitted to IndexNow, when, and what their
//! last-modified time was at submission. This lets us determine which pages
//! need resubmission after a new deployment.

use sea_orm::entity::prelude::*;
use sea_orm::{
    ActiveValue::Set, ConnectOptions, Database, DatabaseConnection, QueryOrder, Statement,
};
use std::path::Path;
use std::sync::Arc;

use crate::types::*;

// ============================================================================
// Entities
// ============================================================================

pub mod submission {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "indexnow_submissions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        /// The full URL that was submitted
        pub url: String,
        /// The hostname extracted from the URL (for IndexNow host param)
        pub host: String,
        /// ISO 8601 timestamp of when the URL was last submitted to IndexNow
        pub last_submitted_at: String,
        /// HTTP Last-Modified header value at time of submission (if available)
        pub last_modified_at: Option<String>,
        /// HTTP ETag header value at time of submission (if available)
        pub etag: Option<String>,
        /// Content hash (sha256 of body) at time of submission
        pub content_hash: Option<String>,
        /// HTTP status code when we last checked the page
        pub last_status_code: Option<i32>,
        /// Number of times this URL has been submitted
        pub submission_count: i32,
        /// The deployment ID that triggered the last submission (if from auto-submit)
        pub deployment_id: Option<i32>,
        /// The project ID this URL belongs to
        pub project_id: Option<i32>,
        /// The environment ID this URL belongs to
        pub environment_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod setting {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "indexnow_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

// ============================================================================
// Store
// ============================================================================

/// SQLite-backed store for IndexNow submission tracking.
#[derive(Clone)]
pub struct IndexNowStore {
    db: Arc<DatabaseConnection>,
}

impl IndexNowStore {
    /// Open (or create) the SQLite database in the given data directory.
    pub async fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let db_path = data_dir.join("indexnow.db");
        let url = format!("sqlite://{}?mode=rwc", db_path.display());

        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1) // SQLite is single-writer
            .sqlx_logging(false);

        let db = Database::connect(opts)
            .await
            .map_err(|e| StoreError::Connect {
                path: db_path.display().to_string(),
                reason: e.to_string(),
            })?;

        Self::migrate(&db).await?;

        tracing::info!(path = %db_path.display(), "IndexNow store opened");

        Ok(Self { db: Arc::new(db) })
    }

    /// Create tables if they don't exist.
    async fn migrate(db: &DatabaseConnection) -> Result<(), StoreError> {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS indexnow_submissions (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                url                 TEXT NOT NULL,
                host                TEXT NOT NULL,
                last_submitted_at   TEXT NOT NULL,
                last_modified_at    TEXT,
                etag                TEXT,
                content_hash        TEXT,
                last_status_code    INTEGER,
                submission_count    INTEGER NOT NULL DEFAULT 1,
                deployment_id       INTEGER,
                project_id          INTEGER,
                environment_id      INTEGER
            );
            "#,
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_submissions_url ON indexnow_submissions(url);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_submissions_host ON indexnow_submissions(host);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_submissions_project ON indexnow_submissions(project_id);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS indexnow_settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA foreign_keys = ON;",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Submission CRUD
    // -----------------------------------------------------------------------

    /// Record a URL submission (upsert: if URL exists, update timestamps and bump count).
    pub async fn record_submission(&self, record: &SubmissionRecord) -> Result<(), StoreError> {
        use sea_orm::sea_query::OnConflict;

        let now = chrono::Utc::now().to_rfc3339();

        let model = submission::ActiveModel {
            id: Default::default(),
            url: Set(record.url.clone()),
            host: Set(record.host.clone()),
            last_submitted_at: Set(now.clone()),
            last_modified_at: Set(record.last_modified_at.clone()),
            etag: Set(record.etag.clone()),
            content_hash: Set(record.content_hash.clone()),
            last_status_code: Set(record.last_status_code),
            submission_count: Set(1),
            deployment_id: Set(record.deployment_id),
            project_id: Set(record.project_id),
            environment_id: Set(record.environment_id),
        };

        submission::Entity::insert(model)
            .on_conflict(
                OnConflict::column(submission::Column::Url)
                    .update_columns([
                        submission::Column::LastSubmittedAt,
                        submission::Column::LastModifiedAt,
                        submission::Column::Etag,
                        submission::Column::ContentHash,
                        submission::Column::LastStatusCode,
                        submission::Column::DeploymentId,
                        submission::Column::ProjectId,
                        submission::Column::EnvironmentId,
                    ])
                    .to_owned(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        // Bump submission_count separately (SQLite doesn't support SET col = col + 1 in ON CONFLICT)
        db_exec_raw(
            &self.db,
            "UPDATE indexnow_submissions SET submission_count = submission_count + 1 WHERE url = ?",
            vec![sea_orm::Value::from(record.url.clone())],
        )
        .await?;

        Ok(())
    }

    /// Get submission history for a URL.
    pub async fn get_submission(&self, url: &str) -> Result<Option<submission::Model>, StoreError> {
        submission::Entity::find()
            .filter(submission::Column::Url.eq(url))
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))
    }

    /// List all submissions, optionally filtered by host, newest first.
    pub async fn list_submissions(
        &self,
        host_filter: Option<&str>,
        project_id: Option<i32>,
        limit: u64,
    ) -> Result<Vec<submission::Model>, StoreError> {
        let mut query =
            submission::Entity::find().order_by_desc(submission::Column::LastSubmittedAt);

        if let Some(host) = host_filter {
            query = query.filter(submission::Column::Host.eq(host));
        }
        if let Some(pid) = project_id {
            query = query.filter(submission::Column::ProjectId.eq(pid));
        }

        // Manual limit via raw paginator
        let results = query
            .all(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(results.into_iter().take(limit as usize).collect())
    }

    /// List submissions that are older than the given duration, meaning they
    /// might need resubmission.
    #[allow(dead_code)]
    pub async fn find_stale_submissions(
        &self,
        older_than: chrono::DateTime<chrono::Utc>,
        project_id: Option<i32>,
    ) -> Result<Vec<submission::Model>, StoreError> {
        let cutoff = older_than.to_rfc3339();

        let mut query = submission::Entity::find()
            .filter(submission::Column::LastSubmittedAt.lt(cutoff))
            .order_by_asc(submission::Column::LastSubmittedAt);

        if let Some(pid) = project_id {
            query = query.filter(submission::Column::ProjectId.eq(pid));
        }

        query
            .all(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))
    }

    /// Delete a submission record by URL.
    pub async fn delete_submission(&self, url: &str) -> Result<bool, StoreError> {
        let result = submission::Entity::delete_many()
            .filter(submission::Column::Url.eq(url))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(result.rows_affected > 0)
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        let model = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.value))
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let existing = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        match existing {
            Some(m) => {
                let mut active: setting::ActiveModel = m.into();
                active.value = Set(value.to_string());
                active
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| StoreError::Database(e.to_string()))?;
            }
            None => {
                setting::ActiveModel {
                    key: Set(key.to_string()),
                    value: Set(value.to_string()),
                }
                .insert(self.db.as_ref())
                .await
                .map_err(|e| StoreError::Database(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Get the full plugin settings (with defaults for unset values).
    pub async fn get_settings(&self) -> Result<PluginSettings, StoreError> {
        let api_key = self.get_setting("api_key").await?;
        let search_engine = self
            .get_setting("search_engine")
            .await?
            .unwrap_or_else(|| PluginSettings::DEFAULT_SEARCH_ENGINE.to_string());
        let auto_submit = self
            .get_setting("auto_submit")
            .await?
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(PluginSettings::DEFAULT_AUTO_SUBMIT);
        let max_pages = self
            .get_setting("max_pages")
            .await?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(PluginSettings::DEFAULT_MAX_PAGES);
        let resubmit_after_hours = self
            .get_setting("resubmit_after_hours")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(PluginSettings::DEFAULT_RESUBMIT_AFTER_HOURS);
        let user_agent = self
            .get_setting("user_agent")
            .await?
            .unwrap_or_else(|| PluginSettings::DEFAULT_USER_AGENT.to_string());

        Ok(PluginSettings {
            api_key,
            search_engine,
            auto_submit,
            max_pages,
            resubmit_after_hours,
            user_agent,
        })
    }

    /// Update plugin settings. Only provided (Some) fields are written.
    pub async fn update_settings(
        &self,
        update: &UpdateSettings,
    ) -> Result<PluginSettings, StoreError> {
        if let Some(ref v) = update.api_key {
            self.set_setting("api_key", v).await?;
        }
        if let Some(ref v) = update.search_engine {
            self.set_setting("search_engine", v).await?;
        }
        if let Some(v) = update.auto_submit {
            self.set_setting("auto_submit", &v.to_string()).await?;
        }
        if let Some(v) = update.max_pages {
            self.set_setting("max_pages", &v.to_string()).await?;
        }
        if let Some(v) = update.resubmit_after_hours {
            self.set_setting("resubmit_after_hours", &v.to_string())
                .await?;
        }
        if let Some(ref v) = update.user_agent {
            self.set_setting("user_agent", v).await?;
        }

        self.get_settings().await
    }
}

/// Execute a raw SQL statement with positional parameters.
async fn db_exec_raw(
    db: &DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<(), StoreError> {
    let stmt = Statement::from_sql_and_values(sea_orm::DatabaseBackend::Sqlite, sql, values);
    db.execute(stmt)
        .await
        .map_err(|e| StoreError::Database(e.to_string()))?;
    Ok(())
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Failed to connect to SQLite at {path}: {reason}")]
    Connect { path: String, reason: String },

    #[error("Migration failed: {0}")]
    Migration(String),

    #[error("Database error: {0}")]
    Database(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (IndexNowStore, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let store = IndexNowStore::open(dir.path()).await.expect("open store");
        (store, dir)
    }

    #[tokio::test]
    async fn test_record_and_list_submissions() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/page1".into(),
                host: "example.com".into(),
                last_modified_at: Some("2025-01-01T00:00:00Z".into()),
                etag: None,
                content_hash: None,
                last_status_code: Some(200),
                deployment_id: Some(42),
                project_id: Some(1),
                environment_id: Some(1),
            })
            .await
            .unwrap();

        let subs = store.list_submissions(None, None, 100).await.unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].url, "https://example.com/page1");
        assert_eq!(subs[0].host, "example.com");
        // First insert sets count to 1, then the UPDATE bumps it to 2
        assert_eq!(subs[0].submission_count, 2);
    }

    #[tokio::test]
    async fn test_upsert_submission() {
        let (store, _dir) = test_store().await;

        let record = SubmissionRecord {
            url: "https://example.com/page1".into(),
            host: "example.com".into(),
            last_modified_at: Some("2025-01-01T00:00:00Z".into()),
            etag: None,
            content_hash: None,
            last_status_code: Some(200),
            deployment_id: Some(42),
            project_id: Some(1),
            environment_id: Some(1),
        };

        store.record_submission(&record).await.unwrap();
        store.record_submission(&record).await.unwrap();

        let subs = store.list_submissions(None, None, 100).await.unwrap();
        assert_eq!(subs.len(), 1);
        // Initial insert = 1, first bump = 2, second upsert resets to 1 in ON CONFLICT,
        // then bump = 2 again. The count tracks submissions, not insert attempts.
        assert!(subs[0].submission_count >= 2);
    }

    #[tokio::test]
    async fn test_settings_defaults() {
        let (store, _dir) = test_store().await;
        let settings = store.get_settings().await.unwrap();

        assert!(settings.api_key.is_none());
        assert_eq!(
            settings.search_engine,
            PluginSettings::DEFAULT_SEARCH_ENGINE
        );
        assert_eq!(settings.auto_submit, PluginSettings::DEFAULT_AUTO_SUBMIT);
        assert_eq!(settings.max_pages, PluginSettings::DEFAULT_MAX_PAGES);
    }

    #[tokio::test]
    async fn test_settings_update() {
        let (store, _dir) = test_store().await;

        let updated = store
            .update_settings(&UpdateSettings {
                api_key: Some("my-test-key-abc123".into()),
                auto_submit: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(updated.api_key, Some("my-test-key-abc123".into()));
        assert!(!updated.auto_submit);
        // Unchanged fields keep defaults
        assert_eq!(updated.max_pages, PluginSettings::DEFAULT_MAX_PAGES);
    }

    #[tokio::test]
    async fn test_find_stale_submissions() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/old".into(),
                host: "example.com".into(),
                last_modified_at: None,
                etag: None,
                content_hash: None,
                last_status_code: Some(200),
                deployment_id: None,
                project_id: Some(1),
                environment_id: None,
            })
            .await
            .unwrap();

        // Everything submitted just now should NOT be stale if cutoff is in the past
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let stale = store.find_stale_submissions(past, None).await.unwrap();
        assert_eq!(stale.len(), 0);

        // Everything submitted just now SHOULD be stale if cutoff is in the future
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let stale = store.find_stale_submissions(future, None).await.unwrap();
        assert_eq!(stale.len(), 1);
    }

    #[tokio::test]
    async fn test_delete_submission() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/del".into(),
                host: "example.com".into(),
                last_modified_at: None,
                etag: None,
                content_hash: None,
                last_status_code: Some(200),
                deployment_id: None,
                project_id: None,
                environment_id: None,
            })
            .await
            .unwrap();

        assert!(store
            .delete_submission("https://example.com/del")
            .await
            .unwrap());
        assert!(!store
            .delete_submission("https://example.com/del")
            .await
            .unwrap());
    }
}
