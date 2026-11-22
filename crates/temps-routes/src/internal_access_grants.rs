// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants in the route snapshot (ADR 101 D5).
//!
//! Every snapshot reads the live grants once and attaches to each internal
//! route the callers from other projects it admits. Grants are bounded per
//! target project by the service that writes them, so this is O(routes +
//! grants) per snapshot and the per-route lists stay short.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, FromQueryResult, Statement};
use temps_core::internal_access::{
    callers_for_route, sandboxes_for_route, InternalAccessCaller, InternalAccessGrantScope,
    InternalAccessSandboxCaller, InternalAccessSandboxGrantScope,
};

use crate::route_sync::RouteEntryDto;

#[derive(Debug, FromQueryResult)]
struct GrantRow {
    target_project_id: i32,
    target_environment_id: Option<i32>,
    caller_project_id: i32,
    caller_environment_id: Option<i32>,
}

/// Every grant whose projects and environments are all live. The database
/// already removes a grant when either side is deleted (ADR 101 D4); the
/// joins keep a row a trigger has not reached yet out of every snapshot.
const LIVE_GRANTS_SQL: &str = "\
SELECT g.target_project_id, g.target_environment_id, \
       g.caller_project_id, g.caller_environment_id \
FROM internal_access_grants g \
JOIN projects tp ON tp.id = g.target_project_id AND NOT tp.is_deleted \
JOIN projects cp ON cp.id = g.caller_project_id AND NOT cp.is_deleted \
LEFT JOIN environments te ON te.id = g.target_environment_id \
LEFT JOIN environments ce ON ce.id = g.caller_environment_id \
WHERE (g.target_environment_id IS NULL \
       OR (te.deleted_at IS NULL AND te.project_id = g.target_project_id)) \
  AND (g.caller_environment_id IS NULL \
       OR (ce.deleted_at IS NULL AND ce.project_id = g.caller_project_id))";

/// Read every live grant.
pub async fn load_live_grants<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<InternalAccessGrantScope>, DbErr> {
    let rows = GrantRow::find_by_statement(Statement::from_string(
        DatabaseBackend::Postgres,
        LIVE_GRANTS_SQL,
    ))
    .all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| InternalAccessGrantScope {
            target_project_id: row.target_project_id,
            target_environment_id: row.target_environment_id,
            caller: InternalAccessCaller {
                project_id: row.caller_project_id,
                environment_id: row.caller_environment_id,
            },
        })
        .collect())
}

/// The node a snapshot is built for. Sandbox grants (ADR 105) are listed
/// only on the node their sandbox runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotNode {
    /// The control plane's own internal proxy (ADR-104), which serves the
    /// sandboxes it runs itself (`sandboxes.node_id IS NULL`).
    ControlPlane,
    /// Worker node `id`.
    Worker(i32),
}

impl SnapshotNode {
    fn node_id(self) -> Option<i32> {
        match self {
            SnapshotNode::ControlPlane => None,
            SnapshotNode::Worker(id) => Some(id),
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct SandboxGrantRow {
    target_project_id: i32,
    target_environment_id: Option<i32>,
    sandbox_public_id: String,
}

/// Every sandbox grant whose target is live and whose sandbox is not
/// destroyed and runs on the node `$1` names (`NULL`: the control plane).
/// The triggers already remove a grant whose side goes away (ADR 105); the
/// joins keep a row they have not reached yet out of every snapshot.
const LIVE_SANDBOX_GRANTS_SQL: &str = "\
SELECT g.target_project_id, g.target_environment_id, s.public_id AS sandbox_public_id \
FROM internal_access_sandbox_grants g \
JOIN sandboxes s ON s.id = g.sandbox_id AND s.status <> 'destroyed' \
JOIN projects tp ON tp.id = g.target_project_id AND NOT tp.is_deleted \
LEFT JOIN environments te ON te.id = g.target_environment_id \
WHERE s.node_id IS NOT DISTINCT FROM $1 \
  AND (g.target_environment_id IS NULL \
       OR (te.deleted_at IS NULL AND te.project_id = g.target_project_id))";

/// Read every live sandbox grant for the sandboxes `node` runs.
pub async fn load_live_sandbox_grants<C: ConnectionTrait>(
    db: &C,
    node: SnapshotNode,
) -> Result<Vec<InternalAccessSandboxGrantScope>, DbErr> {
    let rows = SandboxGrantRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        LIVE_SANDBOX_GRANTS_SQL,
        [node.node_id().into()],
    ))
    .all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| InternalAccessSandboxGrantScope {
            target_project_id: row.target_project_id,
            target_environment_id: row.target_environment_id,
            // The container comes from the public id; `sandboxes.name` is
            // the label the user gave the sandbox.
            sandbox: InternalAccessSandboxCaller::from_public_id(&row.sandbox_public_id),
        })
        .collect())
}

