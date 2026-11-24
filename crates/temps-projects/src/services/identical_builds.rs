// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a project's builds coordinate with identical builds (ADR 114):
//! `projects.identical_build_mode` and `projects.identical_build_scope`, and
//! what coordination did for the project lately.
//!
//! Kept apart from `ProjectService` so its routes depend only on what they
//! touch. The values are typed enums, so nothing outside the allowed set can
//! reach the database; the columns' CHECK constraints refuse it too.

use std::sync::Arc;

use http::StatusCode;
use sea_orm::{
    sea_query::Expr, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter,
    QuerySelect, Statement, TransactionTrait,
};
use temps_core::problemdetails::Problem;
use temps_entities::identical_build_mode::{IdenticalBuildMode, IdenticalBuildScope};
use temps_entities::projects;

#[derive(Debug, thiserror::Error)]
pub enum IdenticalBuildSettingsError {
    #[error("Project {project_id} was not found while accessing its identical build settings")]
    NotFound { project_id: i32 },
    #[error("Failed to {operation} identical build settings for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl From<IdenticalBuildSettingsError> for Problem {
    fn from(error: IdenticalBuildSettingsError) -> Self {
        let status = match &error {
            IdenticalBuildSettingsError::NotFound { .. } => StatusCode::NOT_FOUND,
            IdenticalBuildSettingsError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        temps_core::problemdetails::new(status)
            .with_title("Identical build settings")
            .with_detail(error.to_string())
    }
}

fn db_error(
    project_id: i32,
    operation: &'static str,
) -> impl FnOnce(sea_orm::DbErr) -> IdenticalBuildSettingsError {
    move |source| IdenticalBuildSettingsError::Database {
        project_id,
        operation,
        source,
    }
}

/// A project's identical build setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdenticalBuildSetting {
    pub mode: IdenticalBuildMode,
    pub scope: IdenticalBuildScope,
}

/// The saved value before and after an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdenticalBuildSettingChange {
    pub previous: IdenticalBuildSetting,
    pub current: IdenticalBuildSetting,
}

/// What coordination did for a project's builds in the last 30 days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IdenticalBuildActivity {
    /// Deployments that deployed another build's image instead of building.
    pub reused: u64,
    /// Coordinated builds that built their own image (succeeded or failed).
    pub built: u64,
    /// Builds waiting for their group right now.
    pub waiting: u64,
    /// Builds of this project building for their group right now.
    pub building: u64,
}

pub struct IdenticalBuildSettingsService {
    db: Arc<temps_database::DbConnection>,
}

impl IdenticalBuildSettingsService {
    pub fn new(db: Arc<temps_database::DbConnection>) -> Self {
        Self { db }
    }

