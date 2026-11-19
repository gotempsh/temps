// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants with a sandbox caller (ADR 105): list, create,
//! delete, and the addresses one sandbox may call.
//!
//! A grant names exactly one sandbox, never "every sandbox of a user": a
//! sandbox runs whatever its owner or an agent puts in it, so each one is
//! admitted on its own. Creating one needs the sandbox to belong to the
//! user granting it. The database removes the grant when the sandbox is
//! destroyed and announces every change on `route_table_changes`, so the
//! internal proxies apply it with their next route snapshot.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseBackend, FromQueryResult,
    Statement, TransactionTrait,
};
use temps_core::UtcDateTime;
use temps_entities::internal_access_sandbox_grants;

use super::error::InternalAccessError;
use super::service::{
    ensure_below_grant_limit, ensure_live_environment, ensure_live_project, CountRow,
    InternalAccessService, DEFAULT_PAGE_SIZE, MAX_DESCRIPTION_LENGTH, MAX_PAGE_SIZE,
};

/// A sandbox grant with the target and the sandbox named.
#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct SandboxGrantView {
    pub id: i32,
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    pub target_environment_id: Option<i32>,
    pub target_environment_name: Option<String>,
    /// The sandbox's public id.
    pub sandbox_id: String,
    /// The name the user gave the sandbox (`sandboxes.name`). The internal
    /// proxy knows it by the container its public id runs as instead.
    pub sandbox_name: String,
    pub sandbox_status: String,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    pub created_at: UtcDateTime,
}

/// What a new sandbox grant names. The target project comes from the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSandboxGrant {
    /// `None`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    /// The sandbox's public id.
    pub sandbox_id: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxGrantPage {
    pub items: Vec<SandboxGrantView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// One internal address a sandbox may call, and the grant that allows it.
#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct SandboxAddressView {
    pub grant_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    pub environment_slug: String,
    pub environment_subdomain: String,
    /// Another owner (a managed service, a node) holds the subdomain alias's
    /// name, so the alias is not this environment's (ADR-105 D7).
    pub alias_taken: bool,
}

impl SandboxAddressView {
    /// `<environment>.<project>.temps.local`, as the DNS publisher names it.
    pub fn hostname(&self) -> String {
        format!(
            "{}.{}.temps.local",
            self.environment_slug, self.project_slug
        )
    }

