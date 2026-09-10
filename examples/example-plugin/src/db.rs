// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SQLite-backed persistence for SEO reports.
//!
//! Uses Sea-ORM with a local SQLite file in the plugin's data directory.
//! Tables are created on first use via raw DDL (no migration framework needed
//! for a plugin this simple).

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

pub mod report {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "seo_reports")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub url: String,
        pub score: i32,
        pub status: String,
        pub pages_crawled: i32,
        pub total_issues: i32,
        pub critical: i32,
        pub warnings: i32,
        pub info: i32,
        pub avg_page_score: i32,
        pub missing_titles: i32,
        pub missing_descriptions: i32,
        pub missing_h1: i32,
        pub images_without_alt: i32,
        pub missing_canonical: i32,
        pub missing_og_tags: i32,
        pub created_at: String,
        pub completed_at: Option<String>,
        pub duration_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::page::Entity")]
        Pages,
    }

    impl Related<super::page::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Pages.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod setting {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "seo_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod page {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "seo_pages")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub report_id: String,
        pub url: String,
        pub status_code: i32,
        pub score: i32,
        pub title: Option<String>,
        pub meta_description: Option<String>,
        pub canonical: Option<String>,
        pub h1_count: i32,
        pub h2_count: i32,
        pub image_count: i32,
        pub images_without_alt: i32,
        pub word_count: i32,
        pub internal_links: i32,
        pub external_links: i32,
        pub has_og_title: bool,
        pub has_og_description: bool,
        pub has_og_image: bool,
        pub has_robots_meta: bool,
        pub has_viewport: bool,
        pub has_charset: bool,
        pub has_lang: bool,
        pub load_time_ms: i64,
        /// JSON-serialized Vec<SeoIssue>
        pub issues_json: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::report::Entity",
            from = "Column::ReportId",
            to = "super::report::Column::Id"
        )]
        Report,
    }

    impl Related<super::report::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Report.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

// ============================================================================
// Store
// ============================================================================

/// SQLite-backed store for SEO reports.
#[derive(Clone)]
pub struct SeoStore {
    db: Arc<DatabaseConnection>,
}

impl SeoStore {
    /// Open (or create) the SQLite database in the given data directory.
    pub async fn open(data_dir: &Path) -> Result<Self, SeoStoreError> {
        let db_path = data_dir.join("seo.db");
        let url = format!("sqlite://{}?mode=rwc", db_path.display());

        let mut opts = ConnectOptions::new(&url);
        opts.max_connections(1) // SQLite is single-writer
            .sqlx_logging(false);

        let db = Database::connect(opts)
            .await
            .map_err(|e| SeoStoreError::Connect {
                path: db_path.display().to_string(),
                reason: e.to_string(),
            })?;

        // Run migrations
        Self::migrate(&db).await?;

        tracing::info!(path = %db_path.display(), "SEO store opened");

        Ok(Self { db: Arc::new(db) })
    }

