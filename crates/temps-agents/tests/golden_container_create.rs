// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Golden captures of the container-create requests the Docker sandbox
//! provider sends (ADR-055 phase 0).
//!
//! The real `SandboxProvider::create` runs against a fake Engine API
//! (`temps-docker-capture`) that records every `POST /containers/create`
//! and fails the sandbox's own create, so nothing after it runs. Phase 1
//! moves these requests into the Docker driver; the fixtures must not
//! change when it does. `TEMPS_UPDATE_GOLDEN=1` rewrites them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use temps_agents::sandbox::docker::{DockerSandboxConfig, DockerSandboxProvider};
use temps_agents::sandbox::{SandboxCreateConfig, SandboxProvider};
use temps_docker_capture::{assert_golden, golden_value, FakeDocker, Method, CAPTURED_MESSAGE};

const IMAGE: &str = "alpine:3.20";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(format!("{name}.json"))
}

fn config(run_id: i32, network_mode: Option<&str>) -> SandboxCreateConfig {
    SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id,
        container_name_override: None,
        host_work_dir: PathBuf::from("/tmp/temps-golden-sandbox"),
        workspace_volume: None,
        image: Some(IMAGE.to_string()),
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(1024),
        pids_limit: None,
        disk_size_mb: None,
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: network_mode.map(str::to_string),
        env_vars: HashMap::from([("TASK".to_string(), "golden".to_string())]),
        idle_timeout: Duration::from_secs(600),
        backend: None,
    }
}

/// The images exist, so neither the sandbox nor the proxy pulls or builds.
fn images_present(fake: &FakeDocker) {
    fake.respond(Method::GET, "/images/", 200, json!({}));
}

async fn creates_for(
    fake: &FakeDocker,
    provider: &DockerSandboxProvider,
    cfg: SandboxCreateConfig,
) -> Vec<Value> {
    let Err(error) = provider.create(cfg).await else {
        panic!("the fake fails the sandbox create, yet it succeeded");
    };
    assert!(error.to_string().contains(CAPTURED_MESSAGE), "{error}");
    fake.creates().iter().map(golden_value).collect()
}

#[tokio::test]
async fn golden_sandbox_without_network() {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    images_present(&fake);
    let provider =
        DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default());
    let creates = creates_for(&fake, &provider, config(901, Some("none"))).await;
    assert_eq!(creates.len(), 1, "{:?}", fake.calls());
    assert_golden(&fixture("sandbox-network-none"), &Value::Array(creates));
}

#[tokio::test]
async fn golden_sandbox_with_full_network_and_egress_proxy() {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    images_present(&fake);
    let container = "temps-sandbox-902";
    // The shared egress network and this sandbox's isolated bridge exist
    // with the expected policy, as after a first create.
    fake.respond(
        Method::GET,
        "/networks/temps-sandbox-egress-v1",
        200,
        json!({
            "Name": "temps-sandbox-egress-v1", "Driver": "bridge", "Internal": false,
            "EnableIPv6": false, "Labels": { "sh.temps.sandbox-egress-policy": "1" }
        }),
    );
    fake.respond(
        Method::GET,
        &format!("/networks/temps-sandbox-net-v3-{container}"),
        200,
        json!({
            "Name": format!("temps-sandbox-net-v3-{container}"), "Driver": "bridge",
            "Internal": true, "EnableIPv6": false,
            "Labels": {
                "sh.temps.sandbox-egress-policy": "1",
                "sh.temps.sandbox-network-for": container,
                "sh.temps.preview-gateway-for": "temps-preview-gateway"
            },
            "Options": { "com.docker.network.bridge.gateway_mode_ipv4": "isolated" }
        }),
    );
    fake.allow_create("temps-sandbox-egress-proxy");
    fake.respond(Method::POST, "/networks/", 200, Value::Null);
    // The proxy is started (and joined to networks) by name.
    fake.respond(
        Method::POST,
        "/containers/temps-sandbox-egress-proxy",
        204,
        Value::Null,
    );

    let provider =
        DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default());
    let creates = creates_for(&fake, &provider, config(902, None)).await;
    let names: Vec<_> = creates.iter().map(|c| c["name"].clone()).collect();
    assert_eq!(names.len(), 2, "proxy, then sandbox: {:?}", fake.calls());
    assert_golden(&fixture("sandbox-network-full"), &Value::Array(creates));
}
