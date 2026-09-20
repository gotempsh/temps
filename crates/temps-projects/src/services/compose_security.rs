// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use http::StatusCode;
use sea_orm::{
    sea_query::OnConflict, ActiveValue::Set, EntityTrait, QuerySelect, TransactionTrait,
};
use temps_core::problemdetails::Problem;
use temps_entities::{
    compose_security::ComposeSecurityPolicy, compose_security_policies,
    compose_security_policy_changes, projects,
};

#[derive(Debug, thiserror::Error)]
pub enum ComposeSecurityError {
    #[error("Project {project_id} was not found while accessing Compose security settings")]
    NotFound { project_id: i32 },
    #[error("Project {project_id} does not use Docker Compose")]
    NotCompose { project_id: i32 },
    #[error("Explicit risk acknowledgement is required to disable Compose security checks for project {project_id}")]
    AcknowledgementRequired { project_id: i32 },
    #[error("Failed to {operation} Compose security settings for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}
impl From<ComposeSecurityError> for Problem {
    fn from(error: ComposeSecurityError) -> Self {
        let status = match &error {
            ComposeSecurityError::NotFound { .. } => StatusCode::NOT_FOUND,
            ComposeSecurityError::NotCompose { .. }
            | ComposeSecurityError::AcknowledgementRequired { .. } => StatusCode::BAD_REQUEST,
            ComposeSecurityError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        temps_core::problemdetails::new(status)
            .with_title("Compose security settings")
            .with_detail(error.to_string())
    }
}

fn db_error(
    project_id: i32,
    operation: &'static str,
    source: sea_orm::DbErr,
) -> ComposeSecurityError {
    ComposeSecurityError::Database {
        project_id,
        operation,
        source,
    }
}

fn require_compose(
    project_id: i32,
    project: Option<(i32, temps_entities::preset::Preset)>,
) -> Result<(), ComposeSecurityError> {
    let project = project.ok_or(ComposeSecurityError::NotFound { project_id })?;
    if project.1 != temps_entities::preset::Preset::DockerCompose {
        return Err(ComposeSecurityError::NotCompose { project_id });
    }
    Ok(())
}

pub async fn read(
    db: &temps_database::DbConnection,
    project_id: i32,
) -> Result<ComposeSecurityPolicy, ComposeSecurityError> {
    require_compose(
        project_id,
        projects::Entity::find_by_id(project_id)
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::Preset)
            .into_tuple::<(i32, temps_entities::preset::Preset)>()
            .one(db)
            .await
            .map_err(|e| db_error(project_id, "read project for", e))?,
    )?;
    Ok(compose_security_policies::Entity::find_by_id(project_id)
        .one(db)
        .await
        .map_err(|e| db_error(project_id, "read", e))?
        .map(|row| row.policy)
        .unwrap_or_default())
}

pub async fn update(
    db: &temps_database::DbConnection,
    project_id: i32,
    actor: i32,
    policy: ComposeSecurityPolicy,
    acknowledged: bool,
) -> Result<ComposeSecurityPolicy, ComposeSecurityError> {
    let tx = db
        .begin()
        .await
        .map_err(|e| db_error(project_id, "begin update of", e))?;
    require_compose(
        project_id,
        projects::Entity::find_by_id(project_id)
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::Preset)
            .lock_exclusive()
            .into_tuple::<(i32, temps_entities::preset::Preset)>()
            .one(&tx)
            .await
            .map_err(|e| db_error(project_id, "lock project for", e))?,
    )?;
    let previous = compose_security_policies::Entity::find_by_id(project_id)
        .one(&tx)
        .await
        .map_err(|e| db_error(project_id, "read previous", e))?
        .map(|row| row.policy)
        .unwrap_or_default();
    if policy
        .disabled_checks
        .difference(&previous.disabled_checks)
        .next()
        .is_some()
        && !acknowledged
    {
        return Err(ComposeSecurityError::AcknowledgementRequired { project_id });
    }
    compose_security_policies::Entity::insert(compose_security_policies::ActiveModel {
        project_id: Set(project_id),
        policy: Set(policy.clone()),
        accepted_by: Set(actor),
        updated_at: Set(chrono::Utc::now()),
    })
    .on_conflict(
        OnConflict::column(compose_security_policies::Column::ProjectId)
            .update_columns([
                compose_security_policies::Column::Policy,
                compose_security_policies::Column::AcceptedBy,
                compose_security_policies::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(&tx)
    .await
    .map_err(|e| db_error(project_id, "save", e))?;
    compose_security_policy_changes::Entity::insert(compose_security_policy_changes::ActiveModel {
        project_id: Set(project_id),
        previous: Set(previous.clone()),
        policy: Set(policy),
        accepted_by: Set(actor),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    })
    .exec_without_returning(&tx)
    .await
    .map_err(|e| db_error(project_id, "record grant history for", e))?;
    tx.commit()
        .await
        .map_err(|e| db_error(project_id, "commit", e))?;
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use temps_entities::compose_security::ComposeSecurityCheck;

    fn project(preset: &str) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("id", 7.into()), ("preset", preset.into())])
    }
    fn granted() -> ComposeSecurityPolicy {
        ComposeSecurityPolicy {
            disabled_checks: [ComposeSecurityCheck::Extends].into(),
        }
    }
    fn row(policy: ComposeSecurityPolicy) -> compose_security_policies::Model {
        compose_security_policies::Model {
            project_id: 7,
            policy,
            accepted_by: 1,
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn missing_policy_defaults_to_all_checks_enabled() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([Vec::<compose_security_policies::Model>::new()])
            .into_connection();
        assert_eq!(
            read(&db, 7).await.unwrap(),
            ComposeSecurityPolicy::default()
        );
    }

    #[tokio::test]
    async fn read_returns_saved_exceptions() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([[row(granted())]])
            .into_connection();
        assert_eq!(read(&db, 7).await.unwrap(), granted());
    }