    /// Create tables if they don't exist.
    async fn migrate(db: &DatabaseConnection) -> Result<(), SeoStoreError> {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS seo_reports (
                id               TEXT PRIMARY KEY NOT NULL,
                url              TEXT NOT NULL,
                score            INTEGER NOT NULL DEFAULT 0,
                status           TEXT NOT NULL DEFAULT 'running',
                pages_crawled    INTEGER NOT NULL DEFAULT 0,
                total_issues     INTEGER NOT NULL DEFAULT 0,
                critical         INTEGER NOT NULL DEFAULT 0,
                warnings         INTEGER NOT NULL DEFAULT 0,
                info             INTEGER NOT NULL DEFAULT 0,
                avg_page_score   INTEGER NOT NULL DEFAULT 0,
                missing_titles   INTEGER NOT NULL DEFAULT 0,
                missing_descriptions INTEGER NOT NULL DEFAULT 0,
                missing_h1       INTEGER NOT NULL DEFAULT 0,
                images_without_alt INTEGER NOT NULL DEFAULT 0,
                missing_canonical INTEGER NOT NULL DEFAULT 0,
                missing_og_tags  INTEGER NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                completed_at     TEXT,
                duration_ms      INTEGER NOT NULL DEFAULT 0
            );
            "#,
        ))
        .await
        .map_err(|e| SeoStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS seo_pages (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                report_id           TEXT NOT NULL REFERENCES seo_reports(id) ON DELETE CASCADE,
                url                 TEXT NOT NULL,
                status_code         INTEGER NOT NULL,
                score               INTEGER NOT NULL DEFAULT 0,
                title               TEXT,
                meta_description    TEXT,
                canonical           TEXT,
                h1_count            INTEGER NOT NULL DEFAULT 0,
                h2_count            INTEGER NOT NULL DEFAULT 0,
                image_count         INTEGER NOT NULL DEFAULT 0,
                images_without_alt  INTEGER NOT NULL DEFAULT 0,
                word_count          INTEGER NOT NULL DEFAULT 0,
                internal_links      INTEGER NOT NULL DEFAULT 0,
                external_links      INTEGER NOT NULL DEFAULT 0,
                has_og_title        BOOLEAN NOT NULL DEFAULT 0,
                has_og_description  BOOLEAN NOT NULL DEFAULT 0,
                has_og_image        BOOLEAN NOT NULL DEFAULT 0,
                has_robots_meta     BOOLEAN NOT NULL DEFAULT 0,
                has_viewport        BOOLEAN NOT NULL DEFAULT 0,
                has_charset         BOOLEAN NOT NULL DEFAULT 0,
                has_lang            BOOLEAN NOT NULL DEFAULT 0,
                load_time_ms        INTEGER NOT NULL DEFAULT 0,
                issues_json         TEXT NOT NULL DEFAULT '[]'
            );
            "#,
        ))
        .await
        .map_err(|e| SeoStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "CREATE INDEX IF NOT EXISTS idx_seo_pages_report_id ON seo_pages(report_id);",
        ))
        .await
        .map_err(|e| SeoStoreError::Migration(e.to_string()))?;

        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS seo_settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        ))
        .await
        .map_err(|e| SeoStoreError::Migration(e.to_string()))?;

        // Enable foreign keys (SQLite has them off by default)
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA foreign_keys = ON;",
        ))
        .await
        .map_err(|e| SeoStoreError::Migration(e.to_string()))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Report CRUD
    // -----------------------------------------------------------------------

    /// Insert a new report in "running" status.
    pub async fn create_report(&self, id: &str, url: &str) -> Result<(), SeoStoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        report::ActiveModel {
            id: Set(id.to_string()),
            url: Set(url.to_string()),
            score: Set(0),
            status: Set("running".to_string()),
            pages_crawled: Set(0),
            total_issues: Set(0),
            critical: Set(0),
            warnings: Set(0),
            info: Set(0),
            avg_page_score: Set(0),
            missing_titles: Set(0),
            missing_descriptions: Set(0),
            missing_h1: Set(0),
            images_without_alt: Set(0),
            missing_canonical: Set(0),
            missing_og_tags: Set(0),
            created_at: Set(now),
            completed_at: Set(None),
            duration_ms: Set(0),
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// List all reports (summary view), newest first.
    pub async fn list_reports(&self) -> Result<Vec<ReportSummary>, SeoStoreError> {
        let reports = report::Entity::find()
            .order_by_desc(report::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        Ok(reports.into_iter().map(report_model_to_summary).collect())
    }

    /// Get a full report with all pages.
    pub async fn get_report(&self, id: &str) -> Result<Option<SeoReport>, SeoStoreError> {
        let report_model = report::Entity::find_by_id(id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        let Some(r) = report_model else {
            return Ok(None);
        };

        let page_models = page::Entity::find()
            .filter(page::Column::ReportId.eq(id))
            .all(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        let pages: Vec<PageAnalysis> = page_models
            .into_iter()
            .map(page_model_to_analysis)
            .collect();
        let summary = summary_from_report_model(&r);

        Ok(Some(SeoReport {
            id: r.id,
            url: r.url,
            score: r.score as u32,
            pages,
            summary,
            status: parse_status(&r.status),
            created_at: r.created_at,
            completed_at: r.completed_at,
            duration_ms: r.duration_ms as u64,
        }))
    }

    /// Delete a report (cascade deletes its pages).
    /// Returns true if a report was actually deleted.
    pub async fn delete_report(&self, id: &str) -> Result<bool, SeoStoreError> {
        let result = report::Entity::delete_by_id(id.to_string())
            .exec(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        Ok(result.rows_affected > 0)
    }

    /// Complete an analysis: insert pages & update report to "completed".
    pub async fn complete_report(
        &self,
        report_id: &str,
        pages: &[PageAnalysis],
        duration_ms: u64,
    ) -> Result<(), SeoStoreError> {
        let summary = crate::crawl::compute_summary(pages);
        let overall_score = if pages.is_empty() {
            0
        } else {
            (pages.iter().map(|p| p.score as u64).sum::<u64>() / pages.len() as u64) as u32
        };

        // Insert all pages
        for p in pages {
            let issues_json = serde_json::to_string(&p.issues).unwrap_or_else(|_| "[]".to_string());

            page::ActiveModel {
                id: Default::default(), // autoincrement
                report_id: Set(report_id.to_string()),
                url: Set(p.url.clone()),
                status_code: Set(p.status_code as i32),
                score: Set(p.score as i32),
                title: Set(p.title.clone()),
                meta_description: Set(p.meta_description.clone()),
                canonical: Set(p.canonical.clone()),
                h1_count: Set(p.h1_count as i32),
                h2_count: Set(p.h2_count as i32),
                image_count: Set(p.image_count as i32),
                images_without_alt: Set(p.images_without_alt as i32),
                word_count: Set(p.word_count as i32),
                internal_links: Set(p.internal_links as i32),
                external_links: Set(p.external_links as i32),
                has_og_title: Set(p.has_og_title),
                has_og_description: Set(p.has_og_description),
                has_og_image: Set(p.has_og_image),
                has_robots_meta: Set(p.has_robots_meta),
                has_viewport: Set(p.has_viewport),
                has_charset: Set(p.has_charset),
                has_lang: Set(p.has_lang),
                load_time_ms: Set(p.load_time_ms as i64),
                issues_json: Set(issues_json),
            }
            .insert(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;
        }

        // Update report
        let now = chrono::Utc::now().to_rfc3339();
        let mut active: report::ActiveModel = report::Entity::find_by_id(report_id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?
            .ok_or_else(|| SeoStoreError::NotFound {
                id: report_id.to_string(),
            })?
            .into();

        active.status = Set("completed".to_string());
        active.score = Set(overall_score as i32);
        active.pages_crawled = Set(summary.pages_crawled as i32);
        active.total_issues = Set(summary.total_issues as i32);
        active.critical = Set(summary.critical as i32);
        active.warnings = Set(summary.warnings as i32);
        active.info = Set(summary.info as i32);
        active.avg_page_score = Set(summary.avg_page_score as i32);
        active.missing_titles = Set(summary.missing_titles as i32);
        active.missing_descriptions = Set(summary.missing_descriptions as i32);
        active.missing_h1 = Set(summary.missing_h1 as i32);
        active.images_without_alt = Set(summary.images_without_alt as i32);
        active.missing_canonical = Set(summary.missing_canonical as i32);
        active.missing_og_tags = Set(summary.missing_og_tags as i32);
        active.completed_at = Set(Some(now));
        active.duration_ms = Set(duration_ms as i64);

        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        Ok(())
    }

    /// Mark a report as failed.
    pub async fn mark_failed(&self, report_id: &str) -> Result<(), SeoStoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        let model = report::Entity::find_by_id(report_id.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        if let Some(m) = model {
            let mut active: report::ActiveModel = m.into();
            active.status = Set("failed".to_string());
            active.completed_at = Set(Some(now));
            active
                .update(self.db.as_ref())
                .await
                .map_err(|e| SeoStoreError::Database(e.to_string()))?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------

    /// Get a setting value by key.
    async fn get_setting(&self, key: &str) -> Result<Option<String>, SeoStoreError> {
        let model = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        Ok(model.map(|m| m.value))
    }

    /// Set a setting value (upsert).
    async fn set_setting(&self, key: &str, value: &str) -> Result<(), SeoStoreError> {
        let existing = setting::Entity::find_by_id(key.to_string())
            .one(self.db.as_ref())
            .await
            .map_err(|e| SeoStoreError::Database(e.to_string()))?;

        match existing {
            Some(m) => {
                let mut active: setting::ActiveModel = m.into();
                active.value = Set(value.to_string());
                active
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| SeoStoreError::Database(e.to_string()))?;
            }
            None => {
                setting::ActiveModel {
                    key: Set(key.to_string()),
                    value: Set(value.to_string()),
                }
                .insert(self.db.as_ref())
                .await
                .map_err(|e| SeoStoreError::Database(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Get the full plugin settings (with defaults for unset values).
    pub async fn get_settings(&self) -> Result<PluginSettings, SeoStoreError> {
        let default_max_pages = self
            .get_setting("default_max_pages")
            .await?
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(PluginSettings::DEFAULT_MAX_PAGES);

        let user_agent = self
            .get_setting("user_agent")
            .await?
            .unwrap_or_else(|| PluginSettings::DEFAULT_USER_AGENT.to_string());

        let request_timeout_secs = self
            .get_setting("request_timeout_secs")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(PluginSettings::DEFAULT_REQUEST_TIMEOUT_SECS);

        let crawl_delay_ms = self
            .get_setting("crawl_delay_ms")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(PluginSettings::DEFAULT_CRAWL_DELAY_MS);

        Ok(PluginSettings {
            default_max_pages,
            user_agent,
            request_timeout_secs,
            crawl_delay_ms,
        })
    }

    /// Update plugin settings. Only provided (Some) fields are written.
    pub async fn update_settings(
        &self,
        update: &UpdateSettings,
    ) -> Result<PluginSettings, SeoStoreError> {
        if let Some(v) = update.default_max_pages {
            self.set_setting("default_max_pages", &v.to_string())
                .await?;
        }
        if let Some(ref v) = update.user_agent {
            self.set_setting("user_agent", v).await?;
        }
        if let Some(v) = update.request_timeout_secs {
            self.set_setting("request_timeout_secs", &v.to_string())
                .await?;
        }
        if let Some(v) = update.crawl_delay_ms {
            self.set_setting("crawl_delay_ms", &v.to_string()).await?;
        }

        self.get_settings().await
    }
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum SeoStoreError {
    #[error("Failed to connect to SQLite at {path}: {reason}")]
    Connect { path: String, reason: String },

    #[error("Migration failed: {0}")]
    Migration(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Report {id} not found")]
    NotFound { id: String },
}

// ============================================================================
// Conversions: Model → API types
// ============================================================================

fn report_model_to_summary(r: report::Model) -> ReportSummary {
    ReportSummary {
        id: r.id,
        url: r.url,
        score: r.score as u32,
        pages_crawled: r.pages_crawled as usize,
        critical_issues: r.critical as usize,
        warning_issues: r.warnings as usize,
        info_issues: r.info as usize,
        status: parse_status(&r.status),
        created_at: r.created_at,
        duration_ms: r.duration_ms as u64,
    }
}

fn summary_from_report_model(r: &report::Model) -> ReportSummaryStats {
    ReportSummaryStats {
        pages_crawled: r.pages_crawled as usize,
        total_issues: r.total_issues as usize,
        critical: r.critical as usize,
        warnings: r.warnings as usize,
        info: r.info as usize,
        avg_page_score: r.avg_page_score as u32,
        missing_titles: r.missing_titles as usize,
        missing_descriptions: r.missing_descriptions as usize,
        missing_h1: r.missing_h1 as usize,
        images_without_alt: r.images_without_alt as usize,
        missing_canonical: r.missing_canonical as usize,
        missing_og_tags: r.missing_og_tags as usize,
    }
}

fn page_model_to_analysis(p: page::Model) -> PageAnalysis {
    let issues: Vec<SeoIssue> = serde_json::from_str(&p.issues_json).unwrap_or_default();

    PageAnalysis {
        url: p.url,
        status_code: p.status_code as u16,
        score: p.score as u32,
        title: p.title,
        meta_description: p.meta_description,
        canonical: p.canonical,
        h1_count: p.h1_count as usize,
        h2_count: p.h2_count as usize,
        image_count: p.image_count as usize,
        images_without_alt: p.images_without_alt as usize,
        word_count: p.word_count as usize,
        internal_links: p.internal_links as usize,
        external_links: p.external_links as usize,
        has_og_title: p.has_og_title,
        has_og_description: p.has_og_description,
        has_og_image: p.has_og_image,
        has_robots_meta: p.has_robots_meta,
        has_viewport: p.has_viewport,
        has_charset: p.has_charset,
        has_lang: p.has_lang,
        load_time_ms: p.load_time_ms as u64,
        issues,
    }
}

fn parse_status(s: &str) -> ReportStatus {
    match s {
        "completed" => ReportStatus::Completed,
        "failed" => ReportStatus::Failed,
        _ => ReportStatus::Running,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_store() -> (SeoStore, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let store = SeoStore::open(dir.path()).await.expect("open store");
        (store, dir)
    }

    #[tokio::test]
    async fn test_create_and_list_report() {
        let (store, _dir) = test_store().await;

        store
            .create_report("r1", "https://example.com")
            .await
            .unwrap();
        let reports = store.list_reports().await.unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, "r1");
        assert_eq!(reports[0].url, "https://example.com");
        assert!(matches!(reports[0].status, ReportStatus::Running));
    }

    #[tokio::test]
    async fn test_get_report_not_found() {
        let (store, _dir) = test_store().await;
        let result = store.get_report("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_complete_report() {
        let (store, _dir) = test_store().await;
        store.create_report("r2", "https://test.com").await.unwrap();

        let pages = vec![PageAnalysis {
            url: "https://test.com/".to_string(),
            status_code: 200,
            score: 85,
            title: Some("Test Page".to_string()),
            meta_description: Some("A test page".to_string()),
            canonical: Some("https://test.com/".to_string()),
            h1_count: 1,
            h2_count: 2,
            image_count: 3,
            images_without_alt: 0,
            word_count: 500,
            internal_links: 10,
            external_links: 2,
            has_og_title: true,
            has_og_description: true,
            has_og_image: true,
            has_robots_meta: true,
            has_viewport: true,
            has_charset: true,
            has_lang: true,
            load_time_ms: 150,
            issues: vec![],
        }];

        store.complete_report("r2", &pages, 1234).await.unwrap();

        let report = store.get_report("r2").await.unwrap().unwrap();
        assert!(matches!(report.status, ReportStatus::Completed));
        assert_eq!(report.score, 85);
        assert_eq!(report.pages.len(), 1);
        assert_eq!(report.pages[0].title, Some("Test Page".to_string()));
        assert_eq!(report.duration_ms, 1234);
        assert!(report.completed_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_report() {
        let (store, _dir) = test_store().await;
        store.create_report("r3", "https://del.com").await.unwrap();

        assert!(store.delete_report("r3").await.unwrap());
        assert!(!store.delete_report("r3").await.unwrap()); // already gone
        assert!(store.list_reports().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_mark_failed() {
        let (store, _dir) = test_store().await;
        store.create_report("r4", "https://fail.com").await.unwrap();
        store.mark_failed("r4").await.unwrap();

        let report = store.get_report("r4").await.unwrap().unwrap();
        assert!(matches!(report.status, ReportStatus::Failed));
        assert!(report.completed_at.is_some());
    }

    #[tokio::test]
    async fn test_cascade_delete_pages() {
        let (store, _dir) = test_store().await;
        store
            .create_report("r5", "https://cascade.com")
            .await
            .unwrap();

        let pages = vec![PageAnalysis {
            url: "https://cascade.com/a".to_string(),
            status_code: 200,
            score: 90,
            title: Some("Page A".to_string()),
            meta_description: None,
            canonical: None,
            h1_count: 1,
            h2_count: 0,
            image_count: 0,
            images_without_alt: 0,
            word_count: 100,
            internal_links: 0,
            external_links: 0,
            has_og_title: false,
            has_og_description: false,
            has_og_image: false,
            has_robots_meta: false,
            has_viewport: true,
            has_charset: true,
            has_lang: true,
            load_time_ms: 50,
            issues: vec![SeoIssue {
                severity: IssueSeverity::Warning,
                code: "TEST".to_string(),
                message: "test issue".to_string(),
                recommendation: "fix it".to_string(),
            }],
        }];

        store.complete_report("r5", &pages, 500).await.unwrap();

        // Verify page exists
        let report = store.get_report("r5").await.unwrap().unwrap();
        assert_eq!(report.pages.len(), 1);
        assert_eq!(report.pages[0].issues.len(), 1);

        // Delete should cascade
        assert!(store.delete_report("r5").await.unwrap());
        assert!(store.get_report("r5").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_reports_ordered_newest_first() {
        let (store, _dir) = test_store().await;

        store.create_report("old", "https://old.com").await.unwrap();
        // Small delay to ensure different timestamps
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store.create_report("new", "https://new.com").await.unwrap();

        let reports = store.list_reports().await.unwrap();
        assert_eq!(reports[0].id, "new");
        assert_eq!(reports[1].id, "old");
    }

    #[tokio::test]
    async fn test_settings_defaults() {
        let (store, _dir) = test_store().await;
        let settings = store.get_settings().await.unwrap();

        assert_eq!(
            settings.default_max_pages,
            PluginSettings::DEFAULT_MAX_PAGES
        );
        assert_eq!(settings.user_agent, PluginSettings::DEFAULT_USER_AGENT);
        assert_eq!(
            settings.request_timeout_secs,
            PluginSettings::DEFAULT_REQUEST_TIMEOUT_SECS
        );
        assert_eq!(
            settings.crawl_delay_ms,
            PluginSettings::DEFAULT_CRAWL_DELAY_MS
        );
    }

    #[tokio::test]
    async fn test_settings_update_partial() {
        let (store, _dir) = test_store().await;

        let updated = store
            .update_settings(&UpdateSettings {
                default_max_pages: Some(200),
                user_agent: None,
                request_timeout_secs: None,
                crawl_delay_ms: Some(500),
            })
            .await
            .unwrap();

        assert_eq!(updated.default_max_pages, 200);
        assert_eq!(updated.user_agent, PluginSettings::DEFAULT_USER_AGENT);
        assert_eq!(updated.crawl_delay_ms, 500);
    }

    #[tokio::test]
    async fn test_settings_update_overwrites() {
        let (store, _dir) = test_store().await;

        store
            .update_settings(&UpdateSettings {
                default_max_pages: Some(100),
                user_agent: None,
                request_timeout_secs: None,
                crawl_delay_ms: None,
            })
            .await
            .unwrap();

        // Overwrite again
        let updated = store
            .update_settings(&UpdateSettings {
                default_max_pages: Some(300),
                user_agent: Some("MyBot/2.0".to_string()),
                request_timeout_secs: None,
                crawl_delay_ms: None,
            })
            .await
            .unwrap();

        assert_eq!(updated.default_max_pages, 300);
        assert_eq!(updated.user_agent, "MyBot/2.0");
    }
}
