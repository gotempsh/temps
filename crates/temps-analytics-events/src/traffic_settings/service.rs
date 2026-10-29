// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project analytics traffic settings.
//!
//! Read on the ingest path (to decide whether a datacenter IP or a known spam
//! referrer is counted) and written rarely from the console, so reads are served from a small
//! in-memory cache with a short TTL. A write invalidates this instance's entry
//! immediately; any other process (split topology) picks the change up within
//! [`SETTINGS_CACHE_TTL`].

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use moka::future::Cache;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection, DbErr, EntityTrait, QuerySelect};
use temps_entities::{project_analytics_settings, projects};
use thiserror::Error;
use tracing::warn;

/// Whether datacenter traffic is excluded for a project that never changed
/// the setting. On by default: datacenter IPs are almost never real visitors.
pub const DEFAULT_EXCLUDE_DATACENTER_TRAFFIC: bool = true;

/// Whether referrers on the bundled spam list are excluded for a project that
/// never changed the setting. On by default: the list only names domains whose
/// sole purpose is planting fake referrals.
pub const DEFAULT_EXCLUDE_SPAM_REFERRERS: bool = true;

/// How long a project's settings stay cached for the ingest path. Bounds how
/// long another process keeps applying a setting after it was changed.
const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(30);

/// Upper bound on cached projects. Each entry is a few bytes; this only
/// prevents an unbounded map on an instance with a very large project count.
const SETTINGS_CACHE_MAX_ENTRIES: u64 = 10_000;

/// Effective analytics traffic settings for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAnalyticsSettings {
    pub project_id: i32,
    pub exclude_datacenter_traffic: bool,
    pub exclude_spam_referrers: bool,
    /// When the settings were last changed. `None` while the project still
    /// uses the defaults (no row has ever been written).
    pub updated_at: Option<DateTime<Utc>>,
}

impl ProjectAnalyticsSettings {
    fn defaults(project_id: i32) -> Self {
        Self {
            project_id,
            exclude_datacenter_traffic: DEFAULT_EXCLUDE_DATACENTER_TRAFFIC,
            exclude_spam_referrers: DEFAULT_EXCLUDE_SPAM_REFERRERS,
            updated_at: None,
        }
    }
}

impl From<project_analytics_settings::Model> for ProjectAnalyticsSettings {
    fn from(model: project_analytics_settings::Model) -> Self {
        Self {
            project_id: model.project_id,
            exclude_datacenter_traffic: model.exclude_datacenter_traffic,
            exclude_spam_referrers: model.exclude_spam_referrers,
            updated_at: Some(model.updated_at),
        }
    }
}

/// Settings to change. Fields left `None` keep their current value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectAnalyticsSettingsPatch {
    pub exclude_datacenter_traffic: Option<bool>,
    pub exclude_spam_referrers: Option<bool>,
}

#[derive(Error, Debug)]
pub enum ProjectAnalyticsSettingsError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error(
        "Database error while {operation} analytics settings for project {project_id}: {source}"
    )]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

pub struct ProjectAnalyticsSettingsService {
    db: Arc<DatabaseConnection>,
    cache: Cache<i32, ProjectAnalyticsSettings>,
}

impl ProjectAnalyticsSettingsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let cache = Cache::builder()
            .max_capacity(SETTINGS_CACHE_MAX_ENTRIES)
            .time_to_live(SETTINGS_CACHE_TTL)
            .build();
        Self { db, cache }
    }

    /// The project's settings, for the console. Verifies the project exists
    /// when it has no settings row, so an unknown project is a `404` rather
    /// than a set of plausible-looking defaults.
    pub async fn get(
        &self,
        project_id: i32,
    ) -> Result<ProjectAnalyticsSettings, ProjectAnalyticsSettingsError> {
        if let Some(row) = self.find_row(project_id, "loading").await? {
            return Ok(row.into());
        }
        self.ensure_project_exists(project_id).await?;
        Ok(ProjectAnalyticsSettings::defaults(project_id))
    }

    /// Whether events from datacenter IPs should be excluded for `project_id`.
    /// Ingest path; see [`Self::for_ingest`].
    pub async fn exclude_datacenter_traffic(&self, project_id: i32) -> bool {
        self.for_ingest(project_id).await.exclude_datacenter_traffic
    }

    /// Whether referrers on the bundled spam list should be excluded for
    /// `project_id`. Ingest path; see [`Self::for_ingest`].
    pub async fn exclude_spam_referrers(&self, project_id: i32) -> bool {
        self.for_ingest(project_id).await.exclude_spam_referrers
    }

    /// The project's settings for the ingest path: served from the cache, and
    /// never fails. A database error degrades to the defaults (exclude) and is
    /// logged, because dropping the event or counting a scraper as a visitor
    /// would both be worse than applying the defaults for one cache period.
    async fn for_ingest(&self, project_id: i32) -> ProjectAnalyticsSettings {
        if let Some(cached) = self.cache.get(&project_id).await {
            return cached;
        }

        let settings = match self.find_row(project_id, "loading").await {
            Ok(Some(row)) => row.into(),
            Ok(None) => ProjectAnalyticsSettings::defaults(project_id),
            Err(e) => {
                warn!(
                    project_id,
                    error = %e,
                    "could not read analytics traffic settings; applying the defaults"
                );
                return ProjectAnalyticsSettings::defaults(project_id);
            }
        };
        self.cache.insert(project_id, settings.clone()).await;
        settings
    }

    /// Change the project's settings. Fields left `None` keep their current
    /// value. Returns the settings now in effect.
    pub async fn update(
        &self,
        project_id: i32,
        patch: ProjectAnalyticsSettingsPatch,
    ) -> Result<ProjectAnalyticsSettings, ProjectAnalyticsSettingsError> {
        let current = self.get(project_id).await?;
        let exclude_datacenter = patch
            .exclude_datacenter_traffic
            .unwrap_or(current.exclude_datacenter_traffic);
        let exclude_spam = patch
            .exclude_spam_referrers
            .unwrap_or(current.exclude_spam_referrers);

        let now = Utc::now();
        let row = project_analytics_settings::ActiveModel {
            project_id: Set(project_id),
            exclude_datacenter_traffic: Set(exclude_datacenter),
            exclude_spam_referrers: Set(exclude_spam),
            created_at: Set(now),
            updated_at: Set(now),
        };
        let saved = project_analytics_settings::Entity::insert(row)
            .on_conflict(
                OnConflict::column(project_analytics_settings::Column::ProjectId)
                    .update_columns([
                        project_analytics_settings::Column::ExcludeDatacenterTraffic,
                        project_analytics_settings::Column::ExcludeSpamReferrers,
                        project_analytics_settings::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_with_returning(self.db.as_ref())
            .await
            .map_err(|source| ProjectAnalyticsSettingsError::Database {
                project_id,
                operation: "saving",
                source,
            })?;

        self.cache.invalidate(&project_id).await;
        Ok(saved.into())
    }

    async fn find_row(
        &self,
        project_id: i32,
        operation: &'static str,
    ) -> Result<Option<project_analytics_settings::Model>, ProjectAnalyticsSettingsError> {
        project_analytics_settings::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ProjectAnalyticsSettingsError::Database {
                project_id,
                operation,
                source,
            })
    }

    async fn ensure_project_exists(
        &self,
        project_id: i32,
    ) -> Result<(), ProjectAnalyticsSettingsError> {
        let exists = project_exists(self.db.as_ref(), project_id)
            .await
            .map_err(|source| ProjectAnalyticsSettingsError::Database {
                project_id,
                operation: "checking the project for",
                source,
            })?;
        if exists {
            Ok(())
        } else {
            Err(ProjectAnalyticsSettingsError::ProjectNotFound { project_id })
        }
    }
}

/// Whether a project with `project_id` exists. Selects only the id, so it does
/// not depend on (or pay for) the rest of the project row.
pub(super) async fn project_exists(
    db: &DatabaseConnection,
    project_id: i32,
) -> Result<bool, DbErr> {
    let project: Option<i32> = projects::Entity::find_by_id(project_id)
        .select_only()
        .column(projects::Column::Id)
        .into_tuple()
        .one(db)
        .await?;
    Ok(project.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    /// A row of the `SELECT id FROM projects` existence check.
    fn project_id_row(id: i32) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("id", Value::Int(Some(id)))])
    }

    fn no_project() -> Vec<BTreeMap<&'static str, Value>> {
        Vec::new()
    }

    fn settings_row(project_id: i32, exclude: bool) -> project_analytics_settings::Model {
        let now = Utc::now();
        project_analytics_settings::Model {
            project_id,
            exclude_datacenter_traffic: exclude,
            exclude_spam_referrers: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn service(db: MockDatabase) -> ProjectAnalyticsSettingsService {
        ProjectAnalyticsSettingsService::new(Arc::new(db.into_connection()))
    }

    #[tokio::test]
    async fn a_project_without_a_row_excludes_datacenter_traffic_by_default() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_analytics_settings::Model>::new()]),
        );
        assert!(svc.exclude_datacenter_traffic(7).await);
    }

    #[tokio::test]
    async fn a_stored_opt_out_is_honoured() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![settings_row(7, false)]]),
        );
        assert!(!svc.exclude_datacenter_traffic(7).await);
    }

    #[tokio::test]
    async fn the_ingest_read_is_cached() {
        // Only one query result is queued: a second database hit would make
        // the mock return an error, which degrades to the default (`true`)
        // and fails the assertion.
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![settings_row(7, false)]]),
        );
        assert!(!svc.exclude_datacenter_traffic(7).await);
        assert!(!svc.exclude_datacenter_traffic(7).await);
    }

    #[tokio::test]
    async fn a_database_error_on_the_ingest_read_degrades_to_the_default() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection reset".to_string())]),
        );
        assert_eq!(
            svc.exclude_datacenter_traffic(7).await,
            DEFAULT_EXCLUDE_DATACENTER_TRAFFIC
        );
    }

    #[tokio::test]
    async fn get_returns_defaults_for_an_existing_project_without_a_row() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_analytics_settings::Model>::new()])
                .append_query_results([vec![project_id_row(7)]]),
        );
        let settings = svc.get(7).await.expect("defaults for an existing project");
        assert_eq!(settings, ProjectAnalyticsSettings::defaults(7));
    }

    #[tokio::test]
    async fn get_reports_an_unknown_project_as_not_found() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_analytics_settings::Model>::new()])
                .append_query_results([no_project()]),
        );
        let err = svc.get(404).await.expect_err("unknown project");
        assert!(matches!(
            err,
            ProjectAnalyticsSettingsError::ProjectNotFound { project_id: 404 }
        ));
    }

    #[tokio::test]
    async fn get_surfaces_database_errors_with_the_project_id() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("boom".to_string())]),
        );
        let err = svc.get(9).await.expect_err("database error");
        assert!(matches!(
            err,
            ProjectAnalyticsSettingsError::Database { project_id: 9, .. }
        ));
        assert!(err.to_string().contains("project 9"));
    }

    #[tokio::test]
    async fn update_saves_and_invalidates_the_ingest_cache() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                // Warm the ingest cache with the default.
                .append_query_results([Vec::<project_analytics_settings::Model>::new()])
                // update(): get() finds the row...
                .append_query_results([vec![settings_row(7, true)]])
                // ...the upsert returns the saved row...
                .append_query_results([vec![settings_row(7, false)]])
                // ...and the next ingest read reloads it.
                .append_query_results([vec![settings_row(7, false)]]),
        );
        assert!(svc.exclude_datacenter_traffic(7).await);

        let saved = svc
            .update(
                7,
                ProjectAnalyticsSettingsPatch {
                    exclude_datacenter_traffic: Some(false),
                    exclude_spam_referrers: None,
                },
            )
            .await
            .expect("update succeeds");
        assert!(
            saved.exclude_spam_referrers,
            "an omitted field keeps its value"
        );
        assert!(!saved.exclude_datacenter_traffic);
        assert!(saved.updated_at.is_some());

        assert!(
            !svc.exclude_datacenter_traffic(7).await,
            "the cached default must not outlive the write"
        );
    }

    #[tokio::test]
    async fn update_of_an_unknown_project_is_not_found() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_analytics_settings::Model>::new()])
                .append_query_results([no_project()]),
        );
        let err = svc
            .update(
                404,
                ProjectAnalyticsSettingsPatch {
                    exclude_datacenter_traffic: Some(false),
                    ..Default::default()
                },
            )
            .await
            .expect_err("unknown project");
        assert!(matches!(
            err,
            ProjectAnalyticsSettingsError::ProjectNotFound { project_id: 404 }
        ));
    }
}
