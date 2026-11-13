// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who may call an internal route (ADR 101 D6).
//!
//! The caller is the container that owns the request's source address
//! (resolved by the internal proxy through the local Docker daemon). What it
//! serves comes from the control plane's route snapshot. A caller is admitted
//! when it serves the route's own project, or when one of the route's
//! `allowed_callers` (internal access grants on the route's project) names
//! its project, and its environment if the grant names one. Either way Docker
//! must then confirm the container is still running at that address.
//!
//! A sandbox reaches this proxy only through its egress proxy, so a request
//! whose source is a sandbox's egress proxy is that sandbox's (ADR 105). It is
//! admitted only when the route's `allowed_sandboxes` names it; project
//! grants and the same-project rule never apply to it.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use crate::route_store::{RouteEntry, RouteStore, WorkloadIdentity};

/// The 403 body for a refused caller. It says what fixes it without naming
/// any project: the caller may not be allowed to know the target's name.
pub(crate) const CROSS_PROJECT_DENIED: &str =
    "internal route is not accessible from this workload. Calls from another project need an \
     internal access grant on the project that owns this address: open that project's \
     Settings > Protection > Internal access and allow the calling project (or one of its \
     environments)";

/// The 403 body for a sandbox no grant admits. Like
/// [`CROSS_PROJECT_DENIED`] it names no project; it names the sandbox, which
/// is what the owner of the address adds.
///
/// `container` is the sandbox's container name. The console and API know a
/// sandbox by its public id, and a refused sandbox is in no snapshot to look
/// that up from, so the id is read off the name: a sandbox created through
/// the sandboxes API runs as `temps-sandbox-<hex>` with public id
/// `sbx_<hex>`. Any other name (an agent run's) is shown as it is.
pub(crate) fn sandbox_denied(container: &str) -> String {
    let sandbox = temps_core::internal_access::sandbox_public_id_for_container(container)
        .unwrap_or_else(|| container.to_string());
    format!(
        "internal route is not accessible from sandbox {sandbox}. A sandbox needs an internal \
         access grant on the project that owns this address: open that project's Settings > \
         Protection > Internal access and add sandbox {sandbox} as a caller"
    )
}

/// Why a caller was refused, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The route carries no project (a legacy or malformed snapshot).
    RouteWithoutProject,
    /// No container owns the source address, or the snapshot does not know
    /// the container.
    UnknownCaller,
    /// The caller serves another project, and no grant on the route admits it.
    NotGranted,
    /// The caller is a sandbox, and no sandbox grant on the route admits it.
    SandboxNotGranted,
    /// Docker no longer confirms the container at that address, or the
    /// inspection budget is exhausted.
    StaleCaller,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::RouteWithoutProject => "route_without_project",
            DenyReason::UnknownCaller => "unknown_caller",
            DenyReason::NotGranted => "not_granted",
            DenyReason::SandboxNotGranted => "sandbox_not_granted",
            DenyReason::StaleCaller => "stale_caller",
        }
    }
}

/// The outcome for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    SameProject {
        caller_project_id: i32,
    },
    Granted {
        caller_project_id: i32,
        caller_environment_id: Option<i32>,
    },
    /// A sandbox a sandbox grant on the route admits (ADR 105).
    SandboxGranted,
    Denied(DenyReason),
}

/// Whose inspection budget the Docker re-check of an admitted caller is
/// charged to: the caller's own, never the target's.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Recheck {
    Project(i32),
    Sandbox(Arc<str>),
}

impl AccessDecision {
    /// The project whose inspection budget the re-check is charged to: the
    /// caller's own, never the target's.
    fn caller_project_id(self) -> Option<i32> {
        match self {
            AccessDecision::SameProject { caller_project_id }
            | AccessDecision::Granted {
                caller_project_id, ..
            } => Some(caller_project_id),
            AccessDecision::SandboxGranted | AccessDecision::Denied(_) => None,
        }
    }
}