    /// `<environment subdomain>.temps.local` (ADR-105 D7), when the subdomain
    /// is one DNS label and no other owner holds the name.
    pub fn alias_hostname(&self) -> Option<String> {
        temps_core::internal_access::environment_alias_name(&self.environment_subdomain)
            .filter(|_| !self.alias_taken)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxAddressPage {
    pub items: Vec<SandboxAddressView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

const SANDBOX_VIEW_SELECT: &str = "\
SELECT g.id, g.target_project_id, tp.name AS target_project_name, \
       tp.slug AS target_project_slug, g.target_environment_id, \
       te.name AS target_environment_name, s.public_id AS sandbox_id, \
       s.name AS sandbox_name, s.status AS sandbox_status, \
       g.description, g.created_by, g.created_at \
FROM internal_access_sandbox_grants g \
JOIN projects tp ON tp.id = g.target_project_id \
JOIN sandboxes s ON s.id = g.sandbox_id \
LEFT JOIN environments te ON te.id = g.target_environment_id";

/// Every live environment a sandbox's grants cover, one row per address.
const ADDRESSES_FROM: &str = "\
FROM internal_access_sandbox_grants g \
JOIN sandboxes s ON s.id = g.sandbox_id \
JOIN projects p ON p.id = g.target_project_id AND NOT p.is_deleted \
JOIN environments e ON e.project_id = p.id AND e.deleted_at IS NULL \
     AND (g.target_environment_id IS NULL OR e.id = g.target_environment_id) \
WHERE s.public_id = $1 AND s.user_id = $2 AND s.status <> 'destroyed'";

#[derive(Debug, FromQueryResult)]
struct SandboxRow {
    id: i32,
}

fn page_bounds(page: Option<u64>, page_size: Option<u64>) -> (u64, u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    (page, page_size, (page - 1).saturating_mul(page_size))
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl InternalAccessService {
    /// Sandbox grants on `project_id`, newest first. With `environment_id`,
    /// only the ones that cover that environment.
    pub async fn list_sandbox_grants(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<SandboxGrantPage, InternalAccessError> {
        let db = self.db.as_ref();
        ensure_live_project(db, project_id, false).await?;
        if let Some(environment_id) = environment_id {
            ensure_live_environment(db, project_id, environment_id).await?;
        }
        let (page, page_size, offset) = page_bounds(page, page_size);
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
            format!("SELECT count(*)::BIGINT AS n FROM internal_access_sandbox_grants g{filter}"),
            values.clone(),
        ))
        .one(db)
        .await
        .map_err(InternalAccessError::db(
            "count internal access sandbox grants",
        ))?
        .map_or(0, |row| u64::try_from(row.n).unwrap_or(0));
        let limit_index = values.len() + 1;
        let offset_index = values.len() + 2;
        values.push(as_i64(page_size).into());
        values.push(as_i64(offset).into());
        let items = SandboxGrantView::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "{SANDBOX_VIEW_SELECT}{filter} ORDER BY g.created_at DESC, g.id DESC \
                 LIMIT ${limit_index} OFFSET ${offset_index}"
            ),
            values,
        ))
        .all(db)
        .await
        .map_err(InternalAccessError::db(
            "list internal access sandbox grants",
        ))?;
        Ok(SandboxGrantPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// Let sandbox `grant.sandbox_id`, which `user_id` owns, call
    /// `target_project_id` (or one environment of it). The target must be
    /// live, the sandbox not destroyed, and the target below
    /// [`super::service::MAX_GRANTS_PER_PROJECT`] grants of either kind.
    pub async fn create_sandbox_grant(
        &self,
        target_project_id: i32,
        grant: NewSandboxGrant,
        user_id: i32,
    ) -> Result<SandboxGrantView, InternalAccessError> {
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
            "begin creating an internal access sandbox grant",
        ))?;
        // The target row lock serialises creates on one project (the limit).
        ensure_live_project(&txn, target_project_id, true).await?;
        if let Some(environment_id) = grant.target_environment_id {
            ensure_live_environment(&txn, target_project_id, environment_id).await?;
        }
        // FOR SHARE: a concurrent destroy waits for this commit, and its
        // trigger then removes the grant this inserts.
        let sandbox = SandboxRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM sandboxes \
             WHERE public_id = $1 AND user_id = $2 AND status <> 'destroyed' FOR SHARE",
            [grant.sandbox_id.clone().into(), user_id.into()],
        ))
        .one(&txn)
        .await
        .map_err(InternalAccessError::db(
            "read a sandbox for an internal access grant",
        ))?
        .ok_or_else(|| InternalAccessError::SandboxNotFound {
            sandbox_id: grant.sandbox_id.clone(),
        })?;
        ensure_below_grant_limit(&txn, target_project_id).await?;

        let inserted = internal_access_sandbox_grants::ActiveModel {
            target_project_id: Set(target_project_id),
            target_environment_id: Set(grant.target_environment_id),
            sandbox_id: Set(sandbox.id),
            description: Set(description),
            created_by: Set(Some(user_id)),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(|source| match source.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                InternalAccessError::DuplicateSandboxGrant {
                    target_project_id,
                    sandbox_id: grant.sandbox_id.clone(),
                }
            }
            _ => InternalAccessError::Database {
                operation: "insert an internal access sandbox grant",
                source,
            },
        })?;
        let view = find_sandbox_view(&txn, target_project_id, inserted.id).await?;
        txn.commit().await.map_err(InternalAccessError::db(
            "commit an internal access sandbox grant",
        ))?;
        Ok(view)
    }

    /// Remove sandbox grant `grant_id` of `target_project_id`, returning what
    /// it was for the audit entry.
    pub async fn delete_sandbox_grant(
        &self,
        target_project_id: i32,
        grant_id: i32,
    ) -> Result<SandboxGrantView, InternalAccessError> {
        let txn = self.db.begin().await.map_err(InternalAccessError::db(
            "begin deleting an internal access sandbox grant",
        ))?;
        let view = find_sandbox_view(&txn, target_project_id, grant_id).await?;
        let deleted = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM internal_access_sandbox_grants \
                 WHERE id = $1 AND target_project_id = $2",
                [grant_id.into(), target_project_id.into()],
            ))
            .await
            .map_err(InternalAccessError::db(
                "delete an internal access sandbox grant",
            ))?;
        if deleted.rows_affected() == 0 {
            return Err(InternalAccessError::SandboxGrantNotFound {
                project_id: target_project_id,
                grant_id,
            });
        }
        txn.commit().await.map_err(InternalAccessError::db(
            "commit deleting an internal access sandbox grant",
        ))?;
        Ok(view)
    }

    /// The internal addresses sandbox `sandbox_id`, which `user_id` owns, may
    /// call: one per live environment its grants cover, by project and
    /// environment name.
    pub async fn sandbox_addresses(
        &self,
        sandbox_id: &str,
        user_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<SandboxAddressPage, InternalAccessError> {
        let db = self.db.as_ref();
        SandboxRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM sandboxes WHERE public_id = $1 AND user_id = $2 \
             AND status <> 'destroyed'",
            [sandbox_id.into(), user_id.into()],
        ))
        .one(db)
        .await
        .map_err(InternalAccessError::db(
            "read a sandbox for its internal addresses",
        ))?
        .ok_or_else(|| InternalAccessError::SandboxNotFound {
            sandbox_id: sandbox_id.to_string(),
        })?;
        let (page, page_size, offset) = page_bounds(page, page_size);
        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("SELECT count(*)::BIGINT AS n {ADDRESSES_FROM}"),
            [sandbox_id.into(), user_id.into()],
        ))
        .one(db)
        .await
        .map_err(InternalAccessError::db(
            "count a sandbox's internal addresses",
        ))?
        .map_or(0, |row| u64::try_from(row.n).unwrap_or(0));
        let items = SandboxAddressView::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT g.id AS grant_id, p.id AS project_id, p.name AS project_name, \
                        p.slug AS project_slug, e.id AS environment_id, \
                        e.name AS environment_name, e.slug AS environment_slug, \
                        e.subdomain AS environment_subdomain, \
                        EXISTS (SELECT 1 FROM dns_name_claims c \
                                WHERE c.fqdn = lower(btrim(e.subdomain)) || '.temps.local' \
                                  AND c.owner_kind <> $5) AS alias_taken \
                 {ADDRESSES_FROM} ORDER BY p.name, e.name, e.id LIMIT $3 OFFSET $4"
            ),
            [
                sandbox_id.into(),
                user_id.into(),
                as_i64(page_size).into(),
                as_i64(offset).into(),
                temps_core::internal_access::ENVIRONMENT_ALIAS_OWNER_KIND.into(),
            ],
        ))
        .all(db)
        .await
        .map_err(InternalAccessError::db(
            "list a sandbox's internal addresses",
        ))?;
        Ok(SandboxAddressPage {
            items,
            total,
            page,
            page_size,
        })
    }
}

