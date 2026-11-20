// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Golden captures of the container-create request `DockerRuntime` sends
//! for a project deployment (ADR-055 phase 0).
//!
//! Each case runs the real `deploy_container` against a fake Engine API
//! (`temps-docker-capture`), which records `POST /containers/create` and
//! fails it, so nothing after the create runs. Phase 1 moves this request
//! into the Docker driver; these fixtures must not change when it does.
//! `TEMPS_UPDATE_GOLDEN=1` rewrites them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use temps_deployer::docker::DockerRuntime;
use temps_deployer::docker_socket_grant::DockerSocketGrant;
use temps_deployer::{
    ContainerDeployer, ContainerLogConfig, DeployRequest, PortMapping, Protocol, ResourceLimits,
    RestartPolicy,
};
use temps_docker_capture::{assert_golden, golden_value, FakeDocker, Method, CAPTURED_MESSAGE};

const NETWORK: &str = "temps-app-network";
const SECRETS_ROOT_PLACEHOLDER: &str = "<secrets-root>";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(format!("{name}.json"))
}

fn request(container_name: &str) -> DeployRequest {
    DeployRequest {
        image_name: "registry.example.test/shop/web:4f2a9c1".to_string(),
        container_name: container_name.to_string(),
        environment_vars: HashMap::from([
            ("PORT".to_string(), "3000".to_string()),
            ("NODE_ENV".to_string(), "production".to_string()),
        ]),
        secrets: HashMap::new(),
        port_mappings: vec![PortMapping {
            host_port: 0,
            container_port: 3000,
            protocol: Protocol::Tcp,
            host_ip: None,
        }],
        network_name: None,
        extra_networks: vec![],
        resource_limits: ResourceLimits {
            cpu_limit: Some(0.5),
            memory_limit_mb: Some(512),
            disk_limit_mb: None,
        },
        restart_policy: RestartPolicy::OnFailure,
        log_path: PathBuf::from("/tmp/temps-golden.log"),
        command: None,
        entrypoint: None,
        log_config: Some(ContainerLogConfig::app_default()),
        labels: HashMap::from([
            ("sh.temps.project_id".to_string(), "7".to_string()),
            ("sh.temps.environment".to_string(), "production".to_string()),
            ("sh.temps.deploy_id".to_string(), "31".to_string()),
        ]),
        project_slug: Some("shop".to_string()),
        control_plane_grants_socket: false,
    }
}

/// Runs `deploy_container` against a fresh fake and returns the golden
/// form of its one create request.
async fn capture(
    runtime: impl FnOnce(Arc<bollard::Docker>) -> DockerRuntime,
    req: DeployRequest,
) -> serde_json::Value {
    let fake = FakeDocker::start()
        .await
        .expect("start the fake Engine API");
    // The network already exists, as on any running install.
    fake.respond(Method::GET, "/networks", 200, json!([{ "Name": NETWORK }]));
    let runtime = runtime(Arc::new(fake.docker()));
    let error = runtime
        .deploy_container(req)
        .await
        .expect_err("the fake fails every create");
    assert!(error.to_string().contains(CAPTURED_MESSAGE), "{error}");
    let creates = fake.creates();
    assert_eq!(creates.len(), 1, "one create: {:?}", fake.calls());
    golden_value(&creates[0])
}

fn secrets_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp secrets root")
}

/// The secrets root is a temp dir; the fixture names it by placeholder.
fn without_secrets_root(value: serde_json::Value, root: &Path) -> serde_json::Value {
    let text = value
        .to_string()
        .replace(&root.display().to_string(), SECRETS_ROOT_PLACEHOLDER);
    serde_json::from_str(&text).expect("re-parse")
}

fn data_dir() {
    // `DockerRuntime::new` creates `$TEMPS_DATA_DIR/secrets`; keep that
    // out of the developer's home.
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp data dir"));
    std::env::set_var("TEMPS_DATA_DIR", dir.path());
}

#[tokio::test]
async fn dockerfile_startup_overrides_reach_the_engine_api() {
    data_dir();
    for (entrypoint, expected) in [
        (None, serde_json::Value::Null),
        (Some(vec![]), json!([""])),
        (
            Some(vec!["/bin/sh".into(), "-c".into()]),
            json!(["/bin/sh", "-c"]),
        ),
    ] {
        let root = secrets_root();
        let path = root.path().to_path_buf();
        let mut req = request("startup-overrides");
        req.command = Some(vec![
            "worker".into(),
            "argument with spaces".into(),
            "".into(),
        ]);
        req.entrypoint = entrypoint;
        let captured = capture(
            move |docker| {
                DockerRuntime::new(docker, false, NETWORK.to_string()).with_secrets_root(path)
            },
            req,
        )
        .await;
        assert_eq!(
            captured["body"]["Cmd"],
            json!(["worker", "argument with spaces", ""])
        );
        assert_eq!(captured["body"]["Entrypoint"], expected);
    }
}

#[tokio::test]
async fn golden_deployment_with_published_port_and_limits() {
    data_dir();
    let root = secrets_root();
    let path = root.path().to_path_buf();
    let golden = capture(
        move |docker| {
            DockerRuntime::new(docker, false, NETWORK.to_string()).with_secrets_root(path)
        },
        request("shop-production-31"),
    )
    .await;
    assert_golden(&fixture("deployer-deployment-basic"), &golden);
}

#[tokio::test]
async fn golden_deployment_with_secrets_and_a_socket_grant() {
    data_dir();
    let root = secrets_root();
    let path = root.path().to_path_buf();
    let mut req = request("infra-agent-production-8");
    req.secrets = HashMap::from([
        ("API_TOKEN".to_string(), "seeded-token-value".to_string()),
        ("DB_PASSWORD".to_string(), "seeded-db-password".to_string()),
    ]);
    req.project_slug = Some("infra-agent".to_string());
    req.control_plane_grants_socket = true;
    req.port_mappings[0].host_ip = Some("10.100.0.2".to_string());
    req.port_mappings[0].host_port = 41000;
    let golden = capture(
        move |docker| {
            DockerRuntime::new(docker, false, NETWORK.to_string())
                .with_secrets_root(path)
                .with_docker_socket_grant(DockerSocketGrant::from_slugs(["infra-agent"]))
        },
        req,
    )
    .await;
    let golden = without_secrets_root(golden, root.path());
    let text = golden.to_string();
    assert!(
        !text.contains("seeded-token-value") && !text.contains("seeded-db-password"),
        "secret values must never reach the create request: {text}"
    );
    assert_golden(&fixture("deployer-deployment-secrets-socket"), &golden);
}

#[tokio::test]
async fn golden_deployment_uncapped_with_command() {
    data_dir();
    let root = secrets_root();
    let path = root.path().to_path_buf();
    let mut req = request("worker-production-12");
    req.port_mappings = vec![];
    req.resource_limits = ResourceLimits {
        cpu_limit: None,
        memory_limit_mb: Some(0),
        disk_limit_mb: None,
    };
    req.restart_policy = RestartPolicy::Always;
    req.command = Some(vec![
        "bin/worker".to_string(),
        "--queue".to_string(),
        "default".to_string(),
    ]);
    req.log_config = None;
    req.labels = HashMap::new();
    let golden = capture(
        move |docker| {
            DockerRuntime::new(docker, false, NETWORK.to_string()).with_secrets_root(path)
        },
        req,
    )
    .await;
    assert_golden(&fixture("deployer-deployment-uncapped"), &golden);
}
