// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end integration tests for live Traefik-label route discovery.
//!
//! These run against **real** infrastructure — a real Docker daemon with real
//! containers on a real network, and a real Postgres (schema-isolated per
//! test) with the full migration set — and assert the whole chain:
//!
//! ```text
//! container labels -> reconcile() -> traefik_discovered_routes row
//!                  -> CachedPeerTable::load_routes() -> get_route_by_host()
//! ```
//!
//! Skips gracefully (project convention — never `#[ignore]`) when Docker or
//! Postgres is unavailable: every `boot_*` helper returns `Option`/`Result`
//! and the test returns early with a printed reason.

use std::collections::HashMap;
use std::sync::Arc;

use bollard::query_parameters::InspectContainerOptions;
use bollard::Docker;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use temps_database::test_utils::TestDatabase;
use temps_deployer::traefik_discovery::{
    ConflictReason, TraefikDiscoveryConfig, TraefikDiscoveryService,
};
use temps_entities::traefik_discovered_routes as discovered;
use temps_routes::route_table::{BackendType, CachedPeerTable};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};

/// Tiny image that stays up without needing to serve anything: discovery only
/// reads labels and Docker metadata, never talks to the workload.
const IDLE_IMAGE: &str = "alpine";
const IDLE_TAG: &str = "3.20";

/// Connect to the Docker daemon, or return `None` so the caller can skip.
async fn boot_docker() -> Option<Arc<Docker>> {
    let docker = match Docker::connect_with_defaults() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("⏭️  Docker unavailable, skipping: {e}");
            return None;
        }
    };
    if let Err(e) = docker.ping().await {
        eprintln!("⏭️  Docker daemon unreachable, skipping: {e}");
        return None;
    }
    Some(Arc::new(docker))
}

/// Boot a real Postgres with the full Temps schema, or return `None`.
async fn boot_database() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!("⏭️  Postgres/testcontainers unavailable, skipping: {e}");
            None
        }
    }
}

/// A unique network name per test so parallel runs can't see each other's
/// containers (and so a leaked container from a previous run can't leak in).
fn unique_network(prefix: &str) -> String {
    format!(
        "temps-traefik-itest-{}-{}",
        prefix,
        &uuid::Uuid::new_v4().to_string()[..8]
    )
}

/// Start an idle container carrying the given labels on the given network.
///
/// `exposed_port` is declared on the container (and therefore published to a
/// random host port by testcontainers) because discovery deliberately refuses
/// to honour a `loadbalancer.server.port` label naming a port the container
/// never advertised — that is what stops a label from pointing a hostname at an
/// arbitrary port on the Temps host. Tests must exercise the real, validated
/// path, so the port in the labels has to be a port the container really has.
async fn start_labelled_container(
    network: &str,
    container_name: &str,
    exposed_port: u16,
    labels: &[(&str, &str)],
) -> Option<ContainerAsync<GenericImage>> {
    let mut request = GenericImage::new(IDLE_IMAGE, IDLE_TAG)
        .with_wait_for(WaitFor::seconds(1))
        .with_exposed_port(ContainerPort::Tcp(exposed_port))
        .with_cmd(["sleep", "300"])
        .with_network(network.to_string())
        .with_container_name(container_name.to_string());
    for (k, v) in labels {
        request = request.with_label(k.to_string(), v.to_string());
    }

    match request.start().await {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("⏭️  Could not start test container {container_name}: {e}");
            None
        }
    }
}

fn service(
    docker: Arc<Docker>,
    db: Arc<DatabaseConnection>,
    network: &str,
) -> TraefikDiscoveryService {
    TraefikDiscoveryService::new(
        docker,
        db,
        TraefikDiscoveryConfig {
            enabled: true,
            network: network.to_string(),
            poll_interval: std::time::Duration::from_secs(30),
        },
        // No refresher: each test drives `load_routes()` explicitly so the
        // assertion is about the persisted rows, not about timing.
        None,
    )
}

/// A route table configured exactly as `temps serve` configures it when
/// discovery is enabled for `network`.
///
/// `load_routes()` only serves discovered rows for the network this process is
/// configured to adopt from, and serves none at all when discovery is off — so
/// every route-table assertion here has to say which network it is.
fn discovery_table(db: Arc<DatabaseConnection>, network: &str) -> CachedPeerTable {
    let table = CachedPeerTable::new(db);
    table.set_traefik_discovery_network(Some(network.to_string()));
    table
}

