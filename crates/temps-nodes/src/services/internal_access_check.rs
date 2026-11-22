// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a project may call an internal name (ADR 101 D7), for the DNS
//! *Test resolution* tool.
//!
//! A name resolving is half of a working call: the internal proxy then
//! answers 403 to a caller from another project unless the name's project
//! granted it internal access. Asked as a project, the test reports which
//! project owns the name and whether a grant admits the caller, so the console
//! can link to the owner's Internal access settings instead of leaving the
//! operator to discover the 403 from the app's logs.
//!
//! Asked as a sandbox (ADR 105), the verdict is whether a sandbox grant on
//! the name's project admits that sandbox. A sandbox never looks the name up
//! itself: its egress proxy sends `http://<name>/` to the internal proxy of
//! the node the sandbox runs on.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, FromQueryResult, Statement};
use temps_core::internal_access::{
    admitted_sandbox, callers_for_route, sandboxes_for_route, InternalAccessCaller,
    InternalAccessGrantScope, InternalAccessSandboxCaller, InternalAccessSandboxGrantScope,
};

/// How far a grant on the name's project reaches the asking project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalAccessReach {
    /// The name belongs to the asking project: no grant needed.
    SameProject,
    /// Every environment of the asking project may call it.
    Granted,
    /// Only some environments of the asking project may call it.
    SomeEnvironments,
    /// The internal proxy answers 403 to the asking project.
    NotGranted,
}

impl InternalAccessReach {
    pub fn as_str(self) -> &'static str {
        match self {
            InternalAccessReach::SameProject => "same_project",
            InternalAccessReach::Granted => "granted",
            InternalAccessReach::SomeEnvironments => "some_environments",
            InternalAccessReach::NotGranted => "not_granted",
        }
    }
}

/// Who owns an internal name, and whether the asking project may call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalAccessVerdict {
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    pub target_environment_id: i32,
    pub target_environment_name: String,
    pub reach: InternalAccessReach,
}

/// `<environment>.<project>.temps.local`, the only internal-route form, as
/// `(environment slug, project slug)`.
pub fn internal_route_labels(name: &str) -> Option<(String, String)> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let labels = name.strip_suffix(".temps.local")?;
    let (environment, project) = labels.split_once('.')?;
    if environment.is_empty() || project.is_empty() || project.contains('.') {
        return None;
    }
    Some((environment.to_string(), project.to_string()))
}

/// The pure part: what `grants` (all on `target_project_id`) give
/// `caller_project_id` on `target_environment_id`.
pub fn reach(
    grants: &[InternalAccessGrantScope],
    target_project_id: i32,
    target_environment_id: i32,
    caller_project_id: i32,
) -> InternalAccessReach {
    if target_project_id == caller_project_id {
        return InternalAccessReach::SameProject;
    }
    let callers: Vec<InternalAccessCaller> =
        callers_for_route(grants, target_project_id, Some(target_environment_id))
            .into_iter()
            .filter(|caller| caller.project_id == caller_project_id)
            .collect();
    if callers.iter().any(|caller| caller.environment_id.is_none()) {
        InternalAccessReach::Granted
    } else if callers.is_empty() {
        InternalAccessReach::NotGranted
    } else {
        InternalAccessReach::SomeEnvironments
    }
}

/// The sandbox a test resolution is asked as: one the asking user owns.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct AskingSandbox {
    pub id: i32,
    pub public_id: String,
    /// `None`: the control plane.
    pub node_id: Option<i32>,
}

/// Sandbox `public_id` when `user_id` owns it and it is not destroyed.
pub async fn owned_sandbox<C: ConnectionTrait>(
    db: &C,
    public_id: &str,
    user_id: i32,
) -> Result<Option<AskingSandbox>, DbErr> {
    AskingSandbox::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id, public_id, node_id FROM sandboxes \
         WHERE public_id = $1 AND user_id = $2 AND status <> 'destroyed'",
        [public_id.into(), user_id.into()],
    ))
    .one(db)
    .await
}