/// Fill `allowed_sandboxes` of every internal route from `grants`, as
/// [`attach_allowed_callers`] does for projects.
pub fn attach_allowed_sandboxes(
    routes: &mut [RouteEntryDto],
    grants: &[InternalAccessSandboxGrantScope],
) {
    if grants.is_empty() {
        return;
    }
    for route in routes {
        route.allowed_sandboxes = match route.project_id {
            Some(project_id) => sandboxes_for_route(grants, project_id, route.environment_id),
            None => Vec::new(),
        };
    }
}

/// Fill `allowed_callers` of every internal route from `grants`. A route
/// without a project admits nobody, grant or not, so it gets no list.
pub fn attach_allowed_callers(routes: &mut [RouteEntryDto], grants: &[InternalAccessGrantScope]) {
    if grants.is_empty() {
        return;
    }
    for route in routes {
        route.allowed_callers = match route.project_id {
            Some(project_id) => callers_for_route(grants, project_id, route.environment_id),
            None => Vec::new(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_sync::RouteBackendDto;
    use serde::Deserialize;

    fn route(host: &str, project_id: Option<i32>, environment_id: Option<i32>) -> RouteEntryDto {
        RouteEntryDto {
            host: host.to_string(),
            backends: vec![RouteBackendDto {
                address: "10.0.0.2:3000".to_string(),
                container_id: Some(format!("container-{host}")),
                container_name: None,
            }],
            deployment_id: Some(1),
            project_id,
            environment_id,
            gate: None,
            shield: None,
            allowed_callers: Vec::new(),
            allowed_sandboxes: Vec::new(),
        }
    }

    fn grant(target: (i32, Option<i32>), caller: (i32, Option<i32>)) -> InternalAccessGrantScope {
        InternalAccessGrantScope {
            target_project_id: target.0,
            target_environment_id: target.1,
            caller: InternalAccessCaller {
                project_id: caller.0,
                environment_id: caller.1,
            },
        }
    }

    #[test]
    fn each_route_gets_the_callers_its_grants_cover() {
        let mut routes = vec![
            route("production.api.temps.local", Some(1), Some(10)),
            route("staging.api.temps.local", Some(1), Some(11)),
            route("production.worker.temps.local", Some(2), Some(20)),
            route("legacy.temps.local", None, None),
        ];
        let grants = [
            grant((1, None), (2, Some(20))),
            grant((1, Some(11)), (3, None)),
        ];
        attach_allowed_callers(&mut routes, &grants);
        assert_eq!(
            routes[0].allowed_callers,
            vec![InternalAccessCaller {
                project_id: 2,
                environment_id: Some(20)
            }]
        );
        assert_eq!(routes[1].allowed_callers.len(), 2);
        assert!(
            routes[2].allowed_callers.is_empty(),
            "grants are directional"
        );
        assert!(routes[3].allowed_callers.is_empty());
    }

    /// The pre-ADR-101 agent's route shape (`temps-node-agent`'s
    /// `SnapshotRoute` before the field existed): no `allowed_callers`, and
    /// unknown fields ignored, as serde does by default.
    #[derive(Debug, Deserialize)]
    struct PreGrantAgentRoute {
        host: String,
        #[serde(default)]
        project_id: Option<i32>,
    }

    /// New plane → old agent: the field parses away, so the old agent keeps
    /// admitting its route's own project only.
    #[test]
    fn an_agent_that_predates_grants_parses_a_snapshot_with_callers() {
        let mut routes = vec![route("production.api.temps.local", Some(1), Some(10))];
        attach_allowed_callers(&mut routes, &[grant((1, None), (2, None))]);
        let json = serde_json::to_string(&routes).expect("serializable");
        assert!(json.contains("allowed_callers"));
        let parsed: Vec<PreGrantAgentRoute> =
            serde_json::from_str(&json).expect("an older agent still parses the snapshot");
        assert_eq!(parsed[0].host, "production.api.temps.local");
        assert_eq!(parsed[0].project_id, Some(1));
    }

    /// Old plane → new agent: no field is an empty list.
    #[test]
    fn a_snapshot_from_a_plane_that_predates_grants_has_no_callers() {
        let json = r#"{"host":"production.api.temps.local","backends":[],
            "deployment_id":1,"project_id":1,"environment_id":10}"#;
        let parsed: RouteEntryDto = serde_json::from_str(json).expect("parses");
        assert!(parsed.allowed_callers.is_empty());
    }

    /// An install without grants sends exactly what it sent before.
    #[test]
    fn without_grants_the_wire_form_is_unchanged() {
        let mut routes = vec![route("production.api.temps.local", Some(1), Some(10))];
        attach_allowed_callers(&mut routes, &[]);
        let json = serde_json::to_string(&routes[0]).expect("serializable");
        assert!(!json.contains("allowed_callers"), "{json}");
    }

    /// Against the real schema: live grants load with their scopes, and a row
    /// naming an environment of the wrong project, or a soft-deleted one, never
    /// reaches a route.
    #[tokio::test]
    async fn live_grants_load_from_the_database() -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::ConnectionTrait;
        use temps_test_utils::TestDatabase;

        let test_database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("skipping live grants test: test database unavailable: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let ops = crate::test_utils::TestDBMockOperations::new(test_database.db.clone()).await?;
        let (target, target_env, _) = ops
            .create_test_project_with_domain("grants-target.example.test")
            .await?;
        let (caller, caller_env, _) = ops
            .create_test_project_with_domain("grants-caller.example.test")
            .await?;
        let db = test_database.db.as_ref();
        let insert = |target_env: Option<i32>, caller_env: Option<i32>| {
            format!(
                "INSERT INTO internal_access_grants \
                 (target_project_id, target_environment_id, caller_project_id, caller_environment_id) \
                 VALUES ({}, {}, {}, {})",
                target.id,
                target_env.map_or("NULL".to_string(), |id| id.to_string()),
                caller.id,
                caller_env.map_or("NULL".to_string(), |id| id.to_string()),
            )
        };
        db.execute_unprepared(&insert(None, None)).await?;
        db.execute_unprepared(&insert(Some(target_env.id), Some(caller_env.id)))
            .await?;
        // The caller's environment named as the target's: never live.
        db.execute_unprepared(&insert(Some(caller_env.id), None))
            .await?;

        let mut grants = load_live_grants(db).await?;
        grants.sort_by_key(|grant| (grant.target_environment_id, grant.caller.environment_id));
        assert_eq!(
            grants,
            vec![
                grant((target.id, None), (caller.id, None)),
                grant(
                    (target.id, Some(target_env.id)),
                    (caller.id, Some(caller_env.id))
                ),
            ]
        );

        let mut routes = vec![route(
            "production.target.temps.local",
            Some(target.id),
            Some(target_env.id),
        )];
        attach_allowed_callers(&mut routes, &grants);
        assert_eq!(
            routes[0].allowed_callers,
            vec![
                InternalAccessCaller {
                    project_id: caller.id,
                    environment_id: None,
                },
                InternalAccessCaller {
                    project_id: caller.id,
                    environment_id: Some(caller_env.id),
                },
            ]
        );

        db.execute_unprepared(&format!(
            "UPDATE environments SET deleted_at = now() WHERE id = {}",
            caller_env.id
        ))
        .await?;
        assert_eq!(
            load_live_grants(db).await?,
            vec![grant((target.id, None), (caller.id, None))],
            "the soft-deleted caller environment's grant is gone"
        );
        Ok(())
    }

    fn sandbox_grant(target: (i32, Option<i32>), name: &str) -> InternalAccessSandboxGrantScope {
        InternalAccessSandboxGrantScope {
            target_project_id: target.0,
            target_environment_id: target.1,
            sandbox: InternalAccessSandboxCaller {
                name: name.to_string(),
                public_id: format!("sbx_{name}"),
            },
        }
    }

    #[test]
    fn each_route_gets_the_sandboxes_its_grants_cover() {
        let mut routes = vec![
            route("production.api.temps.local", Some(1), Some(10)),
            route("staging.api.temps.local", Some(1), Some(11)),
            route("legacy.temps.local", None, None),
        ];
        attach_allowed_sandboxes(
            &mut routes,
            &[
                sandbox_grant((1, None), "temps-sandbox-a"),
                sandbox_grant((1, Some(11)), "temps-sandbox-b"),
            ],
        );
        let names = |route: &RouteEntryDto| {
            route
                .allowed_sandboxes
                .iter()
                .map(|sandbox| sandbox.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&routes[0]), ["temps-sandbox-a"]);
        assert_eq!(names(&routes[1]), ["temps-sandbox-a", "temps-sandbox-b"]);
        assert!(
            routes[2].allowed_sandboxes.is_empty(),
            "no project, no list"
        );
        assert!(
            routes.iter().all(|route| route.allowed_callers.is_empty()),
            "a sandbox grant is not a project caller"
        );
    }

    /// An agent that predates sandbox grants reads `allowed_callers` as a list
    /// of project callers. Sandboxes travel in their own field, so its
    /// snapshot still parses and it admits no sandbox.
    #[test]
    fn an_agent_that_predates_sandbox_grants_parses_the_snapshot_and_admits_none() {
        #[derive(Debug, Deserialize)]
        struct PreSandboxAgentRoute {
            host: String,
            #[serde(default)]
            allowed_callers: Vec<InternalAccessCaller>,
        }
        let mut routes = vec![route("production.api.temps.local", Some(1), Some(10))];
        attach_allowed_callers(&mut routes, &[grant((1, None), (2, None))]);
        attach_allowed_sandboxes(&mut routes, &[sandbox_grant((1, None), "temps-sandbox-a")]);
        let json = serde_json::to_string(&routes).expect("serializable");
        assert!(json.contains("allowed_sandboxes"), "{json}");
        let parsed: Vec<PreSandboxAgentRoute> =
            serde_json::from_str(&json).expect("an older agent still parses the snapshot");
        assert_eq!(parsed[0].host, "production.api.temps.local");
        assert_eq!(
            parsed[0].allowed_callers.len(),
            1,
            "only the project caller"
        );
    }

    /// Against the real schema: a sandbox grant reaches only the snapshot of
    /// the node its sandbox runs on, and a destroyed sandbox's never does. The
    /// snapshot names the sandbox by the container its public id runs as,
    /// never by the label the user gave it (`sandboxes.name`).
    #[tokio::test]
    async fn sandbox_grants_load_for_the_node_their_sandbox_runs_on(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::ConnectionTrait;
        use temps_test_utils::TestDatabase;

        let test_database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("skipping sandbox grants test: test database unavailable: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let ops = crate::test_utils::TestDBMockOperations::new(test_database.db.clone()).await?;
        let (target, target_env, _) = ops
            .create_test_project_with_domain("sandbox-grants-target.example.test")
            .await?;
        let db = test_database.db.as_ref();
        db.execute_unprepared(
            "INSERT INTO sandboxes (public_id, name, status, work_dir, timeout_secs) VALUES \
             ('sbx_0a1b2c3d4e5f6789', 'agent scratchpad', 'running', '/workspace', 600), \
             ('sbx_f0e1d2c3b4a59687', 'old experiment', 'running', '/workspace', 600)",
        )
        .await?;
        db.execute_unprepared(&format!(
            "INSERT INTO internal_access_sandbox_grants (target_project_id, target_environment_id, sandbox_id) \
             SELECT {target}, NULL, id FROM sandboxes WHERE public_id = 'sbx_0a1b2c3d4e5f6789' \
             UNION ALL SELECT {target}, {env}, id FROM sandboxes WHERE public_id = 'sbx_f0e1d2c3b4a59687'",
            target = target.id,
            env = target_env.id,
        ))
        .await?;

        let local = load_live_sandbox_grants(db, SnapshotNode::ControlPlane).await?;
        assert_eq!(local.len(), 2, "{local:?}");
        assert!(
            load_live_sandbox_grants(db, SnapshotNode::Worker(9))
                .await?
                .is_empty(),
            "a worker's snapshot lists none of the control plane's sandboxes"
        );

        db.execute_unprepared(
            "UPDATE sandboxes SET status = 'destroyed' WHERE public_id = 'sbx_f0e1d2c3b4a59687'",
        )
        .await?;
        let local = load_live_sandbox_grants(db, SnapshotNode::ControlPlane).await?;
        assert_eq!(
            local,
            vec![InternalAccessSandboxGrantScope {
                target_project_id: target.id,
                target_environment_id: None,
                sandbox: InternalAccessSandboxCaller {
                    name: "temps-sandbox-0a1b2c3d4e5f6789".to_string(),
                    public_id: "sbx_0a1b2c3d4e5f6789".to_string(),
                },
            }],
            "the destroyed sandbox's grant is gone"
        );
        Ok(())
    }
}
