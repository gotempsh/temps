// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reads and changes a project's visitor identity settings: the identity
//! mode (`projects.analytics_identity_mode`, ADR 109 D3) and whether browser
//! identify is accepted (`project_analytics_settings.accept_browser_identify`,
//! ADR 109 D4).
//!
//! The project route-change trigger fires on the mode column, so a change
//! reaches the proxy (which stops or starts setting cookies) and the
//! Host-resolved ingest path on the next route reload. The keyed ingest path
//! caches the mode for a minute; this process drops that entry on change.
//! Identify reads both values fresh on every call, so nothing is cached for
//! the browser identify setting.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseConnection, EntityTrait,
    QueryFilter, Set, TransactionTrait,
};
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use temps_entities::{project_analytics_settings, projects};

use super::types::{
    AnalyticsIdentityError, AnalyticsIdentitySettings, UpdateAnalyticsIdentitySettings,
};
use crate::cookieless::CookielessIdentity;

pub struct AnalyticsIdentityService {
    db: Arc<DatabaseConnection>,
    cookieless: Arc<CookielessIdentity>,
}

/// The settings before and after a change, for the audit entries.
pub struct AnalyticsIdentityChange {
    pub previous_mode: AnalyticsIdentityMode,
    pub previous_accept_browser_identify: bool,
    pub settings: AnalyticsIdentitySettings,
}

impl AnalyticsIdentityService {
    pub fn new(db: Arc<DatabaseConnection>, cookieless: Arc<CookielessIdentity>) -> Self {
        Self { db, cookieless }
    }

    fn db_error(
        project_id: i32,
        operation: &'static str,
    ) -> impl FnOnce(sea_orm::DbErr) -> AnalyticsIdentityError {
        move |source| AnalyticsIdentityError::Database {
            project_id,
            operation,
            source,
        }
    }

    async fn project(&self, project_id: i32) -> Result<projects::Model, AnalyticsIdentityError> {
        projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(Self::db_error(project_id, "load project identity settings"))?
            .ok_or(AnalyticsIdentityError::ProjectNotFound { project_id })
    }

    /// `true` for a project without a settings row.
    async fn accept_browser_identify(
        &self,
        project_id: i32,
    ) -> Result<bool, AnalyticsIdentityError> {
        Ok(project_analytics_settings::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(Self::db_error(
                project_id,
                "load the browser identify setting",
            ))?
            .is_none_or(|row| row.accept_browser_identify))
    }

    pub async fn get(
        &self,
        project_id: i32,
    ) -> Result<AnalyticsIdentitySettings, AnalyticsIdentityError> {
        let project = self.project(project_id).await?;
        let accept = self.accept_browser_identify(project_id).await?;
        Ok(AnalyticsIdentitySettings::new(
            project.id,
            project.analytics_identity_mode,
            accept,
            project.updated_at,
        ))
    }

