// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SQLite-backed persistence for Google Indexing API submissions.
//!
//! Tracks which URLs have been submitted to Google, their notification type,
//! response status, and quota usage. Settings and the encrypted service
//! account key are stored in a key-value settings table.

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
    #[sea_orm(table_name = "google_indexing_submissions")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        /// The full URL that was submitted
        pub url: String,
        /// The hostname extracted from the URL
        pub host: String,
        /// Notification type: URL_UPDATED or URL_DELETED
        pub notification_type: String,
        /// ISO 8601 timestamp of when the URL was last submitted
        pub submitted_at: String,
        /// HTTP status code from Google's response
        pub google_response_status: Option<i32>,
        /// The notifyTime returned by Google (RFC 3339)
        pub notify_time: Option<String>,
        /// Number of times this URL has been submitted
        pub submission_count: i32,
        /// The deployment ID that triggered the last submission
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
    #[sea_orm(table_name = "google_indexing_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod quota_log {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "google_indexing_quota_log")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        /// Date in YYYY-MM-DD format (UTC)
        pub date: String,
        /// Number of URLs submitted on this date
        pub urls_submitted: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

// ============================================================================
// Store
// ============================================================================

/// SQLite-backed store for Google Indexing API submission tracking.
#[derive(Clone)]
pub struct GoogleIndexingStore {
    db: Arc<DatabaseConnection>,
}