    /// The project's setting.
    pub async fn setting(
        &self,
        project_id: i32,
    ) -> Result<IdenticalBuildSetting, IdenticalBuildSettingsError> {
        let (mode, scope) = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::IdenticalBuildMode)
            .column(projects::Column::IdenticalBuildScope)
            .into_tuple::<(IdenticalBuildMode, IdenticalBuildScope)>()
            .one(self.db.as_ref())
            .await
            .map_err(db_error(project_id, "read"))?
            .ok_or(IdenticalBuildSettingsError::NotFound { project_id })?;
        Ok(IdenticalBuildSetting { mode, scope })
    }

    /// Counts of the project's coordinated builds in the last 30 days, by
    /// outcome. One aggregate over an indexed range of a small table.
    pub async fn activity(
        &self,
        project_id: i32,
    ) -> Result<IdenticalBuildActivity, IdenticalBuildSettingsError> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT state, count(*) AS builds FROM identical_builds \
                 WHERE project_id = $1 AND created_at > now() - interval '30 days' \
                 GROUP BY state",
                [project_id.into()],
            ))
            .await
            .map_err(db_error(project_id, "read the recent activity of"))?;
        let mut activity = IdenticalBuildActivity::default();
        for row in rows {
            let read = || -> Result<(String, i64), sea_orm::DbErr> {
                Ok((row.try_get("", "state")?, row.try_get("", "builds")?))
            };
            let (state, builds) =
                read().map_err(db_error(project_id, "read the recent activity of"))?;
            let builds = u64::try_from(builds).unwrap_or(0);
            match state.as_str() {
                "reused" => activity.reused += builds,
                "succeeded" | "failed" => activity.built += builds,
                "waiting" => activity.waiting += builds,
                "building" => activity.building += builds,
                _ => {}
            }
        }
        Ok(activity)
    }

    /// Replace the project's setting, under the row lock so the previous
    /// value the audit log records is the one actually replaced.
    pub async fn update(
        &self,
        project_id: i32,
        requested: IdenticalBuildSetting,
    ) -> Result<IdenticalBuildSettingChange, IdenticalBuildSettingsError> {
        let tx = self
            .db
            .begin()
            .await
            .map_err(db_error(project_id, "begin update of"))?;
        let (mode, scope) = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::IdenticalBuildMode)
            .column(projects::Column::IdenticalBuildScope)
            .lock_exclusive()
            .into_tuple::<(IdenticalBuildMode, IdenticalBuildScope)>()
            .one(&tx)
            .await
            .map_err(db_error(project_id, "lock project for"))?
            .ok_or(IdenticalBuildSettingsError::NotFound { project_id })?;
        let previous = IdenticalBuildSetting { mode, scope };
        if previous != requested {
            projects::Entity::update_many()
                .col_expr(
                    projects::Column::IdenticalBuildMode,
                    Expr::value(requested.mode.as_str()),
                )
                .col_expr(
                    projects::Column::IdenticalBuildScope,
                    Expr::value(requested.scope.as_str()),
                )
                .col_expr(projects::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
                .filter(projects::Column::Id.eq(project_id))
                .exec(&tx)
                .await
                .map_err(db_error(project_id, "save"))?;
        }
        tx.commit().await.map_err(db_error(project_id, "commit"))?;
        Ok(IdenticalBuildSettingChange {
            previous,
            current: requested,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn stored(mode: &str, scope: &str) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("identical_build_mode", Value::from(mode.to_string())),
            ("identical_build_scope", Value::from(scope.to_string())),
        ])
    }

    fn service(db: MockDatabase) -> IdenticalBuildSettingsService {
        IdenticalBuildSettingsService::new(Arc::new(db.into_connection()))
    }

    const SAVE: &str = r#"SET \"identical_build_mode\""#;

    fn share() -> IdenticalBuildSetting {
        IdenticalBuildSetting {
            mode: IdenticalBuildMode::Share,
            scope: IdenticalBuildScope::Fingerprint,
        }
    }

    #[tokio::test]
    async fn reads_the_stored_setting() {
        let service = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[stored("queue", "source")]]),
        );
        assert_eq!(
            service.setting(7).await.unwrap(),
            IdenticalBuildSetting {
                mode: IdenticalBuildMode::Queue,
                scope: IdenticalBuildScope::Source,
            }
        );
    }

    #[tokio::test]
    async fn a_missing_project_is_not_found_with_its_id() {
        let service = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
                .append_query_results([Vec::<BTreeMap<&str, Value>>::new()]),
        );
        let error = service.setting(9).await.unwrap_err();
        assert!(matches!(
            error,
            IdenticalBuildSettingsError::NotFound { project_id: 9 }
        ));
        assert_eq!(Problem::from(error).status_code, StatusCode::NOT_FOUND);
        assert!(matches!(
            service.update(9, share()).await,
            Err(IdenticalBuildSettingsError::NotFound { project_id: 9 })
        ));
    }

    #[tokio::test]
    async fn a_database_failure_names_the_project_and_operation() {
        let service = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())]),
        );
        let error = service.setting(7).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("read identical build settings for project 7"),
            "{error}"
        );
        assert_eq!(
            Problem::from(error).status_code,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn a_change_is_saved_under_the_row_lock() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored("parallel", "fingerprint")]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = IdenticalBuildSettingsService::new(Arc::new(db));
        let change = service.update(7, share()).await.unwrap();
        assert_eq!(change.previous.mode, IdenticalBuildMode::Parallel);
        assert_eq!(change.current, share());
        let db = Arc::try_unwrap(service.db).expect("sole owner");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("FOR UPDATE"), "{log}");
        assert!(log.contains(SAVE), "{log}");
        assert!(log.contains("COMMIT"), "{log}");
    }

    #[tokio::test]
    async fn an_unchanged_value_writes_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored("share", "fingerprint")]])
            .into_connection();
        let service = IdenticalBuildSettingsService::new(Arc::new(db));
        let change = service.update(7, share()).await.unwrap();
        assert_eq!(change.previous, change.current);
        let db = Arc::try_unwrap(service.db).expect("sole owner");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains(SAVE), "{log}");
    }

    #[tokio::test]
    async fn activity_counts_outcomes() {
        let row = |state: &str, builds: i64| {
            BTreeMap::from([
                ("state", Value::from(state.to_string())),
                ("builds", Value::from(builds)),
            ])
        };
        let service = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![
                row("reused", 4),
                row("succeeded", 2),
                row("failed", 1),
                row("waiting", 1),
                row("cancelled", 3),
            ]]),
        );
        assert_eq!(
            service.activity(7).await.unwrap(),
            IdenticalBuildActivity {
                reused: 4,
                built: 3,
                waiting: 1,
                building: 0,
            }
        );
    }
}
