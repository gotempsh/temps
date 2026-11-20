// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What actually crosses the wire when values are sent to a worker.

use std::collections::HashMap;

use temps_core::sealed_request::{open_request, NodeSealing, SEALED_REQUEST_CONTENT_TYPE};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::compose_protocol::{
    ComposePrepareSpec, COMPOSE_PREPARE_PATH, SEALED_COMPOSE_SPEC_FIELD,
};
use crate::{ContainerDeployer, DeployRequest};

const SECRET: &str = "secret-value-only-the-node-may-read";
const VARIABLE: &str = "variable-value-only-the-node-may-read";

fn node_keypair() -> (String, String) {
    temps_core::worker_ingress::generate_ingress_key().unwrap()
}

struct Recorded {
    head: String,
    body: Vec<u8>,
}

impl Recorded {
    fn raw(&self) -> String {
        format!("{}{}", self.head, String::from_utf8_lossy(&self.body))
    }
    fn is_sealed(&self) -> bool {
        self.head
            .to_ascii_lowercase()
            .contains(&format!("content-type: {SEALED_REQUEST_CONTENT_TYPE}"))
    }
}

/// An agent that answers one request and records exactly what it received.
async fn recording_agent(
    status: &'static str,
    reply: &'static str,
) -> (String, tokio::task::JoinHandle<Recorded>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut chunk = [0_u8; 8192];
        let (head, length) = loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the client closed before sending its headers");
            received.extend_from_slice(&chunk[..read]);
            if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..end]).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                received.drain(..end + 4);
                break (head, length);
            }
        };
        while received.len() < length {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the client closed before sending its body");
            received.extend_from_slice(&chunk[..read]);
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{reply}",
            reply.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        Recorded {
            head,
            body: received,
        }
    });
    (format!("http://{address}"), server)
}

fn deployer(url: String, sealing: NodeSealing) -> RemoteNodeDeployer {
    RemoteNodeDeployer::new(url, "node-token".to_string(), "worker-a".to_string())
        .unwrap()
        .with_sealing(sealing)
}

fn deploy_request(values: bool) -> DeployRequest {
    let mut environment_vars = HashMap::new();
    let mut secrets = HashMap::new();
    if values {
        environment_vars.insert("PUBLIC_NAME".to_string(), VARIABLE.to_string());
        secrets.insert("API_KEY".to_string(), SECRET.to_string());
    }
    DeployRequest {
        image_name: "registry.example/app:v1".to_string(),
        container_name: "app-1".to_string(),
        environment_vars,
        secrets,
        port_mappings: vec![],
        network_name: None,
        extra_networks: Vec::new(),
        resource_limits: crate::ResourceLimits::default(),
        restart_policy: crate::RestartPolicy::Never,
        log_path: std::path::PathBuf::from("/tmp/app-1.log"),
        command: None,
        entrypoint: None,
        log_config: None,
        labels: HashMap::new(),
        project_slug: None,
        control_plane_grants_socket: false,
    }
}

const DEPLOYED: &str = r#"{"success":true,"data":{"container_id":"abc","container_name":"app-1","image_name":"registry.example/app:v1","host_port":0,"container_port":0,"status":"Running"},"error":null}"#;

/// Whatever the reply decodes to, the request has been recorded by then.
async fn deploy_and_record(url: String, sealing: NodeSealing, request: DeployRequest) {
    let _ = deployer(url, sealing).deploy_container(request).await;
}

#[tokio::test]
async fn a_deploy_with_values_crosses_the_wire_encrypted_and_only_the_node_can_open_it() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", DEPLOYED).await;

    deploy_and_record(url, NodeSealing::Key(public), deploy_request(true)).await;

    let request = agent.await.unwrap();
    assert!(request.head.starts_with("POST /agent/containers/deploy "));
    assert!(request.is_sealed(), "{}", request.head);
    let raw = request.raw();
    for readable in [SECRET, VARIABLE, "API_KEY", "PUBLIC_NAME", "app-1"] {
        assert!(
            !raw.contains(readable),
            "{readable} is readable on the wire"
        );
    }

    let opened = open_request(&private, "/agent/containers/deploy", &request.body).unwrap();
    let opened: DeployRequest = serde_json::from_slice(&opened).unwrap();
    assert_eq!(opened.secrets["API_KEY"], SECRET);
    assert_eq!(opened.environment_vars["PUBLIC_NAME"], VARIABLE);
    assert_eq!(opened.container_name, "app-1");
    assert_eq!(opened.image_name, "registry.example/app:v1");
}

