// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Golden capture of the container-create request a worker agent sends
//! for a managed service (ADR-055 phase 0).
//!
//! The control plane creates a database on a worker through
//! `POST /agent/services`, and the agent builds its own create request,
//! which differs from the control plane's (defect 1: no pids cap, no
//! `no-new-privileges`, no healthcheck, other labels). This fixture records
//! that request as it is today; phase 4 replaces both paths with the Docker
//! driver. `TEMPS_UPDATE_GOLDEN=1` rewrites it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use temps_docker_capture::{assert_golden, golden_value, FakeDocker, Method};
use temps_node_agent::handlers::AgentState;
use temps_node_agent::service_handlers::create_service;
use temps_node_agent::{ServiceCreateRequest, ServicePortMapping};

#[tokio::test]
async fn golden_worker_postgres_service() {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    fake.respond(
        Method::GET,
        "/networks",
        200,
        json!([{ "Name": temps_core::NETWORK_NAME.as_str() }]),
    );
    let remote = Arc::new(
        temps_deployer::remote::RemoteNodeDeployer::new(
            "http://127.0.0.1:9".into(),
            "unused".into(),
            "no-io".into(),
        )
        .expect("build an unused remote deployer"),
    );
    let state = Arc::new(AgentState {
        container_deployer: remote.clone(),
        image_builder: remote,
        docker: Some(fake.docker()),
        overlay_bridge_address: Default::default(),
        overlay_peers: Default::default(),
        platform: Default::default(),
        host_bind_address: Arc::new(std::sync::RwLock::new("10.100.0.2".into())),
    });
    let request = ServiceCreateRequest {
        name: "postgres-golden".to_string(),
        service_type: "postgres".to_string(),
        image: "gotempsh/postgres-walg:18-bookworm".to_string(),
        environment: HashMap::from([
            ("POSTGRES_USER".to_string(), "postgres".to_string()),
            ("POSTGRES_PASSWORD".to_string(), "testpass123".to_string()),
        ]),
        port_mappings: vec![ServicePortMapping {
            host_port: 15432,
            container_port: 5432,
        }],
        volumes: HashMap::from([(
            "postgres-golden_data".to_string(),
            "/var/lib/postgresql/data".to_string(),
        )]),
        network: Some(temps_core::NETWORK_NAME.to_string()),
        command: None,
        resource_limits: None,
    };

    let response = create_service(State(state), Json(request))
        .await
        .into_response();
    assert!(
        response.status().is_server_error(),
        "the fake fails the create: {}",
        response.status()
    );
    let creates = fake.creates();
    assert_eq!(creates.len(), 1, "{:?}", fake.calls());
    assert_golden(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/golden/worker-postgres-service.json"),
        &Value::Array(creates.iter().map(golden_value).collect()),
    );
}
