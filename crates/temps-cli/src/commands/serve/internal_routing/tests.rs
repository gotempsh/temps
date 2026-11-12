// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's internal routes admit exactly what a worker's would
//! (ADR-104 D2): its own project, and another project only through a grant.

use super::*;
use temps_core::internal_access::{InternalAccessCaller, InternalAccessGrantScope};
use temps_node_agent::internal_access::{decide, AccessDecision, DenyReason};
use temps_routes::internal_access_grants::attach_allowed_callers;
use temps_routes::route_sync::RouteBackendDto;

const API_PROJECT: i32 = 1;
const API_ENV: i32 = 11;
const WORKER_PROJECT: i32 = 2;
const WORKER_ENV: i32 = 21;
const API_HOST: &str = "production.api.temps.local";
const WORKER_HOST: &str = "production.worker.temps.local";

/// A route as the control plane's route table holds a local deployment: a
/// host-port backend on loopback and the container that serves it.
fn dto(host: &str, project_id: i32, environment_id: i32, container_id: &str) -> RouteEntryDto {
    RouteEntryDto {
        host: host.to_string(),
        backends: vec![RouteBackendDto {
            address: "127.0.0.1:32768".to_string(),
            container_id: Some(container_id.to_string()),
            container_name: Some(format!("{container_id}-name")),
        }],
        deployment_id: Some(project_id * 100),
        project_id: Some(project_id),
        environment_id: Some(environment_id),
        gate: None,
        shield: None,
        allowed_callers: Vec::new(),
    }
}

fn grant(target_project_id: i32, caller: InternalAccessCaller) -> InternalAccessGrantScope {
    InternalAccessGrantScope {
        target_project_id,
        target_environment_id: None,
        caller,
    }
}

/// Build the store the way the feeder does: grants attached as
/// `internal_route_entries` attaches them, then mapped and applied.
fn store_with(grants: &[InternalAccessGrantScope]) -> (tempfile::TempDir, RouteStore) {
    let dir = tempfile::tempdir().unwrap();
    let mut routes = vec![
        dto(API_HOST, API_PROJECT, API_ENV, "api-container"),
        dto(WORKER_HOST, WORKER_PROJECT, WORKER_ENV, "worker-container"),
    ];
    attach_allowed_callers(&mut routes, grants);
    let store = RouteStore::new(snapshot_path(dir.path()));
    store.apply_snapshot(7, routes.into_iter().map(route_entry).collect());
    (dir, store)
}

fn decision(store: &RouteStore, caller_container: &str, host: &str) -> AccessDecision {
    let route = store.lookup(host).expect("route is in the store");
    store
        .with_container_identities(caller_container, |identities| {
            decide(&route, Some(identities))
        })
        .unwrap_or_else(|| decide(&route, None))
}

#[test]
fn route_entry_keeps_every_field_a_worker_route_has() {
    let mut source = dto(API_HOST, API_PROJECT, API_ENV, "api-container");
    source.allowed_callers = vec![InternalAccessCaller {
        project_id: WORKER_PROJECT,
        environment_id: Some(WORKER_ENV),
    }];
    let entry = route_entry(source.clone());
    assert_eq!(entry.host, source.host);
    assert_eq!(entry.backends.len(), 1);
    assert_eq!(entry.backends[0].address, "127.0.0.1:32768");
    assert_eq!(
        entry.backends[0].container_id.as_deref(),
        Some("api-container")
    );
    assert_eq!(
        entry.backends[0].container_name.as_deref(),
        Some("api-container-name")
    );
    assert_eq!(entry.deployment_id, Some(API_PROJECT * 100));
    assert_eq!(entry.project_id, Some(API_PROJECT));
    assert_eq!(entry.environment_id, Some(API_ENV));
    assert_eq!(entry.allowed_callers, source.allowed_callers);
    assert!(entry.gate.is_none());
    assert!(entry.shield.is_none());
}

#[test]
fn a_caller_from_the_same_project_is_admitted() {
    let (_dir, store) = store_with(&[]);
    assert_eq!(
        decision(&store, "api-container", API_HOST),
        AccessDecision::SameProject {
            caller_project_id: API_PROJECT
        }
    );
}