/// The in-memory decision: no I/O. Same project first, then grants, each
/// over the caller's identities in a fixed order so the budget a request is
/// charged to does not depend on hash order.
///
/// Public so the control plane, which serves its own containers with this
/// proxy (ADR-104), tests its routes against the same decision.
pub fn decide(
    route: &RouteEntry,
    identities: Option<&HashSet<WorkloadIdentity>>,
) -> AccessDecision {
    let Some(route_project_id) = route.project_id else {
        return AccessDecision::Denied(DenyReason::RouteWithoutProject);
    };
    let Some(identities) = identities.filter(|identities| !identities.is_empty()) else {
        return AccessDecision::Denied(DenyReason::UnknownCaller);
    };
    if identities
        .iter()
        .any(|identity| identity.project_id == route_project_id)
    {
        return AccessDecision::SameProject {
            caller_project_id: route_project_id,
        };
    }
    if route.allowed_callers.is_empty() {
        return AccessDecision::Denied(DenyReason::NotGranted);
    }
    identities
        .iter()
        .filter(|identity| {
            temps_core::internal_access::permits(
                &route.allowed_callers,
                identity.project_id,
                identity.environment_id,
            )
        })
        .min()
        .map(|identity| AccessDecision::Granted {
            caller_project_id: identity.project_id,
            caller_environment_id: identity.environment_id,
        })
        .unwrap_or(AccessDecision::Denied(DenyReason::NotGranted))
}

/// The in-memory decision for a request from sandbox `sandbox` (the
/// container name its egress proxy is named after): only the route's sandbox
/// grants apply.
pub fn decide_sandbox(route: &RouteEntry, sandbox: &str) -> AccessDecision {
    if route.project_id.is_none() {
        return AccessDecision::Denied(DenyReason::RouteWithoutProject);
    }
    match temps_core::internal_access::admitted_sandbox(&route.allowed_sandboxes, sandbox) {
        Some(_) => AccessDecision::SandboxGranted,
        None => AccessDecision::Denied(DenyReason::SandboxNotGranted),
    }
}