/// The pure part for a sandbox: whether `grants` (all naming `sandbox`, on
/// `target_project_id`) cover `target_environment_id`.
pub fn sandbox_reach(
    grants: &[InternalAccessSandboxGrantScope],
    target_project_id: i32,
    target_environment_id: i32,
    sandbox: &str,
) -> InternalAccessReach {
    let admitted = sandboxes_for_route(grants, target_project_id, Some(target_environment_id));
    if admitted_sandbox(&admitted, sandbox).is_some() {
        InternalAccessReach::Granted
    } else {
        InternalAccessReach::NotGranted
    }
}

#[derive(Debug, FromQueryResult)]
struct SandboxGrantRow {
    target_environment_id: Option<i32>,
}

/// The verdict for `name` asked as `sandbox`, or `None` when the name is not
/// a live environment's internal route.
pub async fn sandbox_internal_access_verdict<C: ConnectionTrait>(
    db: &C,
    name: &str,
    sandbox: &AskingSandbox,
) -> Result<Option<InternalAccessVerdict>, DbErr> {
    let Some(target) = internal_route_target(db, name).await? else {
        return Ok(None);
    };
    let caller = InternalAccessSandboxCaller::from_public_id(&sandbox.public_id);
    let grants: Vec<InternalAccessSandboxGrantScope> =
        SandboxGrantRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT target_environment_id FROM internal_access_sandbox_grants \
             WHERE target_project_id = $1 AND sandbox_id = $2",
            [target.project_id.into(), sandbox.id.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|row| InternalAccessSandboxGrantScope {
            target_project_id: target.project_id,
            target_environment_id: row.target_environment_id,
            sandbox: caller.clone(),
        })
        .collect();
    Ok(Some(InternalAccessVerdict {
        reach: sandbox_reach(
            &grants,
            target.project_id,
            target.environment_id,
            &caller.name,
        ),
        target_project_id: target.project_id,
        target_project_name: target.project_name,
        target_project_slug: target.project_slug,
        target_environment_id: target.environment_id,
        target_environment_name: target.environment_name,
    }))
}

/// The live environment `name` is the internal route of: by its
/// `<environment>.<project>` labels, or as the environment whose subdomain is
/// the one label of `<subdomain>.temps.local` (ADR-105 D7), unless another
/// owner holds that name, exactly as the route table decides.
async fn internal_route_target<C: ConnectionTrait>(
    db: &C,
    name: &str,
) -> Result<Option<TargetRow>, DbErr> {
    let Some((environment_slug, project_slug)) = internal_route_labels(name) else {
        let Some(label) = temps_core::internal_access::environment_alias_label(name) else {
            return Ok(None);
        };
        return TargetRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT p.id AS project_id, p.name AS project_name, p.slug AS project_slug, \
                    e.id AS environment_id, e.name AS environment_name \
             FROM environments e JOIN projects p ON p.id = e.project_id \
             WHERE lower(btrim(e.subdomain)) = $1 AND NOT p.is_deleted \
               AND e.deleted_at IS NULL \
               AND NOT EXISTS (SELECT 1 FROM dns_name_claims c \
                               WHERE c.fqdn = $2 AND c.owner_kind <> $3) \
             LIMIT 1",
            [
                label.clone().into(),
                format!("{label}.{}", temps_core::internal_access::INTERNAL_ZONE).into(),
                temps_core::internal_access::ENVIRONMENT_ALIAS_OWNER_KIND.into(),
            ],
        ))
        .one(db)
        .await;
    };
    TargetRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT p.id AS project_id, p.name AS project_name, p.slug AS project_slug, \
                e.id AS environment_id, e.name AS environment_name \
         FROM environments e JOIN projects p ON p.id = e.project_id \
         WHERE p.slug = $1 AND e.slug = $2 AND NOT p.is_deleted AND e.deleted_at IS NULL \
         LIMIT 1",
        [project_slug.into(), environment_slug.into()],
    ))
    .one(db)
    .await
}

#[derive(Debug, FromQueryResult)]
struct TargetRow {
    project_id: i32,
    project_name: String,
    project_slug: String,
    environment_id: i32,
    environment_name: String,
}

