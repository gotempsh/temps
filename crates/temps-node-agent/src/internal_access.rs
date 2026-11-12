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

use std::collections::HashSet;
use std::future::Future;

use crate::route_store::{RouteEntry, RouteStore, WorkloadIdentity};

/// The 403 body for a refused caller. It says what fixes it without naming
/// any project: the caller may not be allowed to know the target's name.
pub(crate) const CROSS_PROJECT_DENIED: &str =
    "internal route is not accessible from this workload. Calls from another project need an \
     internal access grant on the project that owns this address: open that project's \
     Settings > Protection > Internal access and allow the calling project (or one of its \
     environments)";

/// Why a caller was refused, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DenyReason {
    /// The route carries no project (a legacy or malformed snapshot).
    RouteWithoutProject,
    /// No container owns the source address, or the snapshot does not know
    /// the container.
    UnknownCaller,
    /// The caller serves another project, and no grant on the route admits it.
    NotGranted,
    /// Docker no longer confirms the container at that address, or the
    /// inspection budget is exhausted.
    StaleCaller,
}

impl DenyReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DenyReason::RouteWithoutProject => "route_without_project",
            DenyReason::UnknownCaller => "unknown_caller",
            DenyReason::NotGranted => "not_granted",
            DenyReason::StaleCaller => "stale_caller",
        }
    }
}

/// The outcome for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessDecision {
    SameProject {
        caller_project_id: i32,
    },
    Granted {
        caller_project_id: i32,
        caller_environment_id: Option<i32>,
    },
    Denied(DenyReason),
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
            AccessDecision::Denied(_) => None,
        }
    }
}

/// The in-memory decision: no I/O. Same project first, then grants, each
/// over the caller's identities in a fixed order so the budget a request is
/// charged to does not depend on hash order.
pub(crate) fn decide(
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

/// The whole check for one request: the in-memory decision, then
/// `revalidate(container_id, caller_project_id)` (Docker inspection behind
/// the caller project's token bucket) for an allow. A refused caller never
/// reaches `revalidate`, so it cannot spend Docker capacity.
pub(crate) async fn authorize<F, Fut>(
    store: &RouteStore,
    container_id: Option<&str>,
    route: &RouteEntry,
    revalidate: F,
) -> AccessDecision
where
    F: FnOnce(i32) -> Fut,
    Fut: Future<Output = bool>,
{
    let decision = match container_id {
        Some(container_id) => store
            .with_container_identities(container_id, |identities| decide(route, Some(identities)))
            .unwrap_or_else(|| decide(route, None)),
        None => decide(route, None),
    };
    let Some(caller_project_id) = decision.caller_project_id() else {
        return decision;
    };
    if revalidate(caller_project_id).await {
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
        authorize(store, container_id, &route, |_| async { true }).await
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
            &route,
            |_| async { false },
        )
        .await;
        assert_eq!(decision, AccessDecision::Denied(DenyReason::StaleCaller));

        // The same project's caller is re-checked the same way.
        let decision = authorize(
            &store,
            Some("container-target-staging"),
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
            &route,
            |project| {
                charged.store(project, Ordering::SeqCst);
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
}