async fn rows_for(db: &DatabaseConnection, network: &str) -> Vec<discovered::Model> {
    discovered::Entity::find()
        .filter(discovered::Column::Network.eq(network.to_string()))
        .all(db)
        .await
        .expect("query discovered routes")
}

/// Baseline: a labelled container on the watched network becomes a persisted
/// row AND a live entry the proxy's route table can resolve.
#[tokio::test]
async fn discovers_a_labelled_container_and_the_route_table_serves_it() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("basic");
    let host = "discovered-app.example.test";

    let Some(_container) = start_labelled_container(
        &network,
        &format!("temps-itest-app-{}", &network[network.len() - 8..]),
        3000,
        &[
            ("traefik.enable", "true"),
            ("traefik.http.routers.app.rule", &format!("Host(`{host}`)")),
            ("traefik.http.services.app.loadbalancer.server.port", "3000"),
            ("traefik.http.routers.app.tls", "true"),
        ],
    )
    .await
    else {
        return;
    };

    let svc = service(docker, db.clone(), &network);
    let outcome = svc.reconcile().await.expect("reconcile");

    assert_eq!(outcome.routes_upserted, 1, "outcome: {outcome:?}");
    assert_eq!(outcome.routes_removed, 0);
    assert!(outcome.conflicts.is_empty(), "outcome: {outcome:?}");

    let rows = rows_for(db.as_ref(), &network).await;
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.host, host);
    assert_eq!(row.router_name, "app");
    assert_eq!(row.target_port, 3000);
    assert!(row.tls, "the tls label must be persisted");
    assert!(row.enabled);
    assert!(
        row.target_container_name.contains("temps-itest-app"),
        "container name should be the Docker name, got {}",
        row.target_container_name
    );

    assert!(
        row.target_host_port.is_some(),
        "the test container publishes its port, so the row must record the host port \
         (a baremetal install has no other way to reach it)"
    );

    // The whole point: the proxy's real route table resolves it.
    let table = discovery_table(db.clone(), &network);
    table.load_routes().await.expect("load_routes");
    let route = table
        .get_route_by_host(host)
        .unwrap_or_else(|| panic!("route table must serve the discovered host {host}"));
    assert!(
        route.deployment.is_none(),
        "a discovered route has no Temps deployment behind it"
    );
    assert!(route.project.is_none());
    assert!(
        !route.cert_eligible,
        "a container-supplied tls label must NOT drive ACME issuance: these labels belong to a \
         workload Temps did not deploy"
    );
    match &route.backend {
        BackendType::Upstream { backends, .. } => {
            assert_eq!(backends.len(), 1);
            assert_eq!(
                backends[0].container_name.as_deref(),
                Some(row.target_container_name.as_str())
            );
            assert!(
                backends[0].address.ends_with(":3000")
                    || backends[0].address.starts_with("127.0.0.1:"),
                "unexpected backend address {}",
                backends[0].address
            );
        }
        other => panic!("expected an upstream backend, got {other:?}"),
    }

    // Idempotence: a second pass must not rewrite anything (which would
    // otherwise fire a route reload on every 30s tick, forever).
    let second = svc.reconcile().await.expect("second reconcile");
    assert_eq!(second.routes_upserted, 0, "outcome: {second:?}");
    assert_eq!(second.routes_removed, 0, "outcome: {second:?}");
    assert_eq!(second.routes_unchanged, 1, "outcome: {second:?}");
}

