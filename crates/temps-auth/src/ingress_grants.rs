// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Revoking Temps-login ingress grants (ADR-055 §11) when access is lost.
//!
//! A grant is minted after the console checked the user's permission and
//! project access, and the proxy then admits its session cookie for up to
//! twelve hours without asking the database again. So every write that can
//! take that access away must revoke the grants it invalidates; the proxy
//! learns of it from the statement-level `revoked_at` trigger, which reloads
//! the route snapshot that carries the revocation list.
//!
//! The helpers revoke more than strictly necessary (all of a user's grants,
//! or every grant on a project) rather than re-resolve exactly which access
//! survived. A user who still has access just signs in again through the
//! same redirect; a user who lost it is not let through in the meantime.
//!
//! They take a connection so the caller can run them inside the
//! transaction that removes the access: if revocation fails, the access
//! change rolls back with it instead of half-applying.

use chrono::Utc;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use temps_entities::{environments, ingress_login_grants, sandboxes, users};

/// Revokes every live grant held by `user_id`. Returns how many it revoked.
pub async fn revoke_for_user<C: ConnectionTrait>(conn: &C, user_id: i32) -> Result<u64, DbErr> {
    revoke_for_users(conn, &[user_id]).await
}

/// Revokes every live grant held by any of `user_ids`.
pub async fn revoke_for_users<C: ConnectionTrait>(
    conn: &C,
    user_ids: &[i32],
) -> Result<u64, DbErr> {
    if user_ids.is_empty() {
        return Ok(0);
    }
    let now = Utc::now();
    let result = ingress_login_grants::Entity::update_many()
        .col_expr(ingress_login_grants::Column::RevokedAt, Expr::value(now))
        .filter(ingress_login_grants::Column::UserId.is_in(user_ids.iter().copied()))
        .filter(ingress_login_grants::Column::RevokedAt.is_null())
        .filter(ingress_login_grants::Column::ExpiresAt.gt(now))
        .exec(conn)
        .await?;
    Ok(result.rows_affected)
}

/// Revokes every live grant held by a user who signs in through OIDC provider
/// `provider_id`. Mirrors the user set whose console sessions
/// `OidcService::revoke_sessions_for_provider` deletes: removing or revoking
/// the provider ends those sessions, so the ingress cookies minted from them
/// must end with them.
pub async fn revoke_for_oidc_provider<C: ConnectionTrait>(
    conn: &C,
    provider_id: i32,
) -> Result<u64, DbErr> {
    let now = Utc::now();
    let provider_users = Query::select()
        .column(users::Column::Id)
        .from(users::Entity)
        .and_where(Expr::col(users::Column::OidcProviderId).eq(provider_id))
        .and_where(Expr::col(users::Column::DeletedAt).is_null())
        .to_owned();
    let result = ingress_login_grants::Entity::update_many()
        .col_expr(ingress_login_grants::Column::RevokedAt, Expr::value(now))
        .filter(Expr::col(ingress_login_grants::Column::UserId).in_subquery(provider_users))
        .filter(ingress_login_grants::Column::RevokedAt.is_null())
        .filter(ingress_login_grants::Column::ExpiresAt.gt(now))
        .exec(conn)
        .await?;
    Ok(result.rows_affected)
}

/// Revokes every live grant on the environments and sandboxes of
/// `project_id`, whoever holds it: a change to who may reach the project
/// (gating it to teams, or removing a team's access) invalidates them all.
pub async fn revoke_for_project<C: ConnectionTrait>(
    conn: &C,
    project_id: i32,
) -> Result<u64, DbErr> {
    let now = Utc::now();
    let environment_ids = Query::select()
        .column(environments::Column::Id)
        .from(environments::Entity)
        .and_where(Expr::col(environments::Column::ProjectId).eq(project_id))
        .to_owned();
    let sandbox_ids = Query::select()
        .column(sandboxes::Column::Id)
        .from(sandboxes::Entity)
        .and_where(Expr::col(sandboxes::Column::ProjectId).eq(project_id))
        .to_owned();
    let on_project = Expr::col(ingress_login_grants::Column::OwnerKind)
        .eq("environment")
        .and(Expr::col(ingress_login_grants::Column::OwnerId).in_subquery(environment_ids))
        .or(Expr::col(ingress_login_grants::Column::OwnerKind)
            .eq("sandbox")
            .and(Expr::col(ingress_login_grants::Column::OwnerId).in_subquery(sandbox_ids)));
    let result = ingress_login_grants::Entity::update_many()
        .col_expr(ingress_login_grants::Column::RevokedAt, Expr::value(now))
        .filter(on_project)
        .filter(ingress_login_grants::Column::RevokedAt.is_null())
        .filter(ingress_login_grants::Column::ExpiresAt.gt(now))
        .exec(conn)
        .await?;
    Ok(result.rows_affected)
}