impl GoogleIndexingStore {
    /// Open (or create) the SQLite database in the given data directory.
    pub async fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let db_path = data_dir.join("google-indexing.db");
        let url = format!("sqlite://{}?mode=rwc", db_path.display());

        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1).sqlx_logging(false);

        let db = Database::connect(opts)
            .await
            .map_err(|e| StoreError::Connect {
                path: db_path.display().to_string(),
                reason: e.to_string(),
            })?;

        Self::migrate(&db).await?;

        tracing::info!(path = %db_path.display(), "Google Indexing store opened");

        Ok(Self { db: Arc::new(db) })
    }

    /// Create tables if they don't exist.
    async fn migrate(db: &DatabaseConnection) -> Result<(), StoreError> {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS google_indexing_submissions (
                id                      INTEGER PRIMARY KEY AUTOINCREMENT,
                url                     TEXT NOT NULL,
                host                    TEXT NOT NULL,
                notification_type       TEXT NOT NULL DEFAULT 'URL_UPDATED',
                submitted_at            TEXT NOT NULL,
                google_response_status  INTEGER,
                notify_time             TEXT,
                submission_count        INTEGER NOT NULL DEFAULT 1,
                deployment_id           INTEGER,
                project_id              INTEGER,
                environment_id          INTEGER
            );
            "#,
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_gi_submissions_url ON google_indexing_submissions(url);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_gi_submissions_host ON google_indexing_submissions(host);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_gi_submissions_project ON google_indexing_submissions(project_id);",
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS google_indexing_settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS google_indexing_quota_log (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                date            TEXT NOT NULL,
                urls_submitted  INTEGER NOT NULL DEFAULT 0
            );
            "#,
        ))
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_gi_quota_date ON google_indexing_quota_log(date);",
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
            notification_type: Set(record.notification_type.clone()),
            submitted_at: Set(now),
            google_response_status: Set(record.google_response_status),
            notify_time: Set(record.notify_time.clone()),
            submission_count: Set(1),
            deployment_id: Set(record.deployment_id),
            project_id: Set(record.project_id),
            environment_id: Set(record.environment_id),
        };

        submission::Entity::insert(model)
            .on_conflict(
                OnConflict::column(submission::Column::Url)
                    .update_columns([
                        submission::Column::NotificationType,
                        submission::Column::SubmittedAt,
                        submission::Column::GoogleResponseStatus,
                        submission::Column::NotifyTime,
                        submission::Column::DeploymentId,
                        submission::Column::ProjectId,
                        submission::Column::EnvironmentId,
                    ])
                    .to_owned(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        // Bump submission_count separately
        db_exec_raw(
            &self.db,
            "UPDATE google_indexing_submissions SET submission_count = submission_count + 1 WHERE url = ?",
            vec![sea_orm::Value::from(record.url.clone())],
        )
        .await?;

        Ok(())
    }

    /// Get submission history for a URL.
    #[allow(dead_code)]
    pub async fn get_submission(&self, url: &str) -> Result<Option<submission::Model>, StoreError> {
        submission::Entity::find()
            .filter(submission::Column::Url.eq(url))
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))
    }

    /// List all submissions, optionally filtered, newest first.
    pub async fn list_submissions(
        &self,
        host_filter: Option<&str>,
        project_id: Option<i32>,
        limit: u64,
    ) -> Result<Vec<submission::Model>, StoreError> {
        let mut query = submission::Entity::find().order_by_desc(submission::Column::SubmittedAt);

        if let Some(host) = host_filter {
            query = query.filter(submission::Column::Host.eq(host));
        }
        if let Some(pid) = project_id {
            query = query.filter(submission::Column::ProjectId.eq(pid));
        }

        let results = query
            .all(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(results.into_iter().take(limit as usize).collect())
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
    // Quota tracking
    // -----------------------------------------------------------------------

    /// Get the number of URLs submitted today (UTC).
    pub async fn get_today_usage(&self) -> Result<usize, StoreError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        let model = quota_log::Entity::find()
            .filter(quota_log::Column::Date.eq(&today))
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.urls_submitted as usize).unwrap_or(0))
    }

    /// Increment today's quota usage by the given count.
    pub async fn increment_quota(&self, count: usize) -> Result<(), StoreError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Use INSERT OR IGNORE + UPDATE pattern for SQLite upsert
        db_exec_raw(
            &self.db,
            "INSERT OR IGNORE INTO google_indexing_quota_log (date, urls_submitted) VALUES (?, 0)",
            vec![sea_orm::Value::from(today.clone())],
        )
        .await?;

        db_exec_raw(
            &self.db,
            "UPDATE google_indexing_quota_log SET urls_submitted = urls_submitted + ? WHERE date = ?",
            vec![
                sea_orm::Value::from(count as i32),
                sea_orm::Value::from(today),
            ],
        )
        .await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        let model = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.value))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
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

    /// Delete a setting by key.
    pub async fn delete_setting(&self, key: &str) -> Result<(), StoreError> {
        setting::Entity::delete_many()
            .filter(setting::Column::Key.eq(key))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;
        Ok(())
    }

    /// Get the full plugin settings.
    pub async fn get_settings(&self) -> Result<PluginSettings, StoreError> {
        let sa_key_json = self.get_setting("service_account_key").await?;
        let sa_email = sa_key_json.as_ref().and_then(|json| {
            serde_json::from_str::<ServiceAccountKey>(json)
                .ok()
                .map(|k| k.client_email)
        });

        let auto_submit = self
            .get_setting("auto_submit")
            .await?
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(PluginSettings::DEFAULT_AUTO_SUBMIT);

        let max_urls_per_deploy = self
            .get_setting("max_urls_per_deploy")
            .await?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(PluginSettings::DEFAULT_MAX_URLS_PER_DEPLOY);

        let daily_quota = self
            .get_setting("daily_quota")
            .await?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(PluginSettings::DEFAULT_DAILY_QUOTA);

        let urls_submitted_today = self.get_today_usage().await?;

        Ok(PluginSettings {
            service_account_configured: sa_key_json.is_some(),
            service_account_email: sa_email,
            auto_submit,
            max_urls_per_deploy,
            daily_quota,
            urls_submitted_today,
        })
    }

    /// Update plugin settings. Only provided (Some) fields are written.
    pub async fn update_settings(
        &self,
        update: &UpdateSettings,
    ) -> Result<PluginSettings, StoreError> {
        if let Some(v) = update.auto_submit {
            self.set_setting("auto_submit", &v.to_string()).await?;
        }
        if let Some(v) = update.max_urls_per_deploy {
            self.set_setting("max_urls_per_deploy", &v.to_string())
                .await?;
        }
        if let Some(v) = update.daily_quota {
            self.set_setting("daily_quota", &v.to_string()).await?;
        }

        self.get_settings().await
    }

    /// Store the service account key JSON.
    pub async fn set_service_account_key(&self, key_json: &str) -> Result<(), StoreError> {
        // Validate the JSON first
        serde_json::from_str::<ServiceAccountKey>(key_json).map_err(|e| {
            StoreError::Validation(format!("Invalid service account key JSON: {}", e))
        })?;
        self.set_setting("service_account_key", key_json).await
    }

    /// Get the service account key JSON (if configured).
    pub async fn get_service_account_key(&self) -> Result<Option<ServiceAccountKey>, StoreError> {
        let json = self.get_setting("service_account_key").await?;
        match json {
            Some(j) => {
                let key = serde_json::from_str::<ServiceAccountKey>(&j).map_err(|e| {
                    StoreError::Database(format!(
                        "Failed to parse stored service account key: {}",
                        e
                    ))
                })?;
                Ok(Some(key))
            }
            None => Ok(None),
        }
    }

    /// Remove the service account key.
    pub async fn delete_service_account_key(&self) -> Result<(), StoreError> {
        self.delete_setting("service_account_key").await
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

    #[error("Validation error: {0}")]
    Validation(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (GoogleIndexingStore, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let store = GoogleIndexingStore::open(dir.path())
            .await
            .expect("open store");
        (store, dir)
    }

    #[tokio::test]
    async fn test_record_and_list_submissions() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/page1".into(),
                host: "example.com".into(),
                notification_type: "URL_UPDATED".into(),
                google_response_status: Some(200),
                notify_time: Some("2025-01-15T10:30:00.000Z".into()),
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
        assert_eq!(subs[0].notification_type, "URL_UPDATED");
        // First insert sets count to 1, then the UPDATE bumps it to 2
        assert_eq!(subs[0].submission_count, 2);
    }

    #[tokio::test]
    async fn test_upsert_submission() {
        let (store, _dir) = test_store().await;

        let record = SubmissionRecord {
            url: "https://example.com/page1".into(),
            host: "example.com".into(),
            notification_type: "URL_UPDATED".into(),
            google_response_status: Some(200),
            notify_time: None,
            deployment_id: Some(42),
            project_id: Some(1),
            environment_id: Some(1),
        };

        store.record_submission(&record).await.unwrap();
        store.record_submission(&record).await.unwrap();

        let subs = store.list_submissions(None, None, 100).await.unwrap();
        assert_eq!(subs.len(), 1);
        assert!(subs[0].submission_count >= 2);
    }

    #[tokio::test]
    async fn test_delete_submission() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/del".into(),
                host: "example.com".into(),
                notification_type: "URL_UPDATED".into(),
                google_response_status: Some(200),
                notify_time: None,
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

    #[tokio::test]
    async fn test_settings_defaults() {
        let (store, _dir) = test_store().await;
        let settings = store.get_settings().await.unwrap();

        assert!(!settings.service_account_configured);
        assert!(settings.service_account_email.is_none());
        assert_eq!(settings.auto_submit, PluginSettings::DEFAULT_AUTO_SUBMIT);
        assert_eq!(
            settings.max_urls_per_deploy,
            PluginSettings::DEFAULT_MAX_URLS_PER_DEPLOY
        );
        assert_eq!(settings.daily_quota, PluginSettings::DEFAULT_DAILY_QUOTA);
        assert_eq!(settings.urls_submitted_today, 0);
    }

    #[tokio::test]
    async fn test_settings_update() {
        let (store, _dir) = test_store().await;

        let updated = store
            .update_settings(&UpdateSettings {
                auto_submit: Some(false),
                max_urls_per_deploy: Some(25),
                daily_quota: None,
            })
            .await
            .unwrap();

        assert!(!updated.auto_submit);
        assert_eq!(updated.max_urls_per_deploy, 25);
        assert_eq!(updated.daily_quota, PluginSettings::DEFAULT_DAILY_QUOTA);
    }

    #[tokio::test]
    async fn test_quota_tracking() {
        let (store, _dir) = test_store().await;

        assert_eq!(store.get_today_usage().await.unwrap(), 0);

        store.increment_quota(5).await.unwrap();
        assert_eq!(store.get_today_usage().await.unwrap(), 5);

        store.increment_quota(3).await.unwrap();
        assert_eq!(store.get_today_usage().await.unwrap(), 8);
    }

    #[tokio::test]
    async fn test_service_account_key_crud() {
        let (store, _dir) = test_store().await;

        // Initially no key
        assert!(store.get_service_account_key().await.unwrap().is_none());

        // Set a key
        let key_json = r#"{
            "type": "service_account",
            "project_id": "test-project",
            "private_key_id": "key123",
            "private_key": "-----BEGIN RSA PRIVATE KEY-----\ntest\n-----END RSA PRIVATE KEY-----\n",
            "client_email": "test@test.iam.gserviceaccount.com",
            "client_id": "123",
            "auth_uri": "https://accounts.google.com/o/oauth2/auth",
            "token_uri": "https://oauth2.googleapis.com/token"
        }"#;

        store.set_service_account_key(key_json).await.unwrap();

        let key = store.get_service_account_key().await.unwrap().unwrap();
        assert_eq!(key.client_email, "test@test.iam.gserviceaccount.com");
        assert_eq!(key.project_id, "test-project");

        // Settings should reflect configured state
        let settings = store.get_settings().await.unwrap();
        assert!(settings.service_account_configured);
        assert_eq!(
            settings.service_account_email.as_deref(),
            Some("test@test.iam.gserviceaccount.com")
        );

        // Delete key
        store.delete_service_account_key().await.unwrap();
        assert!(store.get_service_account_key().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_invalid_service_account_key() {
        let (store, _dir) = test_store().await;

        let result = store.set_service_account_key("not valid json").await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), StoreError::Validation(_)));
    }

    #[tokio::test]
    async fn test_filter_submissions_by_host() {
        let (store, _dir) = test_store().await;

        store
            .record_submission(&SubmissionRecord {
                url: "https://example.com/a".into(),
                host: "example.com".into(),
                notification_type: "URL_UPDATED".into(),
                google_response_status: Some(200),
                notify_time: None,
                deployment_id: None,
                project_id: None,
                environment_id: None,
            })
            .await
            .unwrap();

        store
            .record_submission(&SubmissionRecord {
                url: "https://other.com/b".into(),
                host: "other.com".into(),
                notification_type: "URL_UPDATED".into(),
                google_response_status: Some(200),
                notify_time: None,
                deployment_id: None,
                project_id: None,
                environment_id: None,
            })
            .await
            .unwrap();

        let all = store.list_submissions(None, None, 100).await.unwrap();
        assert_eq!(all.len(), 2);

        let filtered = store
            .list_submissions(Some("example.com"), None, 100)
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].host, "example.com");
    }
}