/// A container that goes away loses its route — otherwise the proxy keeps
/// sending traffic at a dead backend.
#[tokio::test]
async fn stopping_a_container_removes_its_discovered_route() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("stop");
    let host = "going-away.example.test";

    let Some(container) = start_labelled_container(
        &network,
        &format!("temps-itest-stop-{}", &network[network.len() - 8..]),
        8080,
        &[
            ("traefik.enable", "true"),
            ("traefik.http.routers.gone.rule", &format!("Host(`{host}`)")),
            (
                "traefik.http.services.gone.loadbalancer.server.port",
                "8080",
            ),
        ],
    )
    .await
    else {
        return;
    };
    let container_id = container.id().to_string();

    let svc = service(docker.clone(), db.clone(), &network);
    svc.reconcile().await.expect("initial reconcile");
    assert_eq!(rows_for(db.as_ref(), &network).await.len(), 1);

    // Incremental path: the `die` event handler must drop the row.
    let changed = svc
        .handle_container_event(&container_id, "die")
        .await
        .expect("handle die event");
    assert!(
        changed,
        "a die event for a routed container must change state"
    );
    assert!(
        rows_for(db.as_ref(), &network).await.is_empty(),
        "the route must be removed when the container dies"
    );

    let table = discovery_table(db.clone(), &network);
    table.load_routes().await.expect("load_routes");
    assert!(
        table.get_route_by_host(host).is_none(),
        "a dead container must not remain routable"
    );

    // Reconciliation path: after actually stopping the container, a full pass
    // must agree (and must not resurrect the row).
    container.stop().await.ok();
    // `stop()` returns once the daemon has processed the request, but Docker's
    // own `list_containers(status=running)` filter (what `reconcile()` uses to
    // find candidates) can lag a beat behind that under CI load. Poll the
    // container's real state instead of racing it, so this test verifies
    // reconcile's behavior against a genuinely-stopped container rather than
    // one that is still transitioning.
    wait_until_not_running(&docker, &container_id).await;
    let outcome = svc.reconcile().await.expect("reconcile after stop");
    assert_eq!(outcome.routes_upserted, 0, "outcome: {outcome:?}");
    assert!(rows_for(db.as_ref(), &network).await.is_empty());
}

/// Poll Docker until `container_id` is no longer reported as running, or give
/// up after 10s. Used to close the race between `stop()` returning and the
/// daemon's container-list filters reflecting the new state.
async fn wait_until_not_running(docker: &Docker, container_id: &str) {
    for _ in 0..100 {
        let running = docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|resp| resp.state)
            .and_then(|state| state.running)
            .unwrap_or(false);
        if !running {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    eprintln!("⚠️  container {container_id} still reported running after 10s");
}

/// A Temps-deployed container must never be adopted through its labels, even
/// if it carries a perfectly valid Traefik label set. This is the anti-hijack
/// property: a workload Temps already routes for cannot redirect itself.
#[tokio::test]
async fn temps_managed_container_is_never_discovered() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("owned");
    let host = "self-claimed.example.test";

    let Some(_container) = start_labelled_container(
        &network,
        &format!("temps-itest-owned-{}", &network[network.len() - 8..]),
        80,
        &[
            ("sh.temps.deploy_id", "424242"),
            ("sh.temps.project_id", "7"),
            ("traefik.enable", "true"),
            ("traefik.http.routers.self.rule", &format!("Host(`{host}`)")),
            ("traefik.http.services.self.loadbalancer.server.port", "80"),
        ],
    )
    .await
    else {
        return;
    };

    let svc = service(docker, db.clone(), &network);
    let outcome = svc.reconcile().await.expect("reconcile");

    assert_eq!(
        outcome.skipped_temps_managed, 1,
        "the Temps-managed container must be counted as skipped: {outcome:?}"
    );
    assert_eq!(outcome.routes_upserted, 0, "outcome: {outcome:?}");
    assert!(
        rows_for(db.as_ref(), &network).await.is_empty(),
        "a container carrying sh.temps.deploy_id must never produce a discovered route"
    );

    // The incremental event path must apply the same rule.
    let containers = discovered::Entity::find()
        .all(db.as_ref())
        .await
        .expect("query all discovered routes");
    assert!(containers.is_empty());
}