/// The whole check for one request: the in-memory decision, then
/// `revalidate` (Docker inspection behind the caller's own token bucket) for
/// an allow. A refused caller never reaches `revalidate`, so it cannot spend
/// Docker capacity.
///
/// `sandbox` is the sandbox whose egress proxy owns the source address, when
/// it is one; such a caller is decided by [`decide_sandbox`] alone.
pub(crate) async fn authorize<F, Fut>(
    store: &RouteStore,
    container_id: Option<&str>,
    sandbox: Option<&Arc<str>>,
    route: &RouteEntry,
    revalidate: F,
) -> AccessDecision
where
    F: FnOnce(Recheck) -> Fut,
    Fut: Future<Output = bool>,
{
    let (decision, recheck) = match (sandbox, container_id) {
        (Some(sandbox), Some(_)) => {
            let decision = decide_sandbox(route, sandbox);
            let recheck = matches!(decision, AccessDecision::SandboxGranted)
                .then(|| Recheck::Sandbox(Arc::clone(sandbox)));
            (decision, recheck)
        }
        (_, container_id) => {
            let decision = match container_id {
                Some(container_id) => store
                    .with_container_identities(container_id, |identities| {
                        decide(route, Some(identities))
                    })
                    .unwrap_or_else(|| decide(route, None)),
                None => decide(route, None),
            };
            let recheck = decision.caller_project_id().map(Recheck::Project);
            (decision, recheck)
        }
    };
    let Some(recheck) = recheck else {
        return decision;
    };
    if revalidate(recheck).await {
        decision
    } else {
        AccessDecision::Denied(DenyReason::StaleCaller)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_store::RouteBackend;
    use std::sync::atomic::{AtomicI32, Ordering};
    use tempfile::TempDir;
    use temps_core::internal_access::InternalAccessCaller;

    const TARGET_PROJECT: i32 = 42;
    const TARGET_ENV: i32 = 420;
    const CALLER_PROJECT: i32 = 7;
    const CALLER_ENV: i32 = 70;
    const OTHER_CALLER_ENV: i32 = 71;

    fn route(
        host: &str,
        project_id: i32,
        environment_id: i32,
        container_id: &str,
        allowed_callers: Vec<InternalAccessCaller>,
    ) -> RouteEntry {
        RouteEntry {
            host: host.into(),
            backends: vec![RouteBackend {
                address: "172.20.0.2:3000".into(),
                container_id: Some(container_id.into()),
                container_name: None,
            }],
            deployment_id: Some(1),
            project_id: Some(project_id),
            environment_id: Some(environment_id),
            gate: None,
            shield: None,
            allowed_callers,
            allowed_sandboxes: Vec::new(),
        }
    }

    fn target(allowed_callers: Vec<InternalAccessCaller>) -> RouteEntry {
        route(
            "production.api.temps.local",
            TARGET_PROJECT,
            TARGET_ENV,
            "container-target",
            allowed_callers,
        )
    }

    /// The caller's own routes: one per environment of the caller project.
    fn caller_routes() -> Vec<RouteEntry> {
        vec![
            route(
                "production.worker.temps.local",
                CALLER_PROJECT,
                CALLER_ENV,
                "container-caller-production",
                Vec::new(),
            ),
            route(
                "staging.worker.temps.local",
                CALLER_PROJECT,
                OTHER_CALLER_ENV,
                "container-caller-staging",
                Vec::new(),
            ),
            route(
                "staging.api.temps.local",
                TARGET_PROJECT,
                421,
                "container-target-staging",
                Vec::new(),
            ),
        ]
    }

    fn store_with(target_route: RouteEntry) -> (TempDir, RouteStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = RouteStore::new(dir.path().join("routes.json"));
        let mut routes = caller_routes();
        routes.push(target_route);
        store.apply_snapshot(1, routes);
        (dir, store)
    }

    async fn check(store: &RouteStore, container_id: Option<&str>, host: &str) -> AccessDecision {
        let route = store.lookup(host).expect("the route is in the store");
        authorize(store, container_id, None, &route, |_| async { true }).await
    }

    fn project_wide(project_id: i32) -> InternalAccessCaller {
        InternalAccessCaller {
            project_id,
            environment_id: None,
        }
    }

    #[tokio::test]
    async fn another_environment_of_the_same_project_is_admitted_without_a_grant() {
        let (_dir, store) = store_with(target(Vec::new()));
        assert_eq!(
            check(
                &store,
                Some("container-target-staging"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::SameProject {
                caller_project_id: TARGET_PROJECT
            }
        );
    }

    #[tokio::test]
    async fn another_project_is_refused_without_a_grant() {
        let (_dir, store) = store_with(target(Vec::new()));
        assert_eq!(
            check(
                &store,
                Some("container-caller-production"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Denied(DenyReason::NotGranted)
        );
    }

    #[tokio::test]
    async fn a_project_wide_grant_admits_every_environment_of_the_caller() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        for (container, environment) in [
            ("container-caller-production", CALLER_ENV),
            ("container-caller-staging", OTHER_CALLER_ENV),
        ] {
            assert_eq!(
                check(&store, Some(container), "production.api.temps.local").await,
                AccessDecision::Granted {
                    caller_project_id: CALLER_PROJECT,
                    caller_environment_id: Some(environment),
                },
                "{container}"
            );
        }
    }

    #[tokio::test]
    async fn an_environment_grant_admits_that_environment_only() {
        let (_dir, store) = store_with(target(vec![InternalAccessCaller {
            project_id: CALLER_PROJECT,
            environment_id: Some(CALLER_ENV),
        }]));
        assert_eq!(
            check(
                &store,
                Some("container-caller-production"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Granted {
                caller_project_id: CALLER_PROJECT,
                caller_environment_id: Some(CALLER_ENV),
            }
        );
        assert_eq!(
            check(
                &store,
                Some("container-caller-staging"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Denied(DenyReason::NotGranted),
            "the wrong environment of the granted project"
        );
    }

    #[tokio::test]
    async fn a_grant_for_another_project_admits_nobody_else() {
        let (_dir, store) = store_with(target(vec![project_wide(99)]));
        assert_eq!(
            check(
                &store,
                Some("container-caller-production"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Denied(DenyReason::NotGranted)
        );
    }

    #[tokio::test]
    async fn a_revoked_grant_stops_admitting_on_the_next_snapshot() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        assert!(matches!(
            check(
                &store,
                Some("container-caller-production"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Granted { .. }
        ));

        let mut routes = caller_routes();
        routes.push(target(Vec::new()));
        store.apply_snapshot(2, routes);
        assert_eq!(
            check(
                &store,
                Some("container-caller-production"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Denied(DenyReason::NotGranted)
        );
    }

    #[tokio::test]
    async fn an_unknown_container_is_refused_grant_or_not() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        assert_eq!(
            check(
                &store,
                Some("container-unknown"),
                "production.api.temps.local"
            )
            .await,
            AccessDecision::Denied(DenyReason::UnknownCaller)
        );
        assert_eq!(
            check(&store, None, "production.api.temps.local").await,
            AccessDecision::Denied(DenyReason::UnknownCaller),
            "no container owns the source address"
        );
    }

    #[tokio::test]
    async fn a_stale_container_is_refused_even_when_granted() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        let route = store
            .lookup("production.api.temps.local")
            .expect("the route is in the store");
        let decision = authorize(
            &store,
            Some("container-caller-production"),
            None,
            &route,
            |_| async { false },
        )
        .await;
        assert_eq!(decision, AccessDecision::Denied(DenyReason::StaleCaller));

        // The same project's caller is re-checked the same way.
        let decision = authorize(
            &store,
            Some("container-target-staging"),
            None,
            &route,
            |_| async { false },
        )
        .await;
        assert_eq!(decision, AccessDecision::Denied(DenyReason::StaleCaller));
    }

    #[tokio::test]
    async fn the_recheck_is_charged_to_the_callers_project_and_skipped_on_denial() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        let route = store
            .lookup("production.api.temps.local")
            .expect("the route is in the store");
        let charged = AtomicI32::new(0);
        authorize(
            &store,
            Some("container-caller-production"),
            None,
            &route,
            |recheck| {
                if let Recheck::Project(project) = recheck {
                    charged.store(project, Ordering::SeqCst);
                }
                async { true }
            },
        )
        .await;
        assert_eq!(charged.load(Ordering::SeqCst), CALLER_PROJECT);

        let refused = target(Vec::new());
        let calls = AtomicI32::new(0);
        let decision = authorize(
            &store,
            Some("container-caller-production"),
            None,
            &refused,
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { true }
            },
        )
        .await;
        assert_eq!(decision, AccessDecision::Denied(DenyReason::NotGranted));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a refused caller costs no inspection"
        );
    }

    #[test]
    fn a_route_without_a_project_admits_nobody() {
        let mut route = target(vec![project_wide(CALLER_PROJECT)]);
        route.project_id = None;
        let identities = HashSet::from([WorkloadIdentity {
            project_id: CALLER_PROJECT,
            environment_id: Some(CALLER_ENV),
        }]);
        assert_eq!(
            decide(&route, Some(&identities)),
            AccessDecision::Denied(DenyReason::RouteWithoutProject)
        );
    }

    #[test]
    fn a_container_whose_route_has_no_environment_matches_only_project_wide_grants() {
        let identities = HashSet::from([WorkloadIdentity {
            project_id: CALLER_PROJECT,
            environment_id: None,
        }]);
        let env_grant = target(vec![InternalAccessCaller {
            project_id: CALLER_PROJECT,
            environment_id: Some(CALLER_ENV),
        }]);
        assert_eq!(
            decide(&env_grant, Some(&identities)),
            AccessDecision::Denied(DenyReason::NotGranted)
        );
        let wide = target(vec![project_wide(CALLER_PROJECT)]);
        assert!(matches!(
            decide(&wide, Some(&identities)),
            AccessDecision::Granted { .. }
        ));
    }

    #[test]
    fn the_refusal_names_no_project() {
        assert!(CROSS_PROJECT_DENIED.contains("internal access grant"));
        assert!(CROSS_PROJECT_DENIED.contains("Internal access"));
        assert!(!CROSS_PROJECT_DENIED.chars().any(|c| c.is_ascii_digit()));
    }

    const SANDBOX: &str = "temps-sandbox-alpha";

    fn sandbox_grant(name: &str) -> temps_core::internal_access::InternalAccessSandboxCaller {
        temps_core::internal_access::InternalAccessSandboxCaller {
            name: name.to_string(),
            public_id: format!("sbx_{name}"),
        }
    }

    fn with_sandboxes(names: &[&str]) -> RouteEntry {
        let mut route = target(Vec::new());
        route.allowed_sandboxes = names.iter().map(|name| sandbox_grant(name)).collect();
        route
    }

    async fn check_sandbox(
        store: &RouteStore,
        route: &RouteEntry,
        sandbox: &str,
        current: bool,
    ) -> AccessDecision {
        let sandbox: Arc<str> = Arc::from(sandbox);
        authorize(
            store,
            Some("container-egress-proxy"),
            Some(&sandbox),
            route,
            |_| async move { current },
        )
        .await
    }

    #[tokio::test]
    async fn a_sandbox_without_a_grant_is_refused() {
        let (_dir, store) = store_with(target(vec![project_wide(CALLER_PROJECT)]));
        assert_eq!(
            check_sandbox(
                &store,
                &target(vec![project_wide(CALLER_PROJECT)]),
                SANDBOX,
                true
            )
            .await,
            AccessDecision::Denied(DenyReason::SandboxNotGranted),
            "a project grant does not admit a sandbox"
        );
    }

    #[tokio::test]
    async fn a_granted_sandbox_is_admitted_and_rechecked_on_its_own_budget() {
        let (_dir, store) = store_with(with_sandboxes(&[SANDBOX]));
        let route = with_sandboxes(&[SANDBOX]);
        let charged = std::sync::Mutex::new(None);
        let sandbox: Arc<str> = Arc::from(SANDBOX);
        let decision = authorize(
            &store,
            Some("container-egress-proxy"),
            Some(&sandbox),
            &route,
            |recheck| {
                *charged.lock().expect("not poisoned") = Some(recheck);
                async { true }
            },
        )
        .await;
        assert_eq!(decision, AccessDecision::SandboxGranted);
        assert_eq!(
            charged.into_inner().expect("not poisoned"),
            Some(Recheck::Sandbox(Arc::from(SANDBOX)))
        );
    }

    #[tokio::test]
    async fn a_grant_for_another_sandbox_admits_nobody_else() {
        let (_dir, store) = store_with(with_sandboxes(&["temps-sandbox-beta"]));
        assert_eq!(
            check_sandbox(
                &store,
                &with_sandboxes(&["temps-sandbox-beta"]),
                SANDBOX,
                true
            )
            .await,
            AccessDecision::Denied(DenyReason::SandboxNotGranted)
        );
    }

    /// A revoked grant, or a destroyed sandbox, leaves the next snapshot's
    /// list, and the next request is refused.
    #[tokio::test]
    async fn a_revoked_or_destroyed_sandbox_is_refused_on_the_next_snapshot() {
        let (_dir, store) = store_with(with_sandboxes(&[SANDBOX]));
        let host = "production.api.temps.local";
        let granted = store.lookup(host).expect("the route is in the store");
        assert_eq!(
            check_sandbox(&store, &granted, SANDBOX, true).await,
            AccessDecision::SandboxGranted
        );
        let mut routes = caller_routes();
        routes.push(with_sandboxes(&[]));
        store.apply_snapshot(2, routes);
        let revoked = store.lookup(host).expect("the route is in the store");
        assert_eq!(
            check_sandbox(&store, &revoked, SANDBOX, true).await,
            AccessDecision::Denied(DenyReason::SandboxNotGranted)
        );
    }

    /// A granted sandbox whose egress proxy Docker no longer confirms (gone,
    /// replaced, or its address reused) is refused.
    #[tokio::test]
    async fn a_granted_sandbox_whose_proxy_is_not_current_is_refused() {
        let (_dir, store) = store_with(with_sandboxes(&[SANDBOX]));
        assert_eq!(
            check_sandbox(&store, &with_sandboxes(&[SANDBOX]), SANDBOX, false).await,
            AccessDecision::Denied(DenyReason::StaleCaller)
        );
    }

    /// The sandbox path never falls back to project identities: an egress
    /// proxy container that a route also lists as a backend of the target
    /// project is still only its sandbox.
    #[tokio::test]
    async fn a_sandbox_never_passes_as_the_routes_own_project() {
        let mut target_route = with_sandboxes(&[]);
        target_route.backends[0].container_id = Some("container-egress-proxy".into());
        let (_dir, store) = store_with(target_route.clone());
        assert_eq!(
            check_sandbox(&store, &target_route, SANDBOX, true).await,
            AccessDecision::Denied(DenyReason::SandboxNotGranted)
        );
    }

    #[test]
    fn a_sandbox_refusal_names_the_sandbox_and_where_to_add_it() {
        let body = sandbox_denied(SANDBOX);
        assert!(body.contains(SANDBOX));
        assert!(body.contains("Internal access"));
        assert!(body.contains("internal access grant"));
    }

    /// The console's sandbox picker lists public ids, so a sandbox from the
    /// sandboxes API is named by its id, not its container.
    #[test]
    fn a_sandbox_refusal_names_the_public_id_the_console_lists() {
        let body = sandbox_denied("temps-sandbox-0a1b2c3d4e5f6789");
        assert!(
            body.contains("add sandbox sbx_0a1b2c3d4e5f6789 as a caller"),
            "{body}"
        );
        assert!(!body.contains("temps-sandbox-"), "{body}");

        // An agent run's container, or anything not shaped like a public id,
        // is shown as it is rather than as an id that does not exist.
        for container in [
            "temps-sandbox-1000000",
            "temps-sandbox-0A1B2C3D4E5F6789",
            "temps-sandbox-0a1b2c3d4e5f67890",
            "other-0a1b2c3d4e5f6789",
        ] {
            let body = sandbox_denied(container);
            assert!(
                body.contains(&format!("add sandbox {container} as a caller")),
                "{body}"
            );
            assert!(!body.contains("sbx_"), "{body}");
        }
    }
}
