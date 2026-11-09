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
use temps_docker_capture::{assert_golden, golden_value, FakeDocker, Method, CAPTURED_MESSAGE};
use temps_sandbox_providers::docker::{DockerSandboxConfig, DockerSandboxProvider};
use temps_sandbox_providers::{SandboxCreateConfig, SandboxProvider};

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

#[tokio::test]
async fn service_proxy_mount_requires_a_compatible_image_and_is_read_only() {
    for compatible in [false, true] {
        let root = tempfile::Builder::new()
            .prefix("access-create-")
            .tempdir_in("/tmp")
            .unwrap();
        let fake = FakeDocker::start().await.unwrap();
        fake.respond(
            Method::GET,
            "/images/",
            200,
            if compatible {
                json!({"Config": {"Labels": {"sh.temps.service-access.protocol": "1"}}})
            } else {
                json!({})
            },
        );
        if compatible {
            fake.respond_when(
                Method::POST,
                "/containers/create",
                "name=temps-service-access-probe-",
                201,
                json!({"Id":"probe-container", "Warnings":[]}),
            );
            fake.respond(
                Method::DELETE,
                "/containers/probe-container",
                204,
                json!(null),
            );
        }
        let provider =
            DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default())
                .with_service_access_root(root.path().join("mounts"));
        let creates = creates_for(&fake, &provider, config(990, Some("none"))).await;
        assert_eq!(creates.len(), if compatible { 2 } else { 1 });
        let all_creates = fake.creates();
        let body = &all_creates
            .iter()
            .find(|create| create.name.as_deref() == Some("temps-sandbox-990"))
            .unwrap()
            .body;
        let mount = body["HostConfig"]["Mounts"]
            .as_array()
            .and_then(|mounts| mounts.first());
        if compatible {
            let mount = mount.unwrap();
            assert_eq!(mount["Type"], "bind");
            assert_eq!(mount["ReadOnly"], true);
            assert_eq!(mount["Target"], "/run/temps-service-access");
            assert_eq!(body["HostConfig"]["RestartPolicy"]["Name"], "no");
            let source = PathBuf::from(mount["Source"].as_str().unwrap());
            tokio::time::timeout(Duration::from_secs(3), async {
                while source.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            assert!(mount.is_none());
        }
    }
}

#[tokio::test]
async fn a_daemon_without_the_client_mount_still_creates_an_ordinary_sandbox() {
    let root = tempfile::Builder::new()
        .prefix("access-unshared-")
        .tempdir_in("/tmp")
        .unwrap();
    let fake = FakeDocker::start().await.unwrap();
    fake.respond(
        Method::GET,
        "/images/",
        200,
        json!({"Config":{"Labels":{"sh.temps.service-access.protocol":"1"}}}),
    );
    fake.respond_when(
        Method::POST,
        "/containers/create",
        "name=temps-service-access-probe-",
        400,
        json!({"message":"invalid mount config: bind source path does not exist"}),
    );
    let provider =
        DockerSandboxProvider::new(Arc::new(fake.docker()), DockerSandboxConfig::default())
            .with_service_access_root(root.path().join("mounts"));
    let creates = creates_for(&fake, &provider, config(991, Some("none"))).await;
    assert_eq!(creates.len(), 2);
    let captured = fake.creates();
    let ordinary = captured
        .iter()
        .find(|create| create.name.as_deref() == Some("temps-sandbox-991"))
        .unwrap();
    assert!(ordinary.body["HostConfig"]["Mounts"].is_null());
    assert!(ordinary.body["Labels"]["sh.temps.sandbox.service-access.mount"].is_null());
}