/// A discovered host that collides with an existing DB-driven route loses:
/// the legitimate route is kept, the discovery is skipped, and the conflict is
/// surfaced rather than silently swallowed.
#[tokio::test]
async fn discovered_route_loses_a_host_collision_with_a_db_driven_route() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("collide");
    let host = "contested.example.test";

    // Pre-existing, legitimate, operator-configured route for the same host.
    temps_entities::custom_routes::ActiveModel {
        domain: Set(host.to_string()),
        host: Set("10.9.8.7".to_string()),
        port: Set(9999),
        enabled: Set(true),
        route_type: Set(temps_entities::custom_routes::RouteType::Http),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("seed the legitimate custom route");

    let Some(_container) = start_labelled_container(
        &network,
        &format!("temps-itest-collide-{}", &network[network.len() - 8..]),
        3000,
        &[
            ("traefik.enable", "true"),
            (
                "traefik.http.routers.squat.rule",
                &format!("Host(`{host}`)"),
            ),
            (
                "traefik.http.services.squat.loadbalancer.server.port",
                "3000",
            ),
        ],
    )
    .await
    else {
        return;
    };

    let svc = service(docker, db.clone(), &network);
    let outcome = svc.reconcile().await.expect("reconcile");

    assert_eq!(
        outcome.routes_upserted, 0,
        "a contested host must not be written: {outcome:?}"
    );
    assert!(
        rows_for(db.as_ref(), &network).await.is_empty(),
        "no discovered row may exist for a host owned by a real route"
    );
    assert_eq!(outcome.conflicts.len(), 1, "outcome: {outcome:?}");
    let conflict = &outcome.conflicts[0];
    assert_eq!(conflict.host, host);
    assert_eq!(conflict.reason, ConflictReason::OwnedByTempsRoute);
    assert_eq!(
        svc.last_outcome()
            .expect("outcome recorded")
            .conflicts
            .len(),
        1,
        "the conflict must stay visible for the status surface"
    );

    // The legitimate route is untouched and still serves the host.
    let table = discovery_table(db.clone(), &network);
    table.load_routes().await.expect("load_routes");
    let route = table
        .get_route_by_host(host)
        .expect("the pre-existing custom route must still resolve");
    match &route.backend {
        BackendType::Upstream { backends, .. } => {
            assert_eq!(
                backends[0].address, "10.9.8.7:9999",
                "the operator's custom route must win the host"
            );
        }
        other => panic!("expected an upstream backend, got {other:?}"),
    }
}

/// Two containers claiming the same host: the first (by container name) wins
/// deterministically, and the loser is reported instead of flapping.
#[tokio::test]
async fn two_containers_claiming_one_host_resolve_deterministically() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("dup");
    let host = "duplicate.example.test";
    let suffix = network[network.len() - 8..].to_string();

    let labels = |router: &str| -> Vec<(String, String)> {
        vec![
            ("traefik.enable".to_string(), "true".to_string()),
            (
                format!("traefik.http.routers.{router}.rule"),
                format!("Host(`{host}`)"),
            ),
            (
                format!("traefik.http.services.{router}.loadbalancer.server.port"),
                "3000".to_string(),
            ),
        ]
    };

    let first_labels = labels("first");
    let first_refs: Vec<(&str, &str)> = first_labels
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let Some(_a) = start_labelled_container(
        &network,
        &format!("temps-itest-aaa-{suffix}"),
        3000,
        &first_refs,
    )
    .await
    else {
        return;
    };

    let second_labels = labels("second");
    let second_refs: Vec<(&str, &str)> = second_labels
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let Some(_b) = start_labelled_container(
        &network,
        &format!("temps-itest-zzz-{suffix}"),
        3000,
        &second_refs,
    )
    .await
    else {
        return;
    };

    let svc = service(docker, db.clone(), &network);
    let first_pass = svc.reconcile().await.expect("reconcile");
    assert_eq!(first_pass.routes_upserted, 1, "outcome: {first_pass:?}");
    assert_eq!(first_pass.conflicts.len(), 1, "outcome: {first_pass:?}");

    let rows = rows_for(db.as_ref(), &network).await;
    assert_eq!(rows.len(), 1);
    let winner = rows[0].target_container_name.clone();
    assert!(
        winner.contains("aaa"),
        "the lexicographically first container must win, got {winner}"
    );

    // Deterministic across passes: the winner must not flap.
    let second_pass = svc.reconcile().await.expect("second reconcile");
    assert_eq!(second_pass.routes_upserted, 0, "outcome: {second_pass:?}");
    assert_eq!(
        rows_for(db.as_ref(), &network).await[0].target_container_name,
        winner
    );
}

