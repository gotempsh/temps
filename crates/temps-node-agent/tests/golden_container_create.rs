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
        service_runtime: Arc::new(temps_providers::node_runtime::ProvidersNodeRuntime),
        plugin_builder: Arc::new(NoPluginBuilds),
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
    let golden: Vec<Value> = creates
        .iter()
        .map(golden_value)
        .map(with_stable_create_attempt)
        .collect();
    assert_golden(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/golden/worker-postgres-service.json"),
        &Value::Array(golden),
    );
}

/// Label that tags a container with the create call that made it, so a
/// cleanup removes only its own container.
const CREATE_ATTEMPT_LABEL: &str = "sh.temps.service.create-attempt";

/// The create-attempt label is a fresh random ID per create. Check its
/// shape here and record a placeholder, so the fixture stays stable while
/// still showing that the label is sent.
fn with_stable_create_attempt(mut create: Value) -> Value {
    let Some(label) = create
        .pointer_mut("/body/Labels")
        .and_then(|labels| labels.get_mut(CREATE_ATTEMPT_LABEL))
    else {
        panic!("the create request carries no {CREATE_ATTEMPT_LABEL} label: {create}");
    };
    let attempt = label.as_str().unwrap_or_default();
    assert!(
        attempt.len() == 32 && attempt.chars().all(|c| c.is_ascii_hexdigit()),
        "the create attempt is a random 32-hex-digit ID, got {attempt:?}"
    );
    *label = Value::String("<random per create>".to_string());
    create
}

/// These tests never compile a plugin.
struct NoPluginBuilds;

#[async_trait::async_trait]
impl temps_node_agent::plugin_build_handler::PluginContainerBuilder for NoPluginBuilds {
    async fn build_from_archive(
        &self,
        _archive: &std::path::Path,
        _target: &str,
        _output: &std::path::Path,
    ) -> Result<(), temps_node_agent::plugin_build_handler::PluginBuildError> {
        Err(temps_node_agent::plugin_build_handler::PluginBuildError::RuntimeMissing)
    }
}