    #[tokio::test]
    async fn missing_project_and_wrong_preset_are_distinct() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
            .into_connection();
        assert!(matches!(
            read(&db, 7).await,
            Err(ComposeSecurityError::NotFound { project_id: 7 })
        ));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("dockerfile")]])
            .into_connection();
        assert!(matches!(
            read(&db, 7).await,
            Err(ComposeSecurityError::NotCompose { project_id: 7 })
        ));
    }

    #[tokio::test]
    async fn database_failure_never_defaults_to_an_empty_policy() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_errors([sea_orm::DbErr::Custom("policy read failed".into())])
            .into_connection();
        assert!(matches!(
            read(&db, 7).await,
            Err(ComposeSecurityError::Database {
                project_id: 7,
                operation: "read",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn new_exception_requires_acknowledgement_and_rolls_back() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([Vec::<compose_security_policies::Model>::new()])
            .into_connection();
        assert!(matches!(
            update(&db, 7, 1, granted(), false).await,
            Err(ComposeSecurityError::AcknowledgementRequired { project_id: 7 })
        ));
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("ROLLBACK"));
        assert!(!log.contains("INSERT"));
    }

    #[tokio::test]
    async fn accepted_update_persists_policy_and_history_in_one_transaction() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([Vec::<compose_security_policies::Model>::new()])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 7,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        assert_eq!(
            update(&db, 7, 1, granted(), true).await.unwrap(),
            ComposeSecurityPolicy::default()
        );
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("compose_security_policies"));
        assert!(log.contains("compose_security_policy_changes"));
        assert!(log.contains("COMMIT"));
    }

    #[tokio::test]
    async fn history_failure_rolls_back_the_grant() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([Vec::<compose_security_policies::Model>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 7,
                rows_affected: 1,
            }])
            .append_exec_errors([sea_orm::DbErr::Custom("history unavailable".into())])
            .into_connection();
        assert!(matches!(
            update(&db, 7, 1, granted(), true).await,
            Err(ComposeSecurityError::Database {
                operation: "record grant history for",
                ..
            })
        ));
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("ROLLBACK"));
        assert!(!log.contains("COMMIT"));
    }

    #[tokio::test]
    async fn restoring_checks_does_not_require_risk_acceptance() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[project("docker-compose")]])
            .append_query_results([[row(granted())]])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 7,
                    rows_affected: 1,
                },
                MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
            ])
            .into_connection();
        assert_eq!(
            update(&db, 7, 1, ComposeSecurityPolicy::default(), false)
                .await
                .unwrap(),
            granted()
        );
    }
}
