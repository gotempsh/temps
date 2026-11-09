// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants: list, create, delete (ADR 100 D1-D4).
//!
//! Authorization is the handlers' job; this layer validates scopes, bounds
//! the number of grants and writes in one transaction. The database refuses
//! self-grants and duplicates too, and removes grants when either side is
//! deleted, so a write from any other path keeps the same guarantees.

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    FromQueryResult, Statement, TransactionTrait,
};
use temps_core::UtcDateTime;
use temps_entities::internal_access_grants;

use super::error::InternalAccessError;

/// The most grants one target project may hold. Every grant ends up in the
/// caller list of the target's routes in every route snapshot (ADR 100 D5).
pub const MAX_GRANTS_PER_PROJECT: u64 = 100;
pub const MAX_DESCRIPTION_LENGTH: usize = 200;
pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;

/// A grant with both sides named, for responses and audit entries.
#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct GrantView {
    pub id: i32,
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    pub target_environment_id: Option<i32>,
    pub target_environment_name: Option<String>,
    pub caller_project_id: i32,
    pub caller_project_name: String,
    pub caller_project_slug: String,
    pub caller_environment_id: Option<i32>,
    pub caller_environment_name: Option<String>,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: UtcDateTime,
}

/// What a new grant names. The target project comes from the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant {
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub caller_project_id: i32,
    /// `None`: every environment of the caller project.
    pub caller_environment_id: Option<i32>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrantPage {
    pub items: Vec<GrantView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

const VIEW_SELECT: &str = "\
SELECT g.id, g.target_project_id, tp.name AS target_project_name, \
       tp.slug AS target_project_slug, g.target_environment_id, \
       te.name AS target_environment_name, g.caller_project_id, \
       cp.name AS caller_project_name, cp.slug AS caller_project_slug, \
       g.caller_environment_id, ce.name AS caller_environment_name, \
       g.description, g.created_by, g.created_at \
FROM internal_access_grants g \
JOIN projects tp ON tp.id = g.target_project_id \
JOIN projects cp ON cp.id = g.caller_project_id \
LEFT JOIN environments te ON te.id = g.target_environment_id \
LEFT JOIN environments ce ON ce.id = g.caller_environment_id";

#[derive(Debug, FromQueryResult)]
struct CountRow {
    n: i64,
}

#[derive(Debug, FromQueryResult)]
struct IdRow {
    #[allow(dead_code, reason = "selected only to learn whether the row exists")]
    id: i32,
}

pub struct InternalAccessService {
    db: Arc<DatabaseConnection>,
}

impl InternalAccessService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Grants on `project_id`, newest first. With `environment_id`, only the
    /// grants that cover that environment: its own and the project-wide ones.
    pub async fn list(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<GrantPage, InternalAccessError> {
        let db = self.db.as_ref();
        ensure_live_project(db, project_id, false).await?;
        if let Some(environment_id) = environment_id {
            ensure_live_environment(db, project_id, environment_id).await?;
        }
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = (page - 1).saturating_mul(page_size);

        let (filter, mut values): (&str, Vec<sea_orm::Value>) = match environment_id {
            Some(environment_id) => (
                " WHERE g.target_project_id = $1 \
                  AND (g.target_environment_id IS NULL OR g.target_environment_id = $2)",
                vec![project_id.into(), environment_id.into()],
            ),
            None => (" WHERE g.target_project_id = $1", vec![project_id.into()]),
        };
        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("SELECT count(*)::BIGINT AS n FROM internal_access_grants g{filter}"),
            values.clone(),
        ))
        .one(db)
        .await
        .map_err(InternalAccessError::db("count internal access grants"))?
        .map_or(0, |row| u64::try_from(row.n).unwrap_or(0));

        let limit_index = values.len() + 1;
        let offset_index = values.len() + 2;
        values.push(i64::try_from(page_size).unwrap_or(i64::MAX).into());
        values.push(i64::try_from(offset).unwrap_or(i64::MAX).into());
        let items = GrantView::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "{VIEW_SELECT}{filter} ORDER BY g.created_at DESC, g.id DESC \
                 LIMIT ${limit_index} OFFSET ${offset_index}"
            ),
            values,
        ))
        .all(db)
        .await
        .map_err(InternalAccessError::db("list internal access grants"))?;

        Ok(GrantPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// Store a grant on `target_project_id`. Both projects must be live, each
    /// environment must be a live environment of its project, and the target
    /// may hold at most [`MAX_GRANTS_PER_PROJECT`].
    pub async fn create(
        &self,
        target_project_id: i32,
        grant: NewGrant,
        created_by: Option<i32>,
    ) -> Result<GrantView, InternalAccessError> {
        if grant.caller_project_id == target_project_id {
            return Err(InternalAccessError::SelfGrant {
                project_id: target_project_id,
            });
        }
        let description = grant
            .description
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if let Some(text) = description.as_deref() {
            let length = text.chars().count();
            if length > MAX_DESCRIPTION_LENGTH {
                return Err(InternalAccessError::DescriptionTooLong {
                    length,
                    max: MAX_DESCRIPTION_LENGTH,
                });
            }
        }

        let txn = self.db.begin().await.map_err(InternalAccessError::db(
            "begin creating an internal access grant",
        ))?;
        // Locking the target row serialises creates on one project, so two
        // concurrent requests cannot both pass the limit.
        ensure_live_project(&txn, target_project_id, true).await?;
        if let Some(environment_id) = grant.target_environment_id {
            ensure_live_environment(&txn, target_project_id, environment_id).await?;
        }
        ensure_live_project(&txn, grant.caller_project_id, false).await?;
        if let Some(environment_id) = grant.caller_environment_id {
            ensure_live_environment(&txn, grant.caller_project_id, environment_id).await?;
        }
        let existing = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*)::BIGINT AS n FROM internal_access_grants WHERE target_project_id = $1",
            [target_project_id.into()],
        ))
        .one(&txn)
        .await
        .map_err(InternalAccessError::db(
            "count a project's internal access grants",
        ))?
        .map_or(0, |row| u64::try_from(row.n).unwrap_or(0));
        if existing >= MAX_GRANTS_PER_PROJECT {
            return Err(InternalAccessError::TooManyGrants {
                project_id: target_project_id,
                limit: MAX_GRANTS_PER_PROJECT,
            });
        }

        let inserted = internal_access_grants::ActiveModel {
            target_project_id: Set(target_project_id),
            target_environment_id: Set(grant.target_environment_id),
            caller_project_id: Set(grant.caller_project_id),
            caller_environment_id: Set(grant.caller_environment_id),
            description: Set(description),
            created_by: Set(created_by),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(|source| match source.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                InternalAccessError::DuplicateGrant {
                    target_project_id,
                    caller_project_id: grant.caller_project_id,
                }
            }
            _ => InternalAccessError::Database {
                operation: "insert an internal access grant",
                source,
            },
        })?;
        let view = find_view(&txn, target_project_id, inserted.id).await?;
        txn.commit()
            .await
            .map_err(InternalAccessError::db("commit an internal access grant"))?;
        Ok(view)
    }

    /// Remove grant `grant_id` of `target_project_id`, returning what it was
    /// for the audit entry. A grant of another project is not found.
    pub async fn delete(
        &self,
        target_project_id: i32,
        grant_id: i32,
    ) -> Result<GrantView, InternalAccessError> {
        let txn = self.db.begin().await.map_err(InternalAccessError::db(
            "begin deleting an internal access grant",
        ))?;
        let view = find_view(&txn, target_project_id, grant_id).await?;
        let deleted = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM internal_access_grants WHERE id = $1 AND target_project_id = $2",
                [grant_id.into(), target_project_id.into()],
            ))
            .await
            .map_err(InternalAccessError::db("delete an internal access grant"))?;
        if deleted.rows_affected() == 0 {
            return Err(InternalAccessError::GrantNotFound {
                project_id: target_project_id,
                grant_id,
            });
        }
        txn.commit().await.map_err(InternalAccessError::db(
            "commit deleting an internal access grant",
        ))?;
        Ok(view)
    }
}