#[tokio::test]
async fn an_entrypoint_deploy_is_sealed_for_the_route_it_goes_to() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", DEPLOYED).await;
    let mut deploy = deploy_request(true);
    deploy.entrypoint = Some(vec!["/run".to_string()]);

    deploy_and_record(url, NodeSealing::Key(public), deploy).await;

    let request = agent.await.unwrap();
    assert!(request
        .head
        .starts_with("POST /agent/v2/containers/deploy "));
    assert!(open_request(&private, "/agent/v2/containers/deploy", &request.body).is_ok());
    assert!(open_request(&private, "/agent/containers/deploy", &request.body).is_err());
}

#[tokio::test]
async fn values_are_never_sent_to_a_node_that_cannot_receive_them_sealed() {
    for (sealing, expected) in [
        (NodeSealing::NoKey, "no encryption key"),
        (NodeSealing::PlainHttp, "plain http://"),
    ] {
        // Nothing listens here: a refusal must come before any connection, so
        // it can't be mistaken for a network failure.
        let error = deployer("http://127.0.0.1:9".to_string(), sealing)
            .deploy_container(deploy_request(true))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("worker-a"), "{error}");
    }
}

/// A value does not have to be in the environment: this deploy has none, and
/// its secret is in the command.
fn deploy_with_a_secret_in_its_command() -> DeployRequest {
    let mut deploy = deploy_request(false);
    deploy.command = Some(vec![
        "app".to_string(),
        "--token".to_string(),
        SECRET.to_string(),
    ]);
    deploy
}

#[tokio::test]
async fn a_value_in_a_deploy_command_crosses_the_wire_encrypted() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", DEPLOYED).await;

    deploy_and_record(
        url,
        NodeSealing::Key(public),
        deploy_with_a_secret_in_its_command(),
    )
    .await;

    let request = agent.await.unwrap();
    assert!(request.is_sealed(), "{}", request.head);
    assert!(!request.raw().contains(SECRET));
    let opened = open_request(&private, "/agent/containers/deploy", &request.body).unwrap();
    assert!(String::from_utf8(opened).unwrap().contains(SECRET));
}