/// Fixtures for tests elsewhere in the crate that prove a session- or
/// credential-invalidation path also ends the user's ingress grants.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};

    /// Inserts a grant for `user_id` that stays live for twelve hours, as the
    /// console mints it on a Temps-login redirect.
    pub(crate) async fn live_grant(db: &sea_orm::DatabaseConnection, user_id: i32) -> i64 {
        ingress_login_grants::ActiveModel {
            user_id: Set(user_id),
            owner_kind: Set("environment".to_string()),
            owner_id: Set(1),
            host: Set("app.example.test".to_string()),
            permission: Set("environments:read".to_string()),
            expires_at: Set(Utc::now() + chrono::Duration::hours(12)),
            revoked_at: Set(None),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert ingress login grant")
        .id
    }

    pub(crate) async fn is_revoked(db: &sea_orm::DatabaseConnection, grant_id: i64) -> bool {
        ingress_login_grants::Entity::find_by_id(grant_id)
            .one(db)
            .await
            .expect("read ingress login grant")
            .expect("ingress login grant exists")
            .revoked_at
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;

    async fn test_database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_database::test_utils::is_container_runtime_unavailable(
                        &error.to_string(),
                    ) =>
            {
                println!("Docker not available, skipping: {error}");
                None
            }
            Err(error) => panic!("test database failed to start: {error}"),
        }
    }

    async fn user(db: &sea_orm::DatabaseConnection, email: &str) -> i32 {
        temps_entities::users::ActiveModel {
            name: Set(email.to_string()),
            email: Set(email.to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert user")
        .id
    }

    async fn grant(
        db: &sea_orm::DatabaseConnection,
        user_id: i32,
        owner_kind: &str,
        owner_id: i64,
        expires_in: chrono::Duration,
    ) -> i64 {
        ingress_login_grants::ActiveModel {
            user_id: Set(user_id),
            owner_kind: Set(owner_kind.to_string()),
            owner_id: Set(owner_id),
            host: Set("app.example.test".to_string()),
            permission: Set("environments:read".to_string()),
            expires_at: Set(Utc::now() + expires_in),
            revoked_at: Set(None),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert grant")
        .id
    }

    /// A project with one environment; returns `(project_id, environment_id)`.
    async fn project_with_environment(test_db: &TestDatabase, slug: &str) -> (i32, i32) {
        let rows = test_db
            .query_sql(&format!(
                "WITH p AS (INSERT INTO projects (name, repo_name, repo_owner, directory, \
                 main_branch, preset, slug, created_at, updated_at) VALUES ('{slug}', '{slug}', \
                 'owner', '.', 'main', 'dockerfile', '{slug}', NOW(), NOW()) RETURNING id) \
                 INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                 created_at, updated_at) SELECT 'production', 'production', '{slug}', \
                 '{slug}.example.test', '[]'::json, p.id, NOW(), NOW() FROM p \
                 RETURNING project_id, id"
            ))
            .await
            .expect("insert project and environment");
        let row = rows.first().expect("one environment row");
        (
            row.try_get("", "project_id").expect("project_id"),
            row.try_get("", "id").expect("environment id"),
        )
    }

    async fn revoked(db: &sea_orm::DatabaseConnection, id: i64) -> bool {
        ingress_login_grants::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("read grant")
            .expect("grant exists")
            .revoked_at
            .is_some()
    }

    #[tokio::test]
    async fn revoking_a_user_leaves_other_users_and_expired_grants_alone() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let db = test_db.db.as_ref();
        let alice = user(db, "alice@example.test").await;
        let bob = user(db, "bob@example.test").await;
        let live = grant(db, alice, "environment", 1, chrono::Duration::hours(12)).await;
        let expired = grant(db, alice, "environment", 1, chrono::Duration::hours(-1)).await;
        let other = grant(db, bob, "environment", 1, chrono::Duration::hours(12)).await;

        assert_eq!(revoke_for_user(db, alice).await.expect("revoke"), 1);
        assert!(revoked(db, live).await);
        // An expired grant buys nothing; leaving it untouched keeps it out
        // of the snapshot's revocation list.
        assert!(!revoked(db, expired).await);
        assert!(!revoked(db, other).await);
        // Idempotent: nothing left to revoke.
        assert_eq!(revoke_for_user(db, alice).await.expect("revoke"), 0);
    }

    #[tokio::test]
    async fn revoking_a_project_revokes_only_its_environments_grants() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let db = test_db.db.as_ref();
        let alice = user(db, "carol@example.test").await;
        let (project, environment) = project_with_environment(&test_db, "grants-p1").await;
        let (_, other_environment) = project_with_environment(&test_db, "grants-p2").await;
        let on_project = grant(
            db,
            alice,
            "environment",
            i64::from(environment),
            chrono::Duration::hours(12),
        )
        .await;
        let off_project = grant(
            db,
            alice,
            "environment",
            i64::from(other_environment),
            chrono::Duration::hours(12),
        )
        .await;

        assert_eq!(revoke_for_project(db, project).await.expect("revoke"), 1);
        assert!(revoked(db, on_project).await);
        assert!(!revoked(db, off_project).await);
    }
}