async fn find_view<C: ConnectionTrait>(
    db: &C,
    target_project_id: i32,
    grant_id: i32,
) -> Result<GrantView, InternalAccessError> {
    GrantView::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        format!("{VIEW_SELECT} WHERE g.id = $1 AND g.target_project_id = $2"),
        [grant_id.into(), target_project_id.into()],
    ))
    .one(db)
    .await
    .map_err(InternalAccessError::db("read an internal access grant"))?
    .ok_or(InternalAccessError::GrantNotFound {
        project_id: target_project_id,
        grant_id,
    })
}

async fn ensure_live_project<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    lock: bool,
) -> Result<(), InternalAccessError> {
    let sql = if lock {
        "SELECT id FROM projects WHERE id = $1 AND NOT is_deleted FOR UPDATE"
    } else {
        "SELECT id FROM projects WHERE id = $1 AND NOT is_deleted"
    };
    IdRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        [project_id.into()],
    ))
    .one(db)
    .await
    .map_err(InternalAccessError::db(
        "read a project for an internal access grant",
    ))?
    .map(|_| ())
    .ok_or(InternalAccessError::ProjectNotFound { project_id })
}

async fn ensure_live_environment<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
    environment_id: i32,
) -> Result<(), InternalAccessError> {
    IdRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id FROM environments WHERE id = $1 AND project_id = $2 AND deleted_at IS NULL",
        [environment_id.into(), project_id.into()],
    ))
    .one(db)
    .await
    .map_err(InternalAccessError::db(
        "read an environment for an internal access grant",
    ))?
    .map(|_| ())
    .ok_or(InternalAccessError::EnvironmentNotFound {
        project_id,
        environment_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::BTreeMap;

    fn id_row(id: i32) -> BTreeMap<&'static str, sea_orm::Value> {
        BTreeMap::from([("id", id.into())])
    }

    fn count_row(n: i64) -> BTreeMap<&'static str, sea_orm::Value> {
        BTreeMap::from([("n", n.into())])
    }

    fn grant(caller_project_id: i32) -> NewGrant {
        NewGrant {
            target_environment_id: None,
            caller_project_id,
            caller_environment_id: None,
            description: None,
        }
    }

    fn service(db: MockDatabase) -> InternalAccessService {
        InternalAccessService::new(Arc::new(db.into_connection()))
    }

    #[tokio::test]
    async fn a_self_grant_is_refused_before_the_database() {
        let error = service(MockDatabase::new(DatabaseBackend::Postgres))
            .create(1, grant(1), Some(9))
            .await
            .unwrap_err();
        assert!(
            matches!(error, InternalAccessError::SelfGrant { project_id: 1 }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_long_description_is_refused() {
        let mut new = grant(2);
        new.description = Some("x".repeat(MAX_DESCRIPTION_LENGTH + 1));
        let error = service(MockDatabase::new(DatabaseBackend::Postgres))
            .create(1, new, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::DescriptionTooLong { max: 200, .. }
        ));
    }

    #[tokio::test]
    async fn a_missing_target_project_is_not_found_with_its_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()]);
        let error = service(db).create(5, grant(2), None).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::ProjectNotFound { project_id: 5 }
        ));
        assert!(error.to_string().contains("Project 5"));
    }

    #[tokio::test]
    async fn an_environment_of_another_project_is_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(1)]])
            .append_query_results([vec![id_row(2)]])
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()]);
        let mut new = grant(2);
        new.caller_environment_id = Some(77);
        let error = service(db).create(1, new, None).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::EnvironmentNotFound {
                project_id: 2,
                environment_id: 77
            }
        ));
    }

    #[tokio::test]
    async fn the_limit_is_enforced_per_target_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(1)]])
            .append_query_results([vec![id_row(2)]])
            .append_query_results([vec![count_row(
                i64::try_from(MAX_GRANTS_PER_PROJECT).unwrap(),
            )]]);
        let error = service(db).create(1, grant(2), None).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::TooManyGrants {
                project_id: 1,
                limit: 100
            }
        ));
    }

    #[tokio::test]
    async fn a_database_failure_names_the_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())]);
        let error = service(db).list(1, None, None, None).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("read a project for an internal access grant"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn deleting_a_grant_of_another_project_is_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()]);
        let error = service(db).delete(1, 33).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::GrantNotFound {
                project_id: 1,
                grant_id: 33
            }
        ));
    }

    #[tokio::test]
    async fn a_delete_that_lost_a_race_is_not_found() {
        let now = chrono::Utc::now();
        let view: BTreeMap<&str, sea_orm::Value> = BTreeMap::from([
            ("id", 33.into()),
            ("target_project_id", 1.into()),
            ("target_project_name", "api".into()),
            ("target_project_slug", "api".into()),
            ("target_environment_id", Option::<i32>::None.into()),
            ("target_environment_name", Option::<String>::None.into()),
            ("caller_project_id", 2.into()),
            ("caller_project_name", "worker".into()),
            ("caller_project_slug", "worker".into()),
            ("caller_environment_id", Option::<i32>::None.into()),
            ("caller_environment_name", Option::<String>::None.into()),
            ("description", Option::<String>::None.into()),
            ("created_by", Option::<i32>::None.into()),
            ("created_at", now.into()),
        ]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![view]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]);
        let error = service(db).delete(1, 33).await.unwrap_err();
        assert!(matches!(error, InternalAccessError::GrantNotFound { .. }));
    }

    #[tokio::test]
    async fn list_pages_are_bounded() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(1)]])
            .append_query_results([vec![count_row(0)]])
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()]);
        let page = service(db)
            .list(1, None, Some(0), Some(10_000))
            .await
            .expect("lists");
        assert_eq!(page.page, 1);
        assert_eq!(page.page_size, MAX_PAGE_SIZE);
        assert_eq!(page.total, 0);
    }
}
