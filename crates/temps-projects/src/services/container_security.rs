// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Container security: the Linux capabilities a project's containers get
//! back after `cap_drop: ALL` (`projects.allowed_capabilities`).
//!
//! Every value goes through [`ContainerCapability::parse`], the closed
//! allow-list, so a request can never store `SYS_ADMIN`, `ALL` or
//! `privileged`; the column's CHECK constraint refuses them too. Adding a
//! capability needs an explicit acknowledgement, and the write is a
//! compare-and-set against the value the editor loaded, under a row lock,
//! so a stale tab cannot silently undo a newer change.

use std::collections::BTreeSet;
use std::sync::Arc;

use http::StatusCode;
use sea_orm::{
    sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use temps_core::problemdetails::Problem;
use temps_entities::projects;
use temps_workload::{CapabilityError, ContainerCapability};

#[derive(Debug, thiserror::Error)]
pub enum ContainerSecurityError {
    #[error("Project {project_id} was not found while accessing its container security settings")]
    NotFound { project_id: i32 },
    #[error("Container security settings for project {project_id} changed since they were loaded (now: {current}). Refresh and review them before retrying.")]
    Conflict { project_id: i32, current: String },
    #[error("Invalid container security settings for project {project_id}: {source}")]
    InvalidCapability {
        project_id: i32,
        #[source]
        source: CapabilityError,
    },
    #[error("Allowing {added} for project {project_id} weakens its container hardening; confirm it explicitly (acknowledge_risks) to save")]
    AcknowledgementRequired { project_id: i32, added: String },
    #[error(
        "Failed to {operation} container security settings for project {project_id}: {source}"
    )]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl From<ContainerSecurityError> for Problem {
    fn from(error: ContainerSecurityError) -> Self {
        let status = match &error {
            ContainerSecurityError::NotFound { .. } => StatusCode::NOT_FOUND,
            ContainerSecurityError::Conflict { .. } => StatusCode::CONFLICT,
            ContainerSecurityError::InvalidCapability { .. }
            | ContainerSecurityError::AcknowledgementRequired { .. } => StatusCode::BAD_REQUEST,
            ContainerSecurityError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        temps_core::problemdetails::new(status)
            .with_title("Container security settings")
            .with_detail(error.to_string())
    }
}

/// The saved value before and after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSecurityChange {
    pub previous: Vec<ContainerCapability>,
    pub current: Vec<ContainerCapability>,
}

fn db_error(
    project_id: i32,
    operation: &'static str,
    source: sea_orm::DbErr,
) -> ContainerSecurityError {
    ContainerSecurityError::Database {
        project_id,
        operation,
        source,
    }
}