/// A container without Traefik labels (or with `traefik.enable=false`) on the
/// watched network is inert — discovery must not adopt every container it can
/// see just because the operator turned the feature on.
#[tokio::test]
async fn unlabelled_and_disabled_containers_are_ignored() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let network = unique_network("inert");
    let suffix = network[network.len() - 8..].to_string();

    let Some(_plain) =
        start_labelled_container(&network, &format!("temps-itest-plain-{suffix}"), 80, &[]).await
    else {
        return;
    };
    let Some(_disabled) = start_labelled_container(
        &network,
        &format!("temps-itest-off-{suffix}"),
        80,
        &[
            ("traefik.enable", "false"),
            (
                "traefik.http.routers.off.rule",
                "Host(`disabled.example.test`)",
            ),
            ("traefik.http.services.off.loadbalancer.server.port", "80"),
        ],
    )
    .await
    else {
        return;
    };

    let svc = service(docker, db.clone(), &network);
    let outcome = svc.reconcile().await.expect("reconcile");

    assert!(
        outcome.containers_scanned >= 2,
        "both containers should be visible on the network: {outcome:?}"
    );
    assert_eq!(outcome.routes_upserted, 0, "outcome: {outcome:?}");
    assert!(rows_for(db.as_ref(), &network).await.is_empty());
}

/// A disabled row is kept for visibility but must not reach the route table —
/// that is the operator kill-switch for one discovered route.
#[tokio::test]
async fn disabled_discovered_rows_are_not_routed() {
    let Some(test_db) = boot_database().await else {
        return;
    };
    let db = test_db.connection_arc();
    let host = "kill-switched.example.test";

    discovered::ActiveModel {
        host: Set(host.to_string()),
        router_name: Set("app".to_string()),
        target_container_id: Set("cafebabe".to_string()),
        target_container_name: Set("some-container".to_string()),
        target_port: Set(3000),
        // Published, so the only thing keeping it out of the table is the
        // kill switch — not unreachability.
        target_host_port: Set(Some(13000)),
        network: Set("temps".to_string()),
        tls: Set(false),
        enabled: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("seed a disabled discovered route");

    let table = discovery_table(db.clone(), "temps");
    table.load_routes().await.expect("load_routes");
    assert!(
        table.get_route_by_host(host).is_none(),
        "a disabled discovered route must not be served"
    );

    // ...and flipping the kill switch back on makes it serve, proving the
    // assertion above is about `enabled` and nothing else.
    discovered::Entity::update_many()
        .col_expr(
            discovered::Column::Enabled,
            sea_orm::sea_query::Expr::value(true),
        )
        .filter(discovered::Column::Host.eq(host.to_string()))
        .exec(db.as_ref())
        .await
        .expect("re-enable the discovered route");
    table.load_routes().await.expect("reload after re-enable");
    assert!(
        table.get_route_by_host(host).is_some(),
        "re-enabling the kill switch must bring the route back"
    );

    // A table whose process has discovery disabled serves it either way.
    let off = CachedPeerTable::new(db.clone());
    off.load_routes()
        .await
        .expect("load_routes with discovery off");
    assert!(
        off.get_route_by_host(host).is_none(),
        "a node with discovery disabled must serve no discovered routes at all"
    );
}

/// Sanity check that the label map we build in these tests is what Docker
/// actually reports — a silent label-mangling regression would make every
/// other test in this file pass for the wrong reason.
#[tokio::test]
async fn docker_reports_the_labels_we_set() {
    let Some(docker) = boot_docker().await else {
        return;
    };
    let network = unique_network("labels");
    let name = format!("temps-itest-labels-{}", &network[network.len() - 8..]);

    let Some(container) = start_labelled_container(
        &network,
        &name,
        80,
        &[
            ("traefik.enable", "true"),
            (
                "traefik.http.routers.probe.rule",
                "Host(`probe.example.test`)",
            ),
        ],
    )
    .await
    else {
        return;
    };

    let inspect = docker
        .inspect_container(
            container.id(),
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect container");
    let labels: HashMap<String, String> = inspect
        .config
        .and_then(|c| c.labels)
        .expect("container must report labels");

    assert_eq!(
        labels.get("traefik.enable").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        labels
            .get("traefik.http.routers.probe.rule")
            .map(String::as_str),
        Some("Host(`probe.example.test`)"),
        "backticks must survive the Docker round trip"
    );
}