#[tokio::test]
async fn a_deploy_with_no_variables_is_not_sent_to_a_node_that_cannot_receive_it_sealed() {
    for (sealing, expected) in [
        (NodeSealing::NoKey, "no encryption key"),
        (NodeSealing::PlainHttp, "plain http://"),
    ] {
        let error = deployer("http://127.0.0.1:9".to_string(), sealing)
            .deploy_container(deploy_with_a_secret_in_its_command())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

const EXECUTED: &str =
    r#"{"success":true,"data":{"exit_code":0,"stdout":"","stderr":""},"error":null}"#;

#[tokio::test]
async fn a_credential_in_a_container_exec_crosses_the_wire_encrypted() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", EXECUTED).await;

    deployer(url, NodeSealing::Key(public))
        .exec_command("app-1", vec!["login".to_string(), SECRET.to_string()], None)
        .await
        .unwrap();

    let request = agent.await.unwrap();
    assert!(request.is_sealed(), "{}", request.head);
    assert!(!request.raw().contains(SECRET));
    assert!(open_request(&private, "/agent/containers/app-1/exec", &request.body).is_ok());
}

#[tokio::test]
async fn a_credential_in_a_health_command_crosses_the_wire_encrypted() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", EXECUTED).await;

    deployer(url, NodeSealing::Key(public))
        .exec_in_container(
            "app-1",
            vec!["curl".to_string(), SECRET.to_string()],
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();

    let request = agent.await.unwrap();
    assert!(request.is_sealed(), "{}", request.head);
    assert!(!request.raw().contains(SECRET));
    assert!(open_request(
        &private,
        "/agent/containers/app-1/health-exec",
        &request.body
    )
    .is_ok());
}

/// A checkout whose Dockerfile reads `API_URL`, optionally given a value for it.
fn build_request(source: &std::path::Path, value: Option<&str>) -> crate::BuildRequest {
    std::fs::write(
        source.join("Dockerfile"),
        "FROM scratch\nARG API_URL\nCOPY app /app\n",
    )
    .unwrap();
    std::fs::write(source.join("app"), "hello").unwrap();
    crate::BuildRequest {
        labels: Default::default(),
        build_secrets: Default::default(),
        cache_from: Vec::new(),
        image_name: "web:worker".to_string(),
        context_path: source.to_path_buf(),
        dockerfile_path: None,
        build_args: value
            .map(|value| HashMap::from([("API_URL".to_string(), value.to_string())]))
            .unwrap_or_default(),
        build_args_buildkit: Default::default(),
        platform: None,
        log_path: source.join("build.log"),
    }
}

#[tokio::test]
async fn build_argument_values_cross_the_wire_encrypted() {
    use crate::ImageBuilder;
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("500 Internal Server Error", "{}").await;
    let source = tempfile::tempdir().unwrap();

    let _ = deployer(url, NodeSealing::Key(public))
        .build_image(build_request(source.path(), Some(VARIABLE)))
        .await;

    let request = agent.await.unwrap();
    assert!(request.head.starts_with("POST /agent/images/build "));
    assert!(!request.raw().contains(VARIABLE), "readable on the wire");
    assert!(!request.raw().contains("name=\"spec\""));
    let sealed = multipart_field(&request, SEALED_BUILD_SPEC_FIELD);
    let opened = open_request(&private, BUILD_PATH, &sealed).unwrap();
    let spec: BuildSpec = serde_json::from_slice(&opened).unwrap();
    assert_eq!(spec.build_args["API_URL"], VARIABLE);
    assert!(open_request(&private, "/agent/services", &sealed).is_err());
}

#[tokio::test]
async fn build_argument_values_are_not_sent_to_a_node_that_cannot_receive_them_sealed() {
    use crate::ImageBuilder;
    for (sealing, expected) in [
        (NodeSealing::NoKey, "no encryption key"),
        (NodeSealing::PlainHttp, "plain http://"),
    ] {
        let source = tempfile::tempdir().unwrap();
        let error = deployer("http://127.0.0.1:9".to_string(), sealing)
            .build_image(build_request(source.path(), Some(VARIABLE)))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[tokio::test]
async fn a_build_without_arguments_stays_plain_so_older_agents_keep_building() {
    use crate::ImageBuilder;
    let (url, agent) = recording_agent("500 Internal Server Error", "{}").await;
    let source = tempfile::tempdir().unwrap();

    let _ = deployer(url, NodeSealing::NoKey)
        .build_image(build_request(source.path(), None))
        .await;

    let spec = multipart_field(&agent.await.unwrap(), "spec");
    let spec: BuildSpec = serde_json::from_slice(&spec).unwrap();
    assert_eq!(spec.image_name, "web:worker");
}

#[tokio::test]
async fn an_agent_from_before_sealing_refuses_a_sealed_build_with_an_upgrade_message() {
    use crate::ImageBuilder;
    let (_, public) = node_keypair();
    let (url, _agent) = recording_agent(
        "400 Bad Request",
        r#"{"detail":"Expected spec field first"}"#,
    )
    .await;
    let source = tempfile::tempdir().unwrap();

    let error = deployer(url, NodeSealing::Key(public))
        .build_image(build_request(source.path(), Some(VARIABLE)))
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("too old"), "{error}");
    assert!(error.contains("worker-a"), "{error}");
}

#[tokio::test]
async fn an_older_agent_gets_an_upgrade_message_and_no_deploy_runs() {
    let (_, public) = node_keypair();
    let (url, agent) = recording_agent("415 Unsupported Media Type", "").await;

    let error = deployer(url, NodeSealing::Key(public))
        .deploy_container(deploy_request(true))
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("too old"), "{error}");
    assert!(error.contains("worker-a"), "{error}");
    assert!(agent.await.unwrap().is_sealed());
}

fn credentials() -> RemotePullCredentials {
    RemotePullCredentials {
        username: Some("deployer".to_string()),
        password: Some(SECRET.to_string()),
        identity_token: None,
        server_address: Some("registry.example".to_string()),
    }
}

const PULLED: &str =
    r#"{"success":true,"data":{"image_id":"sha256:abc","digest":null},"error":null}"#;

#[tokio::test]
async fn registry_credentials_cross_the_wire_encrypted() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", PULLED).await;

    let image_id = deployer(url, NodeSealing::Key(public))
        .pull_image_from_registry("registry.example/app:v1", Some(credentials()))
        .await
        .unwrap();
    assert_eq!(image_id, "sha256:abc");

    let request = agent.await.unwrap();
    assert!(request.is_sealed(), "{}", request.head);
    assert!(!request.raw().contains(SECRET));
    let opened = open_request(&private, "/agent/images/pull", &request.body).unwrap();
    assert!(String::from_utf8(opened).unwrap().contains(SECRET));
}