#[test]
fn a_caller_from_another_project_without_a_grant_is_refused() {
    let (_dir, store) = store_with(&[]);
    assert_eq!(
        decision(&store, "worker-container", API_HOST),
        AccessDecision::Denied(DenyReason::NotGranted)
    );
}

#[test]
fn a_grant_admits_the_caller_project_on_the_target_only() {
    let (_dir, store) = store_with(&[grant(
        API_PROJECT,
        InternalAccessCaller {
            project_id: WORKER_PROJECT,
            environment_id: None,
        },
    )]);
    assert_eq!(
        decision(&store, "worker-container", API_HOST),
        AccessDecision::Granted {
            caller_project_id: WORKER_PROJECT,
            caller_environment_id: Some(WORKER_ENV),
        }
    );
    // A grant is one-way: the target project still may not call the caller.
    assert_eq!(
        decision(&store, "api-container", WORKER_HOST),
        AccessDecision::Denied(DenyReason::NotGranted)
    );
}

#[test]
fn a_grant_for_another_environment_does_not_admit_the_caller() {
    let (_dir, store) = store_with(&[grant(
        API_PROJECT,
        InternalAccessCaller {
            project_id: WORKER_PROJECT,
            environment_id: Some(WORKER_ENV + 1),
        },
    )]);
    assert_eq!(
        decision(&store, "worker-container", API_HOST),
        AccessDecision::Denied(DenyReason::NotGranted)
    );
}

#[test]
fn a_container_the_route_table_does_not_know_is_refused() {
    // The console, a database or anything else on the app network serves no
    // route, so it is nobody's caller.
    let (_dir, store) = store_with(&[]);
    assert_eq!(
        decision(&store, "unmanaged-container", API_HOST),
        AccessDecision::Denied(DenyReason::UnknownCaller)
    );
}

#[test]
fn revoking_the_grant_refuses_the_caller_on_the_next_snapshot() {
    let (_dir, store) = store_with(&[grant(
        API_PROJECT,
        InternalAccessCaller {
            project_id: WORKER_PROJECT,
            environment_id: None,
        },
    )]);
    assert!(matches!(
        decision(&store, "worker-container", API_HOST),
        AccessDecision::Granted { .. }
    ));
    let routes = vec![
        dto(API_HOST, API_PROJECT, API_ENV, "api-container"),
        dto(WORKER_HOST, WORKER_PROJECT, WORKER_ENV, "worker-container"),
    ];
    store.apply_snapshot(8, routes.into_iter().map(route_entry).collect());
    assert_eq!(
        decision(&store, "worker-container", API_HOST),
        AccessDecision::Denied(DenyReason::NotGranted)
    );
}

#[test]
fn the_snapshot_lives_under_the_data_directory() {
    let path = snapshot_path(Path::new("/data"));
    assert_eq!(path, PathBuf::from("/data/internal-routing/routes.json"));
}

/// A supervisor that ignores shutdown (a pass stuck on a slow Docker daemon)
/// is aborted once the grace period ends, and is gone, with everything it
/// held, by the time the wait returns. Dropping the handle instead would leave
/// it running.
#[tokio::test(start_paused = true)]
async fn a_supervisor_stuck_in_a_pass_is_aborted_not_detached() {
    let (held, released) = tokio::sync::oneshot::channel::<()>();
    let mut task = tokio::spawn(async move {
        let _listener = held;
        std::future::pending::<()>().await;
    });
    assert!(finish_or_abort(&mut task, Duration::from_secs(10)).await);
    assert!(task.is_finished());
    // What the task held was dropped with it.
    assert!(released.await.is_err());
}

/// A supervisor that ends within the grace period is not aborted.
#[tokio::test(start_paused = true)]
async fn a_supervisor_that_stops_in_time_is_left_to_finish() {
    let mut task = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    assert!(!finish_or_abort(&mut task, Duration::from_secs(10)).await);
    assert!(task.is_finished());
}