#[derive(Debug, FromQueryResult)]
struct GrantRow {
    target_environment_id: Option<i32>,
    caller_environment_id: Option<i32>,
}

/// The verdict for `name` asked as `caller_project_id`, or `None` when the
/// name is not a live environment's internal route.
pub async fn internal_access_verdict<C: ConnectionTrait>(
    db: &C,
    name: &str,
    caller_project_id: i32,
) -> Result<Option<InternalAccessVerdict>, DbErr> {
    let Some(target) = internal_route_target(db, name).await? else {
        return Ok(None);
    };
    let grants = if target.project_id == caller_project_id {
        Vec::new()
    } else {
        GrantRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT target_environment_id, caller_environment_id \
             FROM internal_access_grants \
             WHERE target_project_id = $1 AND caller_project_id = $2",
            [target.project_id.into(), caller_project_id.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|row| InternalAccessGrantScope {
            target_project_id: target.project_id,
            target_environment_id: row.target_environment_id,
            caller: InternalAccessCaller {
                project_id: caller_project_id,
                environment_id: row.caller_environment_id,
            },
        })
        .collect()
    };
    Ok(Some(InternalAccessVerdict {
        reach: reach(
            &grants,
            target.project_id,
            target.environment_id,
            caller_project_id,
        ),
        target_project_id: target.project_id,
        target_project_name: target.project_name,
        target_project_slug: target.project_slug,
        target_environment_id: target.environment_id,
        target_environment_name: target.environment_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(target_env: Option<i32>, caller_env: Option<i32>) -> InternalAccessGrantScope {
        InternalAccessGrantScope {
            target_project_id: 1,
            target_environment_id: target_env,
            caller: InternalAccessCaller {
                project_id: 2,
                environment_id: caller_env,
            },
        }
    }

    #[test]
    fn only_environment_project_names_in_the_internal_zone_are_routes() {
        assert_eq!(
            internal_route_labels("Production.Orders-API.temps.local."),
            Some(("production".to_string(), "orders-api".to_string()))
        );
        assert_eq!(internal_route_labels("orders-api.temps.local"), None);
        assert_eq!(internal_route_labels("pg-0.pg.orders.temps.local"), None);
        assert_eq!(internal_route_labels("production.orders.example.com"), None);
        assert_eq!(internal_route_labels(".orders.temps.local"), None);
        // The one-label form is the subdomain alias, decided by its own rule.
        assert_eq!(
            temps_core::internal_access::environment_alias_label("orders-api.temps.local")
                .as_deref(),
            Some("orders-api")
        );
    }

    #[test]
    fn a_sandbox_is_granted_only_by_a_grant_naming_it_on_that_environment() {
        let grant = |environment: Option<i32>, name: &str| InternalAccessSandboxGrantScope {
            target_project_id: 1,
            target_environment_id: environment,
            sandbox: InternalAccessSandboxCaller {
                name: name.to_string(),
                public_id: format!("sbx_{name}"),
            },
        };
        assert_eq!(
            sandbox_reach(&[], 1, 10, "temps-sandbox-a"),
            InternalAccessReach::NotGranted
        );
        assert_eq!(
            sandbox_reach(&[grant(None, "temps-sandbox-a")], 1, 10, "temps-sandbox-a"),
            InternalAccessReach::Granted
        );
        assert_eq!(
            sandbox_reach(
                &[grant(Some(10), "temps-sandbox-a")],
                1,
                10,
                "temps-sandbox-a"
            ),
            InternalAccessReach::Granted
        );
        assert_eq!(
            sandbox_reach(
                &[grant(Some(11), "temps-sandbox-a")],
                1,
                10,
                "temps-sandbox-a"
            ),
            InternalAccessReach::NotGranted,
            "a grant on another environment"
        );
        assert_eq!(
            sandbox_reach(&[grant(None, "temps-sandbox-b")], 1, 10, "temps-sandbox-a"),
            InternalAccessReach::NotGranted,
            "another sandbox's grant"
        );
    }

    #[test]
    fn reach_follows_the_grants_scopes() {
        assert_eq!(reach(&[], 1, 10, 1), InternalAccessReach::SameProject);
        assert_eq!(reach(&[], 1, 10, 2), InternalAccessReach::NotGranted);
        assert_eq!(
            reach(&[grant(None, None)], 1, 10, 2),
            InternalAccessReach::Granted
        );
        assert_eq!(
            reach(&[grant(Some(10), Some(20))], 1, 10, 2),
            InternalAccessReach::SomeEnvironments
        );
        assert_eq!(
            reach(&[grant(Some(11), None)], 1, 10, 2),
            InternalAccessReach::NotGranted,
            "a grant on another environment of the target"
        );
    }

    /// Against the real schema: the test resolution asked as a sandbox finds
    /// the grant, though the sandbox's label (`sandboxes.name`) is nothing
    /// like the container the internal proxy recognises it by.
    #[tokio::test]
    async fn a_labelled_sandbox_is_granted_by_its_grant() {
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
        let owner = insert(
            "INSERT INTO users (name, email, email_verified, must_change_password, \
                 mfa_enabled, created_at, updated_at) \
             VALUES ('owner', 'owner@example.test', TRUE, FALSE, FALSE, NOW(), NOW()) \
             RETURNING id"
                .to_string(),
        )
        .await;
        let project = insert(
            "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, \
                 created_at, updated_at, slug, is_deleted, is_public_repo, attack_mode, \
                 enable_preview_environments, source_type) \
             VALUES ('Orders', 'r', 'o', '.', 'main', 'nextjs', NOW(), NOW(), 'orders', FALSE, \
                 TRUE, FALSE, FALSE, 'git') RETURNING id"
                .to_string(),
        )
        .await;
        insert(format!(
            "INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                 is_preview, created_at, updated_at) \
             VALUES ('production', 'production', 'orders-production', 'orders-production', \
                 '[]'::json, {project}, FALSE, NOW(), NOW()) RETURNING id"
        ))
        .await;
        let sandbox_row = insert(format!(
            "INSERT INTO sandboxes (public_id, name, status, work_dir, timeout_secs, user_id, \
                 created_at, last_activity_at, expires_at) \
             VALUES ('sbx_0a1b2c3d4e5f6789', 'agent scratchpad', 'running', '/workspace', 3600, \
                 {owner}, NOW(), NOW(), NOW() + INTERVAL '1 hour') RETURNING id"
        ))
        .await;

        let sandbox = owned_sandbox(c, "sbx_0a1b2c3d4e5f6789", owner)
            .await
            .expect("query")
            .expect("the owner sees the sandbox");
        // Both names of the environment, its two labels and its subdomain
        // alias (ADR-105 D7), get the same verdict.
        let verdict = |sandbox: AskingSandbox, name: &'static str| async move {
            sandbox_internal_access_verdict(c, name, &sandbox)
                .await
                .expect("verdict")
                .expect("a live route")
                .reach
        };
        let names = [
            "production.orders.temps.local",
            "Orders-Production.temps.local.",
        ];
        for name in names {
            assert_eq!(
                verdict(sandbox.clone(), name).await,
                InternalAccessReach::NotGranted,
                "{name}"
            );
        }

        insert(format!(
            "INSERT INTO internal_access_sandbox_grants (target_project_id, sandbox_id) \
             VALUES ({project}, {sandbox_row}) RETURNING id"
        ))
        .await;
        for name in names {
            assert_eq!(
                verdict(sandbox.clone(), name).await,
                InternalAccessReach::Granted,
                "{name}"
            );
        }

        // A managed service that holds the alias's name takes it: the name
        // is no longer this environment's route.
        c.execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "INSERT INTO dns_name_claims (fqdn, owner_kind, owner_id) \
             VALUES ('orders-production.temps.local', 'service_role', 3)",
        ))
        .await
        .expect("claim");
        assert!(
            sandbox_internal_access_verdict(c, "orders-production.temps.local", &sandbox)
                .await
                .expect("verdict")
                .is_none()
        );
        assert_eq!(
            verdict(sandbox, "production.orders.temps.local").await,
            InternalAccessReach::Granted
        );
    }
}