#[tokio::test]
async fn registry_credentials_are_not_sent_to_a_node_without_a_key() {
    let error = deployer("http://127.0.0.1:9".to_string(), NodeSealing::NoKey)
        .pull_image_from_registry("registry.example/app:v1", Some(credentials()))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no encryption key"), "{error}");
}

#[tokio::test]
async fn a_public_image_pull_stays_plain() {
    let (url, agent) = recording_agent("200 OK", PULLED).await;
    deployer(url, NodeSealing::NoKey)
        .pull_image_from_registry("registry.example/app:v1", None)
        .await
        .unwrap();
    assert!(!agent.await.unwrap().is_sealed());
}

#[tokio::test]
async fn a_registry_transfer_seals_its_credentials() {
    let (private, public) = node_keypair();
    let (url, agent) =
        recording_agent("200 OK", r#"{"success":true,"data":true,"error":null}"#).await;

    deployer(url, NodeSealing::Key(public))
        .fetch_registry_image("registry.example/app:v1", "app:v1", Some(credentials()))
        .await
        .unwrap();

    let request = agent.await.unwrap();
    assert!(request.is_sealed(), "{}", request.head);
    assert!(!request.raw().contains(SECRET));
    assert!(open_request(&private, "/agent/images/fetch", &request.body).is_ok());
}

fn compose_spec() -> ComposePrepareSpec {
    serde_json::from_value(serde_json::json!({
        "version": 1,
        "project_name": "stack",
        "generation": "g1",
        "compose_content": "services: {}",
        "env_content": format!("FROM_DOTENV={VARIABLE}"),
        "compose_path": null,
        "environment_vars": { "PUBLIC_NAME": VARIABLE },
        "secrets": { "API_KEY": SECRET },
        "secret_compose_services": {},
        "build_args": {},
        "labels": {},
        "compose_override": null,
        "relaxed_capability_services": [],
        "unsandboxed_services": [],
        "ready_timeout_secs": null,
        "routed_services": [],
        "policy": {},
        "has_stack": false,
    }))
    .unwrap()
}

fn multipart_field(request: &Recorded, name: &str) -> Vec<u8> {
    let marker = format!("name=\"{name}\"");
    let start = request
        .body
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .unwrap_or_else(|| panic!("no {name} part in {}", request.raw()));
    let rest = &request.body[start..];
    let begin = rest.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let rest = &rest[begin..];
    let end = rest.windows(4).position(|w| w == b"\r\n--").unwrap();
    rest[..end].to_vec()
}

#[tokio::test]
async fn a_compose_spec_crosses_the_wire_encrypted() {
    let (private, public) = node_keypair();
    let (url, agent) = recording_agent("200 OK", "").await;

    let _ = deployer(url, NodeSealing::Key(public))
        .compose_prepare(&compose_spec(), None, None)
        .await;

    let request = agent.await.unwrap();
    let raw = request.raw();
    for readable in [SECRET, VARIABLE, "API_KEY", "FROM_DOTENV"] {
        assert!(
            !raw.contains(readable),
            "{readable} is readable on the wire"
        );
    }
    assert!(!raw.contains("name=\"spec\""), "{raw}");

    let sealed = multipart_field(&request, SEALED_COMPOSE_SPEC_FIELD);
    let opened = open_request(&private, COMPOSE_PREPARE_PATH, &sealed).unwrap();
    let opened: ComposePrepareSpec = serde_json::from_slice(&opened).unwrap();
    assert_eq!(opened.secrets["API_KEY"], SECRET);
    assert_eq!(opened.environment_vars["PUBLIC_NAME"], VARIABLE);
}

#[tokio::test]
async fn a_compose_spec_is_not_sent_to_a_node_without_a_key() {
    let error = deployer("http://127.0.0.1:9".to_string(), NodeSealing::NoKey)
        .compose_prepare(&compose_spec(), None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no encryption key"), "{error}");
}

#[tokio::test]
async fn an_older_agent_refusing_a_sealed_compose_spec_gets_an_upgrade_message() {
    let (_, public) = node_keypair();
    let (url, agent) = recording_agent(
        "400 Bad Request",
        r#"{"success":false,"data":null,"error":"Expected spec field first"}"#,
    )
    .await;

    let error = deployer(url, NodeSealing::Key(public))
        .compose_prepare(&compose_spec(), None, None)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("too old"), "{error}");
    let _ = agent.await;
}