fn names(capabilities: &BTreeSet<ContainerCapability>) -> String {
    if capabilities.is_empty() {
        return "none".to_string();
    }
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse(
    project_id: i32,
    values: &[String],
) -> Result<BTreeSet<ContainerCapability>, ContainerSecurityError> {
    ContainerCapability::parse_set(values)
        .map_err(|source| ContainerSecurityError::InvalidCapability { project_id, source })
}

/// Whether moving from `expected` to `requested` allows a capability that
/// was not allowed: the direction that needs step-up and human consent.
/// Invalid names are rejected here, before any of that runs.
pub fn adds_capabilities(
    project_id: i32,
    requested: &[String],
    expected: &[String],
) -> Result<bool, ContainerSecurityError> {
    let requested = parse(project_id, requested)?;
    let expected = parse(project_id, expected)?;
    Ok(requested.difference(&expected).next().is_some())
}

/// Reads and changes `projects.allowed_capabilities`. Kept apart from
/// `ProjectService` so its routes depend only on what they touch.
pub struct ContainerSecurityService {
    db: Arc<temps_database::DbConnection>,
}

impl ContainerSecurityService {
    pub fn new(db: Arc<temps_database::DbConnection>) -> Self {
        Self { db }
    }

    /// The project's allowed capabilities (empty: fully hardened).
    pub async fn allowed_capabilities(
        &self,
        project_id: i32,
    ) -> Result<Vec<ContainerCapability>, ContainerSecurityError> {
        read(self.db.as_ref(), project_id).await
    }

    /// Replace the project's allowed capabilities; see [`update`].
    pub async fn update_allowed_capabilities(
        &self,
        project_id: i32,
        requested: &[String],
        expected: &[String],
        acknowledged: bool,
    ) -> Result<ContainerSecurityChange, ContainerSecurityError> {
        update(
            self.db.as_ref(),
            project_id,
            requested,
            expected,
            acknowledged,
        )
        .await
    }
}

/// The project's allowed capabilities, sorted. Unknown stored values are
/// dropped (fail closed).
async fn read(
    db: &temps_database::DbConnection,
    project_id: i32,
) -> Result<Vec<ContainerCapability>, ContainerSecurityError> {
    let row = projects::Entity::find_by_id(project_id)
        .filter(projects::Column::IsDeleted.eq(false))
        .select_only()
        .column(projects::Column::AllowedCapabilities)
        .into_tuple::<Vec<String>>()
        .one(db)
        .await
        .map_err(|e| db_error(project_id, "read", e))?
        .ok_or(ContainerSecurityError::NotFound { project_id })?;
    Ok(ContainerCapability::from_stored(&row)
        .0
        .into_iter()
        .collect())
}

/// Replace the project's allowed capabilities with `requested`, provided
/// the saved value is still `expected`. Adding any capability requires
/// `acknowledged`; removing one never does.
async fn update(
    db: &temps_database::DbConnection,
    project_id: i32,
    requested: &[String],
    expected: &[String],
    acknowledged: bool,
) -> Result<ContainerSecurityChange, ContainerSecurityError> {
    // Validate before touching the database: an invalid name never opens
    // a transaction or takes the row lock.
    let requested = parse(project_id, requested)?;
    let expected = parse(project_id, expected)?;

    let tx = db
        .begin()
        .await
        .map_err(|e| db_error(project_id, "begin update of", e))?;
    let stored = projects::Entity::find_by_id(project_id)
        .filter(projects::Column::IsDeleted.eq(false))
        .select_only()
        .column(projects::Column::AllowedCapabilities)
        .lock_exclusive()
        .into_tuple::<Vec<String>>()
        .one(&tx)
        .await
        .map_err(|e| db_error(project_id, "lock project for", e))?
        .ok_or(ContainerSecurityError::NotFound { project_id })?;
    let previous = ContainerCapability::from_stored(&stored).0;
    if previous != expected {
        return Err(ContainerSecurityError::Conflict {
            project_id,
            current: names(&previous),
        });
    }
    let added: BTreeSet<ContainerCapability> = requested.difference(&previous).copied().collect();
    if !added.is_empty() && !acknowledged {
        return Err(ContainerSecurityError::AcknowledgementRequired {
            project_id,
            added: names(&added),
        });
    }
    if requested != previous {
        let values: Vec<String> = requested
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect();
        projects::Entity::update_many()
            .col_expr(projects::Column::AllowedCapabilities, Expr::value(values))
            .col_expr(projects::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
            .filter(projects::Column::Id.eq(project_id))
            .exec(&tx)
            .await
            .map_err(|e| db_error(project_id, "save", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| db_error(project_id, "commit", e))?;
    Ok(ContainerSecurityChange {
        previous: previous.into_iter().collect(),
        current: requested.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn stored(values: &[&str]) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([(
            "allowed_capabilities",
            Value::from(values.iter().map(|v| v.to_string()).collect::<Vec<_>>()),
        )])
    }
    /// The save statement as it appears in the Debug-formatted log.
    const SAVE: &str = r#"SET \"allowed_capabilities\""#;

    fn nbs() -> Vec<String> {
        vec!["NET_BIND_SERVICE".to_string()]
    }

    #[test]
    fn only_widening_counts_as_adding() {
        assert!(adds_capabilities(7, &nbs(), &[]).unwrap());
        assert!(!adds_capabilities(7, &[], &nbs()).unwrap());
        assert!(!adds_capabilities(7, &nbs(), &nbs()).unwrap());
        assert!(!adds_capabilities(7, &[], &[]).unwrap());
        assert!(matches!(
            adds_capabilities(7, &["SYS_ADMIN".to_string()], &[]),
            Err(ContainerSecurityError::InvalidCapability { project_id: 7, .. })
        ));
    }

    #[tokio::test]
    async fn read_defaults_to_fully_hardened_and_returns_saved_values() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&[])]])
            .append_query_results([[stored(&["NET_BIND_SERVICE"])]])
            .into_connection();
        assert!(read(&db, 7).await.unwrap().is_empty());
        assert_eq!(
            read(&db, 7).await.unwrap(),
            vec![ContainerCapability::NetBindService]
        );
    }

    #[tokio::test]
    async fn read_reports_the_missing_project_and_database_failures() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
            .append_query_errors([sea_orm::DbErr::Custom("read failed".into())])
            .into_connection();
        assert!(matches!(
            read(&db, 7).await,
            Err(ContainerSecurityError::NotFound { project_id: 7 })
        ));
        let error = read(&db, 7).await.unwrap_err();
        assert!(matches!(
            error,
            ContainerSecurityError::Database {
                project_id: 7,
                operation: "read",
                ..
            }
        ));
        assert!(error.to_string().contains("project 7"), "{error}");
    }

    #[tokio::test]
    async fn forbidden_and_unknown_capabilities_are_rejected_before_the_database() {
        // No query results: touching the database would fail the test.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        for bad in [
            "SYS_ADMIN",
            "ALL",
            "privileged",
            "NET_ADMIN",
            "not-a-cap",
            "",
        ] {
            let error = update(&db, 7, &[bad.to_string()], &[], true)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    ContainerSecurityError::InvalidCapability { project_id: 7, .. }
                ),
                "{bad}: {error:?}"
            );
            assert_eq!(
                Problem::from(error).status_code,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn adding_a_capability_requires_acknowledgement_and_rolls_back() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&[])]])
            .into_connection();
        let error = update(&db, 7, &nbs(), &[], false).await.unwrap_err();
        assert!(
            matches!(&error, ContainerSecurityError::AcknowledgementRequired { project_id: 7, added } if added == "NET_BIND_SERVICE"),
            "{error:?}"
        );
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("ROLLBACK"), "{log}");
        assert!(!log.contains(SAVE), "{log}");
    }

    #[tokio::test]
    async fn an_acknowledged_change_is_saved_under_the_row_lock() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&[])]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let change = update(&db, 7, &nbs(), &[], true).await.unwrap();
        assert!(change.previous.is_empty());
        assert_eq!(change.current, vec![ContainerCapability::NetBindService]);
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("FOR UPDATE"), "{log}");
        assert!(log.contains(SAVE), "{log}");
        assert!(log.contains("COMMIT"), "{log}");
    }

    #[tokio::test]
    async fn removing_a_capability_needs_no_acknowledgement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&["NET_BIND_SERVICE"])]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let change = update(&db, 7, &[], &nbs(), false).await.unwrap();
        assert_eq!(change.previous, vec![ContainerCapability::NetBindService]);
        assert!(change.current.is_empty());
    }

    #[tokio::test]
    async fn a_stale_editor_gets_a_conflict_naming_the_current_value() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&["NET_BIND_SERVICE"])]])
            .into_connection();
        let error = update(&db, 7, &nbs(), &[], true).await.unwrap_err();
        assert!(
            matches!(&error, ContainerSecurityError::Conflict { project_id: 7, current } if current == "NET_BIND_SERVICE"),
            "{error:?}"
        );
        assert_eq!(Problem::from(error).status_code, StatusCode::CONFLICT);
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains(SAVE), "{log}");
    }

    #[tokio::test]
    async fn an_unchanged_value_writes_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[stored(&["NET_BIND_SERVICE"])]])
            .into_connection();
        let change = update(&db, 7, &nbs(), &nbs(), false).await.unwrap();
        assert_eq!(change.previous, change.current);
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains(SAVE), "{log}");
    }

    #[tokio::test]
    async fn a_missing_project_is_not_found_on_update() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
            .into_connection();
        let error = update(&db, 9, &nbs(), &[], true).await.unwrap_err();
        assert!(matches!(
            error,
            ContainerSecurityError::NotFound { project_id: 9 }
        ));
        assert_eq!(Problem::from(error).status_code, StatusCode::NOT_FOUND);
    }
}
