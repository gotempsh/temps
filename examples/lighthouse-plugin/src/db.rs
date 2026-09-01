// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SQLite-backed persistence for Lighthouse audit results.
//!
//! Uses Sea-ORM with a local SQLite file in the plugin's data directory.

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

pub mod audit {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "lighthouse_audits")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub url: String,
        pub performance_score: Option<i32>,
        pub accessibility_score: Option<i32>,
        pub best_practices_score: Option<i32>,
        pub seo_score: Option<i32>,
        pub status: String,
        pub trigger: String,
        pub project_id: Option<i32>,
        pub deployment_id: Option<i32>,
        /// JSON-serialized CoreWebVitals
        pub metrics_json: Option<String>,
        /// JSON-serialized Vec<AuditDiagnostic>
        pub diagnostics_json: String,
        /// Whether raw Lighthouse JSON is stored
        pub has_raw_json: bool,
        pub error_message: Option<String>,
        pub device: String,
        pub created_at: String,
        pub completed_at: Option<String>,
        pub duration_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod setting {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "lighthouse_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod raw_json {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "lighthouse_raw_json")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub audit_id: String,
        pub json_data: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

// ============================================================================
// Store
// ============================================================================

/// SQLite-backed store for Lighthouse audits.
#[derive(Clone)]
pub struct AuditStore {
    db: Arc<DatabaseConnection>,
}

impl AuditStore {
    /// Open (or create) the SQLite database in the given data directory.
    pub async fn open(data_dir: &Path) -> Result<Self, AuditStoreError> {
        let db_path = data_dir.join("lighthouse.db");
        let url = format!("sqlite://{}?mode=rwc", db_path.display());

        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1).sqlx_logging(false);

        let db = Database::connect(opts)
            .await
            .map_err(|e| AuditStoreError::Connect {
                path: db_path.display().to_string(),
                reason: e.to_string(),
            })?;

        Self::migrate(&db).await?;

        tracing::info!(path = %db_path.display(), "Lighthouse audit store opened");

        Ok(Self { db: Arc::new(db) })
    }

    /// Create tables if they don't exist.
    async fn migrate(db: &DatabaseConnection) -> Result<(), AuditStoreError> {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS lighthouse_audits (
                id                   TEXT PRIMARY KEY NOT NULL,
                url                  TEXT NOT NULL,
                performance_score    INTEGER,
                accessibility_score  INTEGER,
                best_practices_score INTEGER,
                seo_score            INTEGER,
                status               TEXT NOT NULL DEFAULT 'running',
                trigger              TEXT NOT NULL DEFAULT 'manual',
                project_id           INTEGER,
                deployment_id        INTEGER,
                metrics_json         TEXT,
                diagnostics_json     TEXT NOT NULL DEFAULT '[]',
                has_raw_json         BOOLEAN NOT NULL DEFAULT 0,
                error_message        TEXT,
                device               TEXT NOT NULL DEFAULT 'mobile',
                created_at           TEXT NOT NULL,
                completed_at         TEXT,
                duration_ms          INTEGER NOT NULL DEFAULT 0
            );
            "#,
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS lighthouse_raw_json (
                audit_id TEXT PRIMARY KEY NOT NULL REFERENCES lighthouse_audits(id) ON DELETE CASCADE,
                json_data TEXT NOT NULL
            );
            "#,
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS lighthouse_settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_lighthouse_audits_project ON lighthouse_audits(project_id);",
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_lighthouse_audits_created ON lighthouse_audits(created_at);",
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA foreign_keys = ON;",
        ))
        .await
        .map_err(|e| AuditStoreError::Migration(e.to_string()))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Audit CRUD
    // -----------------------------------------------------------------------

    /// Insert a new audit in "running" status.
    pub async fn create_audit(
        &self,
        id: &str,
        url: &str,
        trigger: &AuditTrigger,
        project_id: Option<i32>,
        deployment_id: Option<i32>,
        device: &str,
    ) -> Result<(), AuditStoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        let trigger_str = match trigger {
            AuditTrigger::Deployment => "deployment",
            AuditTrigger::Manual => "manual",
        };

        audit::ActiveModel {
            id: Set(id.to_string()),
            url: Set(url.to_string()),
            performance_score: Set(None),
            accessibility_score: Set(None),
            best_practices_score: Set(None),
            seo_score: Set(None),
            status: Set("running".to_string()),
            trigger: Set(trigger_str.to_string()),
            project_id: Set(project_id),
            deployment_id: Set(deployment_id),
            metrics_json: Set(None),
            diagnostics_json: Set("[]".to_string()),
            has_raw_json: Set(false),
            error_message: Set(None),
            device: Set(device.to_string()),
            created_at: Set(now),
            completed_at: Set(None),
            duration_ms: Set(0),
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Complete an audit with results.
    pub async fn complete_audit(
        &self,
        audit_id: &str,
        result: &crate::lighthouse::AuditResult,
    ) -> Result<(), AuditStoreError> {
        let now = chrono::Utc::now().to_rfc3339();

        let metrics_json = result
            .metrics
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default());
        let diagnostics_json =
            serde_json::to_string(&result.diagnostics).unwrap_or_else(|_| "[]".to_string());

        let model = audit::Entity::find_by_id(audit_id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .ok_or_else(|| AuditStoreError::NotFound {
                id: audit_id.to_string(),
            })?;

        let mut active: audit::ActiveModel = model.into();
        active.status = Set("completed".to_string());
        active.performance_score = Set(result.performance_score.map(|s| s as i32));
        active.accessibility_score = Set(result.accessibility_score.map(|s| s as i32));
        active.best_practices_score = Set(result.best_practices_score.map(|s| s as i32));
        active.seo_score = Set(result.seo_score.map(|s| s as i32));
        active.metrics_json = Set(metrics_json);
        active.diagnostics_json = Set(diagnostics_json);
        active.has_raw_json = Set(true);
        active.completed_at = Set(Some(now));
        active.duration_ms = Set(result.duration_ms as i64);

        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        // Store raw JSON separately (can be large)
        raw_json::ActiveModel {
            audit_id: Set(audit_id.to_string()),
            json_data: Set(result.raw_json.clone()),
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Mark an audit as failed.
    pub async fn mark_failed(
        &self,
        audit_id: &str,
        error_message: &str,
    ) -> Result<(), AuditStoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        let model = audit::Entity::find_by_id(audit_id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        if let Some(m) = model {
            let mut active: audit::ActiveModel = m.into();
            active.status = Set("failed".to_string());
            active.error_message = Set(Some(error_message.to_string()));
            active.completed_at = Set(Some(now));
            active
                .update(self.db.as_ref())
                .await
                .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        }

        Ok(())
    }

    /// List all audits (summary view), newest first.
    pub async fn list_audits(&self) -> Result<Vec<AuditSummary>, AuditStoreError> {
        let audits = audit::Entity::find()
            .order_by_desc(audit::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(audits.into_iter().map(audit_model_to_summary).collect())
    }

    /// Get a full audit with details.
    pub async fn get_audit(&self, id: &str) -> Result<Option<LighthouseAudit>, AuditStoreError> {
        let model = audit::Entity::find_by_id(id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(model.map(audit_model_to_full))
    }

    /// Delete an audit (cascade deletes raw JSON).
    pub async fn delete_audit(&self, id: &str) -> Result<bool, AuditStoreError> {
        let result = audit::Entity::delete_by_id(id.to_string())
            .exec(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(result.rows_affected > 0)
    }

    /// Get raw Lighthouse JSON for an audit.
    pub async fn get_raw_json(&self, audit_id: &str) -> Result<Option<String>, AuditStoreError> {
        let model = raw_json::Entity::find_by_id(audit_id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.json_data))
    }

    /// Get score history for charts (newest first, limited).
    pub async fn get_score_history(
        &self,
        limit: u64,
    ) -> Result<Vec<ScoreHistoryPoint>, AuditStoreError> {
        let audits = audit::Entity::find()
            .filter(audit::Column::Status.eq("completed"))
            .order_by_desc(audit::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(audits
            .into_iter()
            .take(limit as usize)
            .map(|a| ScoreHistoryPoint {
                id: a.id,
                performance_score: a.performance_score.map(|s| s as u32),
                accessibility_score: a.accessibility_score.map(|s| s as u32),
                best_practices_score: a.best_practices_score.map(|s| s as u32),
                seo_score: a.seo_score.map(|s| s as u32),
                created_at: a.created_at,
                trigger: parse_trigger(&a.trigger),
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    async fn get_setting(&self, key: &str) -> Result<Option<String>, AuditStoreError> {
        let model = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.value))
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<(), AuditStoreError> {
        let existing = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        match existing {
            Some(m) => {
                let mut active: setting::ActiveModel = m.into();
                active.value = Set(value.to_string());
                active
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| AuditStoreError::Database(e.to_string()))?;
            }
            None => {
                setting::ActiveModel {
                    key: Set(key.to_string()),
                    value: Set(value.to_string()),
                }
                .insert(self.db.as_ref())
                .await
                .map_err(|e| AuditStoreError::Database(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Get the full plugin settings (with defaults for unset values).
    pub async fn get_settings(&self) -> Result<PluginSettings, AuditStoreError> {
        let auto_audit = self
            .get_setting("auto_audit_on_deploy")
            .await?
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(PluginSettings::DEFAULT_AUTO_AUDIT);

        let categories = self
            .get_setting("categories")
            .await?
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_else(|| {
                PluginSettings::DEFAULT_CATEGORIES
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            });

        let score_threshold = self
            .get_setting("score_threshold")
            .await?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(PluginSettings::DEFAULT_SCORE_THRESHOLD);

        let timeout_secs = self
            .get_setting("timeout_secs")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(PluginSettings::DEFAULT_TIMEOUT_SECS);

        let chrome_flags = self
            .get_setting("chrome_flags")
            .await?
            .unwrap_or_else(|| PluginSettings::DEFAULT_CHROME_FLAGS.to_string());

        let device = self
            .get_setting("device")
            .await?
            .unwrap_or_else(|| PluginSettings::DEFAULT_DEVICE.to_string());

        Ok(PluginSettings {
            auto_audit_on_deploy: auto_audit,
            categories,
            score_threshold,
            timeout_secs,
            chrome_flags,
            device,
        })
    }

    /// Update plugin settings. Only provided (Some) fields are written.
    pub async fn update_settings(
        &self,
        update: &UpdateSettings,
    ) -> Result<PluginSettings, AuditStoreError> {
        if let Some(v) = update.auto_audit_on_deploy {
            self.set_setting("auto_audit_on_deploy", &v.to_string())
                .await?;
        }
        if let Some(ref v) = update.categories {
            let json = serde_json::to_string(v).unwrap_or_default();
            self.set_setting("categories", &json).await?;
        }
        if let Some(v) = update.score_threshold {
            self.set_setting("score_threshold", &v.to_string()).await?;
        }
        if let Some(v) = update.timeout_secs {
            self.set_setting("timeout_secs", &v.to_string()).await?;
        }
        if let Some(ref v) = update.chrome_flags {
            self.set_setting("chrome_flags", v).await?;
        }
        if let Some(ref v) = update.device {
            self.set_setting("device", v).await?;
        }

        self.get_settings().await
    }
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum AuditStoreError {
    #[error("Failed to connect to SQLite at {path}: {reason}")]
    Connect { path: String, reason: String },

    #[error("Migration failed: {0}")]
    Migration(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Audit {id} not found")]
    NotFound { id: String },
}

// ============================================================================
// Conversions
// ============================================================================

fn audit_model_to_summary(a: audit::Model) -> AuditSummary {
    AuditSummary {
        id: a.id,
        url: a.url,
        performance_score: a.performance_score.map(|s| s as u32),
        accessibility_score: a.accessibility_score.map(|s| s as u32),
        best_practices_score: a.best_practices_score.map(|s| s as u32),
        seo_score: a.seo_score.map(|s| s as u32),
        status: parse_status(&a.status),
        trigger: parse_trigger(&a.trigger),
        project_id: a.project_id,
        deployment_id: a.deployment_id,
        device: a.device,
        created_at: a.created_at,
        duration_ms: a.duration_ms as u64,
    }
}

fn audit_model_to_full(a: audit::Model) -> LighthouseAudit {
    let metrics: Option<CoreWebVitals> = a
        .metrics_json
        .as_ref()
        .and_then(|j| serde_json::from_str(j).ok());

    let diagnostics: Vec<AuditDiagnostic> =
        serde_json::from_str(&a.diagnostics_json).unwrap_or_default();

    LighthouseAudit {
        id: a.id,
        url: a.url,
        performance_score: a.performance_score.map(|s| s as u32),
        accessibility_score: a.accessibility_score.map(|s| s as u32),
        best_practices_score: a.best_practices_score.map(|s| s as u32),
        seo_score: a.seo_score.map(|s| s as u32),
        status: parse_status(&a.status),
        trigger: parse_trigger(&a.trigger),
        project_id: a.project_id,
        deployment_id: a.deployment_id,
        metrics,
        diagnostics,
        raw_json_available: a.has_raw_json,
        error_message: a.error_message,
        created_at: a.created_at,
        completed_at: a.completed_at,
        duration_ms: a.duration_ms as u64,
        device: a.device,
    }
}

fn parse_status(s: &str) -> AuditStatus {
    match s {
        "completed" => AuditStatus::Completed,
        "failed" => AuditStatus::Failed,
        _ => AuditStatus::Running,
    }
}

fn parse_trigger(s: &str) -> AuditTrigger {
    match s {
        "deployment" => AuditTrigger::Deployment,
        _ => AuditTrigger::Manual,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (AuditStore, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let store = AuditStore::open(dir.path()).await.expect("open store");
        (store, dir)
    }

    #[tokio::test]
    async fn test_create_and_list_audit() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "a1",
                "https://example.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();
        let audits = store.list_audits().await.unwrap();

        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].id, "a1");
        assert_eq!(audits[0].url, "https://example.com");
        assert!(matches!(audits[0].status, AuditStatus::Running));
        assert!(matches!(audits[0].trigger, AuditTrigger::Manual));
    }

    #[tokio::test]
    async fn test_create_deployment_audit() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "a2",
                "https://app.example.com",
                &AuditTrigger::Deployment,
                Some(42),
                Some(100),
                "desktop",
            )
            .await
            .unwrap();

        let audit = store.get_audit("a2").await.unwrap().unwrap();
        assert_eq!(audit.project_id, Some(42));
        assert_eq!(audit.deployment_id, Some(100));
        assert!(matches!(audit.trigger, AuditTrigger::Deployment));
        assert_eq!(audit.device, "desktop");
    }

    #[tokio::test]
    async fn test_get_audit_not_found() {
        let (store, _dir) = test_store().await;
        let result = store.get_audit("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_complete_audit() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "a3",
                "https://test.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();

        let result = crate::lighthouse::AuditResult {
            performance_score: Some(85),
            accessibility_score: Some(92),
            best_practices_score: Some(100),
            seo_score: Some(78),
            metrics: Some(CoreWebVitals {
                lcp_ms: Some(2500.0),
                fcp_ms: Some(1200.0),
                tbt_ms: Some(250.0),
                cls: Some(0.05),
                speed_index_ms: Some(3000.0),
                tti_ms: Some(4000.0),
            }),
            diagnostics: vec![AuditDiagnostic {
                id: "render-blocking-resources".to_string(),
                title: "Eliminate render-blocking resources".to_string(),
                score: Some(0.3),
                savings: Some("Potential savings of 1.2 s".to_string()),
                severity: DiagnosticSeverity::Critical,
            }],
            raw_json: r#"{"test": true}"#.to_string(),
            duration_ms: 5000,
            device: "mobile".to_string(),
        };

        store.complete_audit("a3", &result).await.unwrap();

        let audit = store.get_audit("a3").await.unwrap().unwrap();
        assert!(matches!(audit.status, AuditStatus::Completed));
        assert_eq!(audit.performance_score, Some(85));
        assert_eq!(audit.accessibility_score, Some(92));
        assert_eq!(audit.diagnostics.len(), 1);
        assert!(audit.raw_json_available);
        assert!(audit.completed_at.is_some());

        let cwv = audit.metrics.unwrap();
        assert_eq!(cwv.lcp_ms, Some(2500.0));

        // Check raw JSON
        let raw = store.get_raw_json("a3").await.unwrap().unwrap();
        assert!(raw.contains("test"));
    }

    #[tokio::test]
    async fn test_mark_failed() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "a4",
                "https://fail.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();
        store
            .mark_failed("a4", "Lighthouse CLI not found")
            .await
            .unwrap();

        let audit = store.get_audit("a4").await.unwrap().unwrap();
        assert!(matches!(audit.status, AuditStatus::Failed));
        assert_eq!(
            audit.error_message,
            Some("Lighthouse CLI not found".to_string())
        );
    }

    #[tokio::test]
    async fn test_delete_audit() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "a5",
                "https://del.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();

        assert!(store.delete_audit("a5").await.unwrap());
        assert!(!store.delete_audit("a5").await.unwrap());
        assert!(store.list_audits().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_score_history() {
        let (store, _dir) = test_store().await;

        // Create and complete two audits
        store
            .create_audit(
                "h1",
                "https://a.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();
        let result = crate::lighthouse::AuditResult {
            performance_score: Some(70),
            accessibility_score: Some(80),
            best_practices_score: Some(90),
            seo_score: Some(85),
            metrics: None,
            diagnostics: vec![],
            raw_json: "{}".to_string(),
            duration_ms: 1000,
            device: "mobile".to_string(),
        };
        store.complete_audit("h1", &result).await.unwrap();

        let history = store.get_score_history(10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].performance_score, Some(70));
    }

    #[tokio::test]
    async fn test_settings_defaults() {
        let (store, _dir) = test_store().await;
        let settings = store.get_settings().await.unwrap();

        assert!(settings.auto_audit_on_deploy);
        assert_eq!(settings.categories.len(), 4);
        assert_eq!(settings.score_threshold, 80);
        assert_eq!(settings.device, "mobile");
    }

    #[tokio::test]
    async fn test_settings_update_partial() {
        let (store, _dir) = test_store().await;

        let updated = store
            .update_settings(&UpdateSettings {
                score_threshold: Some(90),
                device: Some("desktop".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(updated.score_threshold, 90);
        assert_eq!(updated.device, "desktop");
        assert!(updated.auto_audit_on_deploy); // unchanged
    }

    #[tokio::test]
    async fn test_audits_ordered_newest_first() {
        let (store, _dir) = test_store().await;

        store
            .create_audit(
                "old",
                "https://old.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store
            .create_audit(
                "new",
                "https://new.com",
                &AuditTrigger::Manual,
                None,
                None,
                "mobile",
            )
            .await
            .unwrap();

        let audits = store.list_audits().await.unwrap();
        assert_eq!(audits[0].id, "new");
        assert_eq!(audits[1].id, "old");
    }
}