async fn find_sandbox_view<C: ConnectionTrait>(
    db: &C,
    target_project_id: i32,
    grant_id: i32,
) -> Result<SandboxGrantView, InternalAccessError> {
    SandboxGrantView::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        format!("{SANDBOX_VIEW_SELECT} WHERE g.id = $1 AND g.target_project_id = $2"),
        [grant_id.into(), target_project_id.into()],
    ))
    .one(db)
    .await
    .map_err(InternalAccessError::db(
        "read an internal access sandbox grant",
    ))?
    .ok_or(InternalAccessError::SandboxGrantNotFound {
        project_id: target_project_id,
        grant_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    type Row = BTreeMap<&'static str, sea_orm::Value>;

    fn id_row(id: i32) -> Row {
        BTreeMap::from([("id", id.into())])
    }

    fn count_row(n: i64) -> Row {
        BTreeMap::from([("n", n.into())])
    }

    fn service(db: MockDatabase) -> InternalAccessService {
        InternalAccessService::new(Arc::new(db.into_connection()))
    }

    fn grant() -> NewSandboxGrant {
        NewSandboxGrant {
            target_environment_id: None,
            sandbox_id: "sbx_alpha".to_string(),
            description: None,
        }
    }

    #[tokio::test]
    async fn a_sandbox_the_user_does_not_own_is_not_found_with_its_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(1)]])
            .append_query_results([Vec::<Row>::new()]);
        let error = service(db)
            .create_sandbox_grant(1, grant(), 9)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, InternalAccessError::SandboxNotFound { sandbox_id } if sandbox_id == "sbx_alpha"),
            "{error}"
        );
        assert!(error.to_string().contains("sbx_alpha"));
    }

    #[tokio::test]
    async fn project_and_sandbox_grants_share_the_limit() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(1)]])
            .append_query_results([vec![id_row(5)]])
            .append_query_results([vec![count_row(100)]]);
        let error = service(db)
            .create_sandbox_grant(1, grant(), 9)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::TooManyGrants {
                project_id: 1,
                limit: 100
            }
        ));
    }

    #[tokio::test]
    async fn a_long_description_is_refused_before_the_database() {
        let mut new = grant();
        new.description = Some("x".repeat(MAX_DESCRIPTION_LENGTH + 1));
        let error = service(MockDatabase::new(DatabaseBackend::Postgres))
            .create_sandbox_grant(1, new, 9)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::DescriptionTooLong { max: 200, .. }
        ));
    }

    #[tokio::test]
    async fn a_target_being_deleted_is_not_found() {
        let db =
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([Vec::<Row>::new()]);
        let error = service(db)
            .create_sandbox_grant(4, grant(), 9)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::ProjectNotFound { project_id: 4 }
        ));
    }

    #[tokio::test]
    async fn deleting_a_missing_sandbox_grant_names_it() {
        let db =
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([Vec::<Row>::new()]);
        let error = service(db).delete_sandbox_grant(1, 41).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::SandboxGrantNotFound {
                project_id: 1,
                grant_id: 41
            }
        ));
    }

    #[tokio::test]
    async fn a_delete_that_lost_a_race_is_not_found() {
        let view: Row = BTreeMap::from([
            ("id", 41.into()),
            ("target_project_id", 1.into()),
            ("target_project_name", "api".into()),
            ("target_project_slug", "api".into()),
            ("target_environment_id", Option::<i32>::None.into()),
            ("target_environment_name", Option::<String>::None.into()),
            ("sandbox_id", "sbx_alpha".into()),
            ("sandbox_name", "agent scratchpad".into()),
            ("sandbox_status", "running".into()),
            ("description", Option::<String>::None.into()),
            ("created_by", Some(9).into()),
            ("created_at", chrono::Utc::now().into()),
        ]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![view]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]);
        let error = service(db).delete_sandbox_grant(1, 41).await.unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::SandboxGrantNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn addresses_of_a_sandbox_someone_else_owns_are_not_found() {
        let db =
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([Vec::<Row>::new()]);
        let error = service(db)
            .sandbox_addresses("sbx_other", 9, None, None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            InternalAccessError::SandboxNotFound { ref sandbox_id } if sandbox_id == "sbx_other"
        ));
    }

    #[tokio::test]
    async fn a_database_failure_names_the_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".into())]);
        let error = service(db)
            .sandbox_addresses("sbx_alpha", 9, None, None)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("read a sandbox for its internal addresses"),
            "{error}"
        );
    }

    /// The SQL against a migrated database: a grant names only the
    /// creator's live sandbox, lists the addresses it covers, shares the
    /// limit, and is gone once the sandbox is destroyed.
    #[tokio::test]
    async fn sandbox_grants_round_trip_through_the_database() {
        use sea_orm::{ConnectionTrait, FromQueryResult};
        use temps_test_utils::TestDatabase;

        #[derive(FromQueryResult)]
        struct Id {
            id: i32,
        }
        let database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error) => {
                eprintln!("test database unavailable, skipping: {error}");
                return;
            }
        };
        let conn = database.connection_arc();
        let c = conn.as_ref();
        let insert = |sql: String| async move {
            Id::find_by_statement(Statement::from_string(DatabaseBackend::Postgres, sql))
                .one(c)
                .await
                .expect("insert")
                .expect("returned id")
                .id
        };
        let user = |email: &'static str| {
            format!(
                "INSERT INTO users (name, email, email_verified, must_change_password, \
                     mfa_enabled, created_at, updated_at) \
                 VALUES ('{email}', '{email}', TRUE, FALSE, FALSE, NOW(), NOW()) RETURNING id"
            )
        };
        let owner = insert(user("owner@example.test")).await;
        let other = insert(user("other@example.test")).await;
        let project = insert(
            "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, \
                 created_at, updated_at, slug, is_deleted, is_public_repo, attack_mode, \
                 enable_preview_environments, source_type) \
             VALUES ('Orders', 'r', 'o', '.', 'main', 'nextjs', NOW(), NOW(), 'orders', FALSE, \
                 TRUE, FALSE, FALSE, 'git') RETURNING id"
                .to_string(),
        )
        .await;
        for slug in ["production", "staging"] {
            insert(format!(
                "INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                     is_preview, created_at, updated_at) \
                 VALUES ('{slug}', '{slug}', 'orders-{slug}', 'orders-{slug}', '[]'::json, \
                     {project}, FALSE, NOW(), NOW()) RETURNING id"
            ))
            .await;
        }
        for (public_id, name, user_id) in [
            ("sbx_mine000001", "my scratchpad", owner),
            ("sbx_theirs0001", "their scratchpad", other),
        ] {
            insert(format!(
                "INSERT INTO sandboxes (public_id, name, status, work_dir, timeout_secs, user_id, \
                     created_at, last_activity_at, expires_at) \
                 VALUES ('{public_id}', '{name}', 'running', '/workspace', 3600, {user_id}, \
                     NOW(), NOW(), NOW() + INTERVAL '1 hour') RETURNING id"
            ))
            .await;
        }
        let service = InternalAccessService::new(conn.clone());
        let new = |sandbox: &str| NewSandboxGrant {
            target_environment_id: None,
            sandbox_id: sandbox.to_string(),
            description: Some("  agent calls the api  ".to_string()),
        };

        let error = service
            .create_sandbox_grant(project, new("sbx_theirs0001"), owner)
            .await
            .unwrap_err();
        assert!(
            matches!(error, InternalAccessError::SandboxNotFound { .. }),
            "{error}"
        );

        let grant = service
            .create_sandbox_grant(project, new("sbx_mine000001"), owner)
            .await
            .expect("granted");
        assert_eq!(grant.sandbox_name, "my scratchpad");
        assert_eq!(grant.description.as_deref(), Some("agent calls the api"));
        let error = service
            .create_sandbox_grant(project, new("sbx_mine000001"), owner)
            .await
            .unwrap_err();
        assert!(
            matches!(error, InternalAccessError::DuplicateSandboxGrant { .. }),
            "{error}"
        );

        let listed = service
            .list_sandbox_grants(project, None, None, None)
            .await
            .expect("listed");
        assert_eq!((listed.total, listed.items.len()), (1, 1));

        let addresses = service
            .sandbox_addresses("sbx_mine000001", owner, None, None)
            .await
            .expect("addresses");
        let hostnames: Vec<String> = addresses
            .items
            .iter()
            .map(SandboxAddressView::hostname)
            .collect();
        assert_eq!(
            hostnames,
            [
                "production.orders.temps.local",
                "staging.orders.temps.local"
            ]
        );
        let aliases = |page: &SandboxAddressPage| -> Vec<Option<String>> {
            page.items
                .iter()
                .map(SandboxAddressView::alias_hostname)
                .collect()
        };
        assert_eq!(
            aliases(&addresses),
            [
                Some("orders-production.temps.local".to_string()),
                Some("orders-staging.temps.local".to_string())
            ]
        );
        // A managed service holding `orders-staging.temps.local` takes that
        // alias away; the alias's own claim never does.
        conn.execute(sea_orm::Statement::from_string(
            DatabaseBackend::Postgres,
            "INSERT INTO dns_name_claims (fqdn, owner_kind, owner_id) VALUES \
             ('orders-staging.temps.local', 'service_role', 3), \
             ('orders-production.temps.local', 'environment_subdomain', 1)",
        ))
        .await
        .expect("claims");
        let addresses = service
            .sandbox_addresses("sbx_mine000001", owner, None, None)
            .await
            .expect("addresses");
        assert_eq!(
            aliases(&addresses),
            [Some("orders-production.temps.local".to_string()), None]
        );
        assert!(matches!(
            service
                .sandbox_addresses("sbx_mine000001", other, None, None)
                .await,
            Err(InternalAccessError::SandboxNotFound { .. })
        ));

        c.execute_unprepared(
            "UPDATE sandboxes SET status = 'destroyed' WHERE public_id = 'sbx_mine000001'",
        )
        .await
        .expect("destroy");
        let listed = service
            .list_sandbox_grants(project, None, None, None)
            .await
            .expect("listed");
        assert_eq!(listed.total, 0, "a destroyed sandbox's grant is removed");
        assert!(matches!(
            service.delete_sandbox_grant(project, grant.id).await,
            Err(InternalAccessError::SandboxGrantNotFound { .. })
        ));
    }

    #[test]
    fn an_address_is_named_as_the_dns_publisher_names_it() {
        let address = SandboxAddressView {
            grant_id: 1,
            project_id: 2,
            project_name: "Orders".into(),
            project_slug: "orders".into(),
            environment_id: 3,
            environment_name: "Production".into(),
            environment_slug: "production".into(),
            environment_subdomain: "orders-production".into(),
            alias_taken: false,
        };
        assert_eq!(address.hostname(), "production.orders.temps.local");
        assert_eq!(
            address.alias_hostname().as_deref(),
            Some("orders-production.temps.local")
        );
        let taken = SandboxAddressView {
            alias_taken: true,
            ..address.clone()
        };
        assert_eq!(taken.alias_hostname(), None, "a name another owner holds");
        let dotted = SandboxAddressView {
            environment_subdomain: "orders.production".into(),
            ..address
        };
        assert_eq!(dotted.alias_hostname(), None, "not one DNS label");
    }
}
