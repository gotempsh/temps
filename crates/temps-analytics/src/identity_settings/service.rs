// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reads and changes `projects.analytics_identity_mode`.
//!
//! The project route-change trigger fires on this column, so a change reaches
//! the proxy (which stops or starts setting cookies) and the Host-resolved
//! ingest path on the next route reload. The keyed ingest path caches the
//! mode for a minute; this process drops that entry on change.

use std::sync::Arc;

use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use temps_entities::projects;

use super::types::{AnalyticsIdentityError, AnalyticsIdentitySettings};
use crate::cookieless::CookielessIdentity;

pub struct AnalyticsIdentityService {
    db: Arc<DatabaseConnection>,
    cookieless: Arc<CookielessIdentity>,
}

/// The settings before and after a change, for the audit entry.
pub struct AnalyticsIdentityChange {
    pub previous: AnalyticsIdentityMode,
    pub settings: AnalyticsIdentitySettings,
}

impl AnalyticsIdentityService {
    pub fn new(db: Arc<DatabaseConnection>, cookieless: Arc<CookielessIdentity>) -> Self {
        Self { db, cookieless }
    }

    async fn project(&self, project_id: i32) -> Result<projects::Model, AnalyticsIdentityError> {
        projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(|source| AnalyticsIdentityError::Database {
                project_id,
                operation: "load project identity settings",
                source,
            })?
            .ok_or(AnalyticsIdentityError::ProjectNotFound { project_id })
    }

    pub async fn get(
        &self,
        project_id: i32,
    ) -> Result<AnalyticsIdentitySettings, AnalyticsIdentityError> {
        let project = self.project(project_id).await?;
        Ok(AnalyticsIdentitySettings::new(
            project.id,
            project.analytics_identity_mode,
            project.updated_at,
        ))
    }

    pub async fn set(
        &self,
        project_id: i32,
        mode: AnalyticsIdentityMode,
    ) -> Result<AnalyticsIdentityChange, AnalyticsIdentityError> {
        let project = self.project(project_id).await?;
        let previous = project.analytics_identity_mode;
        if previous == mode {
            return Ok(AnalyticsIdentityChange {
                previous,
                settings: AnalyticsIdentitySettings::new(project.id, mode, project.updated_at),
            });
        }
        // Only the two changed columns are written, so a concurrent edit of
        // any other project setting is not overwritten.
        let mut active: projects::ActiveModel = project.into();
        active.analytics_identity_mode = Set(mode);
        active.updated_at = Set(Utc::now());
        let updated =
            active
                .update(self.db.as_ref())
                .await
                .map_err(|source| AnalyticsIdentityError::Database {
                    project_id,
                    operation: "update project identity mode",
                    source,
                })?;
        self.cookieless.forget_identity_mode(project_id).await;
        Ok(AnalyticsIdentityChange {
            previous,
            settings: AnalyticsIdentitySettings::new(
                updated.id,
                updated.analytics_identity_mode,
                updated.updated_at,
            ),
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

    #[tokio::test]
    async fn get_reports_the_stored_mode() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Cookieless)]]);
        let settings = service(db).get(5).await.expect("settings");
        assert_eq!(settings.project_id, 5);
        assert_eq!(settings.mode, AnalyticsIdentityMode::Cookieless);
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
    async fn setting_the_same_mode_writes_nothing() {
        // Only the read is mocked: an UPDATE would find no result and fail.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Persistent)]]);
        let change = service(db)
            .set(5, AnalyticsIdentityMode::Persistent)
            .await
            .expect("no-op change");
        assert_eq!(change.previous, AnalyticsIdentityMode::Persistent);
        assert_eq!(change.settings.mode, AnalyticsIdentityMode::Persistent);
    }

    #[tokio::test]
    async fn switching_modes_returns_the_previous_and_the_new_mode() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project(AnalyticsIdentityMode::Persistent)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![project(AnalyticsIdentityMode::Cookieless)]]);
        let change = service(db)
            .set(5, AnalyticsIdentityMode::Cookieless)
            .await
            .expect("switched");
        assert_eq!(change.previous, AnalyticsIdentityMode::Persistent);
        assert_eq!(change.settings.mode, AnalyticsIdentityMode::Cookieless);
        assert_eq!(
            change.settings.sdk_option.as_deref(),
            Some("cookieless: true")
        );
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
}