    pub async fn set(
        &self,
        project_id: i32,
        patch: &UpdateAnalyticsIdentitySettings,
    ) -> Result<AnalyticsIdentityChange, AnalyticsIdentityError> {
        if patch.mode.is_none() && patch.accept_browser_identify.is_none() {
            return Err(AnalyticsIdentityError::Validation {
                project_id,
                message: "set `mode`, `accept_browser_identify`, or both".to_string(),
            });
        }
        let project = self.project(project_id).await?;
        let previous_mode = project.analytics_identity_mode;
        let previous_accept = self.accept_browser_identify(project_id).await?;
        let mut mode = previous_mode;
        let mut updated_at = project.updated_at;

        // Both settings are written together or not at all: a failure on the
        // second must not leave the first changed without its audit entry.
        let txn = self.db.begin().await.map_err(Self::db_error(
            project_id,
            "start the identity settings update",
        ))?;
        let mut mode_changed = false;
        if let Some(requested) = patch.mode.filter(|requested| *requested != previous_mode) {
            // Only the two changed columns are written, so a concurrent edit
            // of any other project setting is not overwritten.
            let mut active: projects::ActiveModel = project.into();
            active.analytics_identity_mode = Set(requested);
            active.updated_at = Set(Utc::now());
            let updated = active
                .update(&txn)
                .await
                .map_err(Self::db_error(project_id, "update project identity mode"))?;
            mode_changed = true;
            mode = updated.analytics_identity_mode;
            updated_at = updated.updated_at;
        }

        let mut accept = previous_accept;
        if let Some(requested) = patch
            .accept_browser_identify
            .filter(|requested| *requested != previous_accept)
        {
            // The traffic settings share this row; only this column (and
            // `updated_at`) is written on conflict, so they are left alone.
            let now = Utc::now();
            let row = project_analytics_settings::ActiveModel {
                project_id: Set(project_id),
                exclude_datacenter_traffic: NotSet,
                exclude_spam_referrers: NotSet,
                accept_browser_identify: Set(requested),
                created_at: Set(now),
                updated_at: Set(now),
            };
            let saved = project_analytics_settings::Entity::insert(row)
                .on_conflict(
                    OnConflict::column(project_analytics_settings::Column::ProjectId)
                        .update_columns([
                            project_analytics_settings::Column::AcceptBrowserIdentify,
                            project_analytics_settings::Column::UpdatedAt,
                        ])
                        .to_owned(),
                )
                .exec_with_returning(&txn)
                .await
                .map_err(Self::db_error(
                    project_id,
                    "save the browser identify setting",
                ))?;
            accept = saved.accept_browser_identify;
            updated_at = updated_at.max(saved.updated_at);
        }
        txn.commit().await.map_err(Self::db_error(
            project_id,
            "commit the identity settings update",
        ))?;
        if mode_changed {
            self.cookieless.forget_identity_mode(project_id).await;
        }

        Ok(AnalyticsIdentityChange {
            previous_mode,
            previous_accept_browser_identify: previous_accept,
            settings: AnalyticsIdentitySettings::new(project_id, mode, accept, updated_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use temps_core::EncryptionService;

    fn service(db: MockDatabase) -> AnalyticsIdentityService {
        let db = Arc::new(db.into_connection());
        let encryption = Arc::new(
            EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("valid test key"),
        );
        AnalyticsIdentityService::new(
            db.clone(),
            Arc::new(CookielessIdentity::new(db, encryption)),
        )
    }

    fn project(mode: AnalyticsIdentityMode) -> projects::Model {
        let mut project = crate::ingest_keys::test_fixtures::project_model(5);
        project.analytics_identity_mode = mode;
        project
    }

    fn settings_row(accept: bool) -> project_analytics_settings::Model {
        let now = Utc::now();
        project_analytics_settings::Model {
            project_id: 5,
            exclude_datacenter_traffic: true,
            exclude_spam_referrers: true,
            accept_browser_identify: accept,
            created_at: now,
            updated_at: now,
        }
    }

    fn no_settings() -> Vec<project_analytics_settings::Model> {
        Vec::new()
    }

    fn patch(
        mode: Option<AnalyticsIdentityMode>,
        accept: Option<bool>,
    ) -> UpdateAnalyticsIdentitySettings {
        UpdateAnalyticsIdentitySettings {
            mode,
            accept_browser_identify: accept,
        }
    }

    #[tokio::test]
    async fn get_reports_the_stored_mode_and_the_browser_identify_default() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Cookieless)]])
            .append_query_results([no_settings()]);
        let settings = service(db).get(5).await.expect("settings");
        assert_eq!(settings.project_id, 5);
        assert_eq!(settings.mode, AnalyticsIdentityMode::Cookieless);
        assert!(settings.accept_browser_identify, "accepted by default");
        assert!(!settings.limitations.is_empty());
    }

    #[tokio::test]
    async fn a_missing_project_is_not_found_with_its_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<projects::Model>::new()]);
        let error = service(db).get(42).await.expect_err("no such project");
        assert!(matches!(
            error,
            AnalyticsIdentityError::ProjectNotFound { project_id: 42 }
        ));
    }

    #[tokio::test]
    async fn an_empty_patch_is_refused_before_any_read() {
        let db = MockDatabase::new(DatabaseBackend::Postgres);
        let error = service(db)
            .set(5, &patch(None, None))
            .await
            .err()
            .expect("nothing to change");
        assert!(matches!(
            error,
            AnalyticsIdentityError::Validation { project_id: 5, .. }
        ));
    }

    #[tokio::test]
    async fn setting_the_same_values_writes_nothing() {
        // Only the reads are mocked: a write would find no result and fail.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Persistent)]])
            .append_query_results([vec![settings_row(true)]]);
        let change = service(db)
            .set(
                5,
                &patch(Some(AnalyticsIdentityMode::Persistent), Some(true)),
            )
            .await
            .expect("no-op change");
        assert_eq!(change.previous_mode, AnalyticsIdentityMode::Persistent);
        assert_eq!(change.settings.mode, AnalyticsIdentityMode::Persistent);
        assert!(change.settings.accept_browser_identify);
    }

    #[tokio::test]
    async fn switching_modes_returns_the_previous_and_the_new_mode() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Persistent)]])
            .append_query_results([no_settings()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![project(AnalyticsIdentityMode::Cookieless)]]);
        let change = service(db)
            .set(5, &patch(Some(AnalyticsIdentityMode::Cookieless), None))
            .await
            .expect("switched");
        assert_eq!(change.previous_mode, AnalyticsIdentityMode::Persistent);
        assert_eq!(change.settings.mode, AnalyticsIdentityMode::Cookieless);
        assert_eq!(
            change.settings.sdk_option.as_deref(),
            Some("cookieless: true")
        );
    }

    #[tokio::test]
    async fn refusing_browser_identify_saves_only_that_column() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Persistent)]])
            .append_query_results([no_settings()])
            .append_query_results([vec![settings_row(false)]]);
        let change = service(db)
            .set(5, &patch(None, Some(false)))
            .await
            .expect("saved");
        assert!(change.previous_accept_browser_identify);
        assert!(!change.settings.accept_browser_identify);
        assert_eq!(change.settings.mode, AnalyticsIdentityMode::Persistent);
    }

    #[tokio::test]
    async fn a_database_failure_names_the_project_and_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())]);
        let error = service(db).get(9).await.expect_err("db down");
        let message = error.to_string();
        assert!(message.contains("project 9"), "{message}");
        assert!(
            message.contains("load project identity settings"),
            "{message}"
        );
    }

    /// A failure saving the browser setting leaves the identity mode as it
    /// was: the two settings are written in one transaction.
    #[tokio::test]
    async fn a_failed_browser_setting_save_leaves_the_mode_unchanged() {
        let test_db = match crate::testing::TestDatabase::new("identity_settings_atomic").await {
            Ok(test_db) => test_db,
            Err(error) => {
                println!("Docker not available ({error}), skipping");
                return;
            }
        };
        use sea_orm::ConnectionTrait;
        let db = test_db.db.clone();
        crate::testing::test_utils::AnalyticsTestUtils::insert_test_data(&db)
            .await
            .expect("test data");
        let project = projects::Entity::find()
            .one(db.as_ref())
            .await
            .expect("load project")
            .expect("a project");
        assert_eq!(
            project.analytics_identity_mode,
            AnalyticsIdentityMode::Persistent
        );
        db.execute_unprepared(
            "ALTER TABLE project_analytics_settings \
             ADD CONSTRAINT reject_every_row CHECK (false) NOT VALID",
        )
        .await
        .expect("make the settings write fail");

        let encryption = Arc::new(
            EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect("valid test key"),
        );
        let service = AnalyticsIdentityService::new(
            db.clone(),
            Arc::new(CookielessIdentity::new(db.clone(), encryption)),
        );
        let result = service
            .set(
                project.id,
                &patch(Some(AnalyticsIdentityMode::Cookieless), Some(false)),
            )
            .await;
        assert!(
            matches!(result, Err(AnalyticsIdentityError::Database { .. })),
            "the failed save is reported"
        );
        let after = projects::Entity::find_by_id(project.id)
            .one(db.as_ref())
            .await
            .expect("reload project")
            .expect("the project");
        assert_eq!(
            after.analytics_identity_mode,
            AnalyticsIdentityMode::Persistent,
            "the mode change was rolled back with the failed save"
        );
    }
}
