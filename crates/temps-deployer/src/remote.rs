//! Remote node deployer — implements `ContainerDeployer` and `ImageBuilder`
//! by calling the agent's HTTP API on a remote worker node.
//!
//! From `WorkflowExecutionService`'s perspective, deploying to a remote node
//! is identical to deploying locally.

use async_trait::async_trait;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    BuildRequest, BuildRequestWithCallback, BuildResult, BuilderError, ContainerDeployer,
    ContainerInfo, ContainerStats, DeployRequest, DeployResult, DeployerError, ImageBuilder,
    ImageInfo,
};

/// Response envelope from the agent API.
#[derive(Deserialize)]
struct AgentResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

/// Deploys containers to a remote node by calling its agent HTTP API.
pub struct RemoteNodeDeployer {
    /// Base URL of the agent, e.g. "https://10.100.0.2:3100"
    agent_url: String,
    /// Bearer token for authentication
    token: String,
    /// Node name (for error messages)
    node_name: String,
    /// HTTP client with timeouts
    client: reqwest::Client,
    /// Container platform of the remote node's Docker daemon
    /// (`linux/amd64`, `linux/arm64`), as recorded on the `nodes` row.
    ///
    /// This is what makes `get_native_platform` truthful for a remote node.
    /// Set via [`Self::with_platform`] by the caller that already loaded the
    /// node row, so the common path costs no extra round-trip; when it is
    /// `None` (node never reported, e.g. a pre-multi-arch agent), it can be
    /// filled in from the agent's health endpoint with
    /// [`Self::refresh_platform`].
    platform: std::sync::Arc<std::sync::OnceLock<String>>,
}

impl RemoteNodeDeployer {
    pub fn new(agent_url: String, token: String, node_name: String) -> Result<Self, DeployerError> {
        // Strict TLS by default; operators with self-signed agent certs
        // on a trusted internal network can opt in via the
        // `insecure_tls` toggle in the application settings UI.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(temps_core::tls::insecure_tls_enabled())
            .build()
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to create HTTP client for node {}: {}",
                    node_name, e
                ))
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            platform: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Construct a deployer that talks to the agent over **mutual TLS**
    /// (ADR-020 WS-2.1): the control plane presents `client_identity_pem`
    /// (its leaf cert + key, signed by the cluster CA) and pins the agent's
    /// server cert to the cluster CA (`ca_cert_pem`). Built-in roots are
    /// disabled so ONLY the cluster CA is trusted. Used for nodes whose
    /// `agent_url` is `https://`.
    pub fn new_mtls(
        agent_url: String,
        token: String,
        node_name: String,
        client_identity_pem: &str,
        ca_cert_pem: &str,
    ) -> Result<Self, DeployerError> {
        let identity =
            reqwest::Identity::from_pem(client_identity_pem.as_bytes()).map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Invalid control-plane client identity for node {}: {}",
                    node_name, e
                ))
            })?;
        let ca = reqwest::Certificate::from_pem(ca_cert_pem.as_bytes()).map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid cluster CA certificate for node {}: {}",
                node_name, e
            ))
        })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(ca)
            .tls_built_in_root_certs(false)
            .build()
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to create mTLS client for node {}: {}",
                    node_name, e
                ))
            })?;

        Ok(Self {
            agent_url,
            token,
            node_name,
            client,
            platform: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Record the node's container platform, as stored on its `nodes` row.
    ///
    /// A `None` or blank value leaves the platform unknown — the caller can
    /// then fall back to [`Self::refresh_platform`], which asks the agent.
    pub fn with_platform(self, platform: Option<String>) -> Self {
        if let Some(platform) = platform {
            let platform = platform.trim();
            if !platform.is_empty() {
                let _ = self
                    .platform
                    .set(crate::platform::canonicalize_platform(platform));
            }
        }
        self
    }

    /// Ask the agent for its platform and cache it.
    ///
    /// Only needed when the `nodes` row has no architecture yet (agent older
    /// than multi-arch support, or upgraded but not yet heartbeated). Returns
    /// `None` when the agent is unreachable or reports nothing usable — the
    /// caller must then decide whether to proceed, not silently assume amd64.
    pub async fn refresh_platform(&self) -> Option<String> {
        if let Some(cached) = self.platform.get() {
            return Some(cached.clone());
        }

        #[derive(Deserialize)]
        struct HealthPlatform {
            #[serde(default)]
            platform: String,
        }

        let health: HealthPlatform = match self.agent_get("/agent/health").await {
            Ok(health) => health,
            Err(e) => {
                tracing::warn!(
                    node = %self.node_name,
                    "Could not read platform from agent health endpoint: {}",
                    e
                );
                return None;
            }
        };

        let reported = health.platform.trim();
        if reported.is_empty() {
            return None;
        }

        let platform = crate::platform::canonicalize_platform(reported);
        let _ = self.platform.set(platform.clone());
        Some(platform)
    }

    /// The node's platform if known, without contacting the agent.
    pub fn platform(&self) -> Option<String> {
        self.platform.get().cloned()
    }

    /// Helper to make authenticated GET requests to the agent.
    async fn agent_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<T, DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let body: AgentResponse<T> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error ({}): {}",
                self.node_name,
                status,
                body.error.unwrap_or_default()
            )));
        }

        body.data.ok_or_else(|| {
            DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned success but no data at {}",
                self.node_name, url
            ))
        })
    }

    /// Helper to make authenticated POST requests to the agent.
    async fn agent_post<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let body: AgentResponse<T> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error ({}): {}",
                self.node_name,
                status,
                body.error.unwrap_or_default()
            )));
        }

        body.data.ok_or_else(|| {
            DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned success but no data at {}",
                self.node_name, url
            ))
        })
    }

    /// Helper to make authenticated DELETE requests to the agent.
    async fn agent_delete(&self, path: &str) -> Result<(), DeployerError> {
        let url = format!("{}{}", self.agent_url, path);
        let response = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| {
                DeployerError::NetworkError(format!(
                    "Failed to reach agent on node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;
        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(DeployerError::ContainerNotFound(path.to_string()));
        }

        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(DeployerError::ContainerNotFound(format!(
                "container at {} was not found on node {}",
                url, self.node_name
            )));
        }

        let body: AgentResponse<String> = response.json().await.map_err(|e| {
            DeployerError::NetworkError(format!(
                "Invalid response from node {} at {}: {}",
                self.node_name, url, e
            ))
        })?;

        if !body.success {
            return Err(DeployerError::DeploymentFailed(format!(
                "Agent on node {} returned error: {}",
                self.node_name,
                body.error.unwrap_or_default()
            )));
        }

        Ok(())
    }

    /// Get the node name this deployer targets.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Get the agent URL.
    pub fn agent_url(&self) -> &str {
        &self.agent_url
    }

    /// The bearer token used to authenticate with the agent. Exposed so
    /// the control plane can build a `Sec-WebSocket` upgrade request that
    /// matches the rest of the agent API auth.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Run a one-shot command in a container on the remote node and
    /// collect stdout/stderr + exit code. Mirrors the CP's local
    /// `exec_command` handler so the CP can pick the right path by
    /// `node_id` without a behavior change for callers.
    pub async fn exec_command(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<RemoteExecResult, DeployerError> {
        let body = serde_json::json!({
            "command": command,
            "timeout_seconds": timeout_seconds,
        });
        self.agent_post(&format!("/agent/containers/{}/exec", container_id), &body)
            .await
    }
}

/// Wire-compatible mirror of the agent's `AgentExecResponse`. Lives in the
/// deployer crate so the CP service layer can depend on it without pulling
/// in `temps-agent`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteExecResult {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
impl ContainerDeployer for RemoteNodeDeployer {
    async fn deploy_container(
        &self,
        request: DeployRequest,
    ) -> Result<DeployResult, DeployerError> {
        self.agent_post("/agent/containers/deploy", &request).await
    }

    async fn start_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let _: String = self
            .agent_post(
                &format!("/agent/containers/{}/start", container_id),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn stop_container(&self, container_id: &str) -> Result<(), DeployerError> {
        let _: String = self
            .agent_post(
                &format!("/agent/containers/{}/stop", container_id),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn pause_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Err(DeployerError::Other(
            "Pause not supported on remote nodes".into(),
        ))
    }

    async fn resume_container(&self, _container_id: &str) -> Result<(), DeployerError> {
        Err(DeployerError::Other(
            "Resume not supported on remote nodes".into(),
        ))
    }

    async fn remove_container(&self, container_id: &str) -> Result<(), DeployerError> {
        self.agent_delete(&format!("/agent/containers/{}", container_id))
            .await
    }

    async fn get_container_info(&self, container_id: &str) -> Result<ContainerInfo, DeployerError> {
        self.agent_get(&format!("/agent/containers/{}/info", container_id))
            .await
    }

    async fn get_container_stats(
        &self,
        container_id: &str,
    ) -> Result<ContainerStats, DeployerError> {
        self.agent_get(&format!("/agent/containers/{}/stats", container_id))
            .await
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        self.agent_get("/agent/containers").await
    }

    async fn get_container_logs(&self, container_id: &str) -> Result<String, DeployerError> {
        self.agent_get(&format!("/agent/containers/{}/logs", container_id))
            .await
    }

    async fn stream_container_logs(
        &self,
        _container_id: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
        Err(DeployerError::Other(
            "Log streaming not yet supported on remote nodes".into(),
        ))
    }

    async fn image_exists(&self, image_name: &str) -> Result<bool, DeployerError> {
        self.agent_get(&format!(
            "/agent/images/{}/exists",
            urlencoding::encode(image_name)
        ))
        .await
    }
}

#[async_trait]
impl ImageBuilder for RemoteNodeDeployer {
    async fn build_image(&self, _request: BuildRequest) -> Result<BuildResult, BuilderError> {
        Err(BuilderError::Other(
            "Remote image building not supported — images are transferred via tar".into(),
        ))
    }

    async fn build_image_with_callback(
        &self,
        _request: BuildRequestWithCallback,
    ) -> Result<BuildResult, BuilderError> {
        Err(BuilderError::Other(
            "Remote image building not supported — images are transferred via tar".into(),
        ))
    }

    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        tracing::info!(
            node = %self.node_name,
            image = %tag,
            "Transferring image tar to remote node"
        );

        let file = tokio::fs::File::open(&image_path).await.map_err(|e| {
            BuilderError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to open image tar {:?}: {}", image_path, e),
            ))
        })?;

        let file_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);

        let stream = tokio_util::codec::FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream.map_ok(|b| b.freeze()));

        let url = format!("{}/agent/images/import", self.agent_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header("content-type", "application/x-tar")
            .header("x-image-tag", tag)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                BuilderError::Other(format!(
                    "Failed to transfer image to node {} at {}: {}",
                    self.node_name, url, e
                ))
            })?;

        let status = response.status();
        let resp_body: AgentResponse<String> = response.json().await.map_err(|e| {
            BuilderError::Other(format!(
                "Invalid response from node {} during image import: {}",
                self.node_name, e
            ))
        })?;

        if !resp_body.success {
            return Err(BuilderError::Other(format!(
                "Image import failed on node {} ({}): {}",
                self.node_name,
                status,
                resp_body.error.unwrap_or_default()
            )));
        }

        tracing::info!(
            node = %self.node_name,
            image = %tag,
            size_mb = format!("{:.1}", file_size as f64 / 1_048_576.0),
            "Image transferred successfully"
        );

        Ok(resp_body.data.unwrap_or_else(|| tag.to_string()))
    }

    async fn save_image(&self, _image_name: &str, _output_path: &Path) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Save image not supported on remote nodes — images are saved on the control plane"
                .into(),
        ))
    }

    async fn extract_from_image(
        &self,
        _image_name: &str,
        _source_path: &str,
        _destination_path: &Path,
    ) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Extract from image not supported on remote nodes".into(),
        ))
    }

    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        Err(BuilderError::Other(
            "List images not supported on remote nodes".into(),
        ))
    }

    async fn remove_image(&self, _image_name: &str) -> Result<(), BuilderError> {
        Err(BuilderError::Other(
            "Remove image not supported on remote nodes".into(),
        ))
    }

    async fn inspect_image(&self, _image_name: &str) -> Result<ImageInfo, BuilderError> {
        Err(BuilderError::Other(
            "Inspect image not supported on remote nodes".into(),
        ))
    }

    fn get_native_platform(&self) -> String {
        // Known from the node row (or a health-endpoint refresh). Falling back
        // to the control plane's own platform when it isn't known keeps the
        // historical behaviour for pre-multi-arch agents: assume compatible
        // rather than block the deploy. Callers that need certainty check
        // `platform()` for `None` and warn.
        self.platform
            .get()
            .cloned()
            .unwrap_or_else(crate::platform::native_platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_node_deployer_creation() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "test-token".to_string(),
            "worker-1".to_string(),
        );
        assert!(deployer.is_ok());
        let deployer = deployer.unwrap();
        assert_eq!(deployer.node_name(), "worker-1");
        assert_eq!(deployer.agent_url(), "https://10.100.0.2:3100");
    }

    #[test]
    fn test_remote_node_deployer_accessors() {
        let deployer = RemoteNodeDeployer::new(
            "https://worker-3.internal:3100".to_string(),
            "secret-token".to_string(),
            "worker-3".to_string(),
        )
        .unwrap();
        assert_eq!(deployer.node_name(), "worker-3");
        assert_eq!(deployer.agent_url(), "https://worker-3.internal:3100");
    }

    #[tokio::test]
    async fn remove_container_maps_agent_not_found_to_typed_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("mock agent address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read request");
            let body = r#"{"success":false,"data":null,"error":"container missing"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let deployer = RemoteNodeDeployer::new(
            format!("http://{address}"),
            "test-token".to_string(),
            "worker-test".to_string(),
        )
        .expect("create remote deployer");
        let result = deployer.remove_container("already-gone").await;
        assert!(matches!(result, Err(DeployerError::ContainerNotFound(_))));
        server.await.expect("mock agent task");
    }

    /// Live **REAL DEPLOYMENT** over mTLS: drives the production
    /// `RemoteNodeDeployer::deploy_container` to actually create + start a
    /// container on the worker's Docker through the mutual-TLS channel — the
    /// genuine end-to-end deploy path, not just a control-plane round-trip.
    /// Gated on `TEMPS_MTLS_DEPLOY_IMAGE` (the image, which MUST already be
    /// present in the worker's Docker, e.g. pre-pulled) plus the same
    /// `TEMPS_MTLS_*` connection env. Run inside a cluster container.
    #[tokio::test]
    async fn test_mtls_real_deploy_live() {
        let image = match std::env::var("TEMPS_MTLS_DEPLOY_IMAGE") {
            Ok(i) => i,
            Err(_) => {
                eprintln!("TEMPS_MTLS_DEPLOY_IMAGE not set — skipping real mTLS deploy test");
                return;
            }
        };
        let (url, token, cert, key, ca) = match (
            std::env::var("TEMPS_MTLS_AGENT_URL"),
            std::env::var("TEMPS_MTLS_TOKEN"),
            std::env::var("TEMPS_MTLS_CERT"),
            std::env::var("TEMPS_MTLS_KEY"),
            std::env::var("TEMPS_MTLS_CA"),
        ) {
            (Ok(u), Ok(t), Ok(c), Ok(k), Ok(a)) => (u, t, c, k, a),
            _ => {
                eprintln!("TEMPS_MTLS_* not set — skipping real mTLS deploy test");
                return;
            }
        };
        let cert_pem = std::fs::read_to_string(&cert).expect("read client cert PEM");
        let key_pem = std::fs::read_to_string(&key).expect("read client key PEM");
        let ca_pem = std::fs::read_to_string(&ca).expect("read cluster CA PEM");
        let identity = format!("{}\n{}", cert_pem.trim(), key_pem.trim());

        let deployer = RemoteNodeDeployer::new_mtls(
            url.clone(),
            token,
            "mtls-deploy-probe".to_string(),
            &identity,
            &ca_pem,
        )
        .expect("build mTLS deployer");

        // Deployed containers run with cap_drop:ALL + no-new-privileges, so the
        // probe image must be unprivileged-friendly (no startup chown, high
        // port). Port + command are env-configurable to fit such an image.
        let container_port: u16 = std::env::var("TEMPS_MTLS_DEPLOY_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(80);
        let command = std::env::var("TEMPS_MTLS_DEPLOY_CMD")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.split(' ').map(|x| x.to_string()).collect::<Vec<_>>());

        let name = "mtls-deploy-probe".to_string();
        let req = DeployRequest {
            image_name: image.clone(),
            container_name: name.clone(),
            environment_vars: std::collections::HashMap::new(),
            secrets: std::collections::HashMap::new(),
            port_mappings: vec![crate::PortMapping {
                host_port: 18080,
                container_port,
                protocol: crate::Protocol::Tcp,
            }],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: crate::ResourceLimits::default(),
            restart_policy: crate::RestartPolicy::Never,
            log_path: std::path::PathBuf::from("/tmp/mtls-deploy-probe.log"),
            command,
            log_config: None,
            labels: std::collections::HashMap::new(),
        };

        let result = deployer
            .deploy_container(req)
            .await
            .expect("deploy_container over mTLS must succeed");
        eprintln!(
            "✓ REAL deploy over mTLS: image={} container_id={} status={:?} host_port={}",
            image, result.container_id, result.status, result.host_port
        );
        assert!(
            !result.container_id.is_empty(),
            "expected a real container id"
        );
    }

    /// Live end-to-end check of the control-plane reqwest+rustls **mutual TLS**
    /// client against a real agent serving mTLS (ADR-020 WS-2.1). Unlike the
    /// curl-based `verify-mtls.sh` harness (which proves the *agent* side), this
    /// drives the exact production client path — `new_mtls` loading a CA-signed
    /// client identity, pinning the cluster CA, completing the rustls handshake,
    /// and round-tripping an authenticated request. Skips gracefully unless the
    /// `TEMPS_MTLS_*` env is set (so normal `cargo test` runs are unaffected);
    /// run it inside a cluster container that can reach the agent. A client cert
    /// SAN is irrelevant here (client certs aren't hostname-checked), so any
    /// CA-signed leaf faithfully stands in for the control plane's identity.
    #[tokio::test]
    async fn test_mtls_deploy_channel_live() {
        let (url, token, cert, key, ca) = match (
            std::env::var("TEMPS_MTLS_AGENT_URL"),
            std::env::var("TEMPS_MTLS_TOKEN"),
            std::env::var("TEMPS_MTLS_CERT"),
            std::env::var("TEMPS_MTLS_KEY"),
            std::env::var("TEMPS_MTLS_CA"),
        ) {
            (Ok(u), Ok(t), Ok(c), Ok(k), Ok(a)) => (u, t, c, k, a),
            _ => {
                eprintln!("TEMPS_MTLS_* not set — skipping live mTLS deploy-channel test");
                return;
            }
        };

        let cert_pem = std::fs::read_to_string(&cert).expect("read client cert PEM");
        let key_pem = std::fs::read_to_string(&key).expect("read client key PEM");
        let ca_pem = std::fs::read_to_string(&ca).expect("read cluster CA PEM");
        // reqwest's PEM Identity wants the cert chain followed by the key — the
        // same layout `cluster_ca::cp_client_identity` produces in production.
        let identity = format!("{}\n{}", cert_pem.trim(), key_pem.trim());

        let deployer = RemoteNodeDeployer::new_mtls(
            url.clone(),
            token,
            "mtls-live-test".to_string(),
            &identity,
            &ca_pem,
        )
        .expect("build mTLS deployer");

        // A lightweight authenticated read that fully round-trips the mutual-TLS
        // channel: TLS handshake (server cert validated against the pinned CA +
        // our client cert presented), bearer auth, JSON response.
        let containers = deployer
            .list_containers()
            .await
            .expect("list_containers over mTLS must succeed");
        eprintln!(
            "✓ mTLS deploy channel live: agent {} returned {} container(s)",
            url,
            containers.len()
        );
    }

    #[tokio::test]
    async fn test_pause_container_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.pause_container("test-container").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DeployerError::Other(_)));
    }

    #[tokio::test]
    async fn test_resume_container_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.resume_container("test-container").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DeployerError::Other(_)));
    }

    #[tokio::test]
    async fn test_get_container_stats_returns_network_error_for_unreachable_agent() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.get_container_stats("test-container").await;
        // Stats now hit the real `/agent/containers/{id}/stats` endpoint.
        // With an unreachable address the call must surface a network error
        // (used to be a hard-coded "not supported" before the endpoint
        // existed).
        assert!(matches!(
            result.unwrap_err(),
            DeployerError::NetworkError(_)
        ));
    }

    #[tokio::test]
    async fn test_list_containers_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.list_containers().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_image_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer
            .build_image(BuildRequest {
                image_name: "test:latest".to_string(),
                context_path: PathBuf::from("/tmp"),
                dockerfile_path: None,
                build_args: std::collections::HashMap::new(),
                build_args_buildkit: std::collections::HashMap::new(),
                platform: None,
                log_path: PathBuf::from("/tmp/build.log"),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_import_image_missing_file_returns_error() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer
            .import_image(PathBuf::from("/tmp/nonexistent-image.tar"), "test:latest")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_save_image_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer
            .save_image("test:latest", Path::new("/tmp/out.tar"))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_get_native_platform() {
        // No platform known yet: fall back to the control plane's own, which
        // is what the historical hardcoded value effectively assumed.
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        assert_eq!(
            deployer.get_native_platform(),
            crate::platform::native_platform()
        );
        assert_eq!(deployer.platform(), None);
    }

    /// The whole point of the change: a node's platform must be reported
    /// truthfully, not assumed to be the control plane's.
    #[test]
    fn test_with_platform_reports_the_nodes_architecture() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-arm".to_string(),
        )
        .unwrap()
        .with_platform(Some("linux/arm64".to_string()));

        assert_eq!(deployer.get_native_platform(), "linux/arm64");
        assert_eq!(deployer.platform().as_deref(), Some("linux/arm64"));
    }

    #[test]
    fn test_with_platform_canonicalizes_and_ignores_blanks() {
        let make = |platform: Option<&str>| {
            RemoteNodeDeployer::new(
                "https://10.100.0.2:3100".to_string(),
                "token".to_string(),
                "worker-1".to_string(),
            )
            .unwrap()
            .with_platform(platform.map(|p| p.to_string()))
        };

        // Docker's kernel spelling is normalized to the OCI one.
        assert_eq!(
            make(Some("linux/aarch64")).platform().as_deref(),
            Some("linux/arm64")
        );
        // Blank/whitespace means "unknown", not a platform named "".
        assert_eq!(make(Some("   ")).platform(), None);
        assert_eq!(make(None).platform(), None);
    }

    /// Spawn a throwaway HTTP server that answers `GET /agent/health` like a
    /// real agent would. Returns its base URL.
    ///
    /// Hand-rolled rather than pulled from a framework: this crate has no HTTP
    /// server dependency and one canned response doesn't justify adding one.
    async fn spawn_fake_agent(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Read (and discard) the request head; we only ever serve
                    // one route.
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn test_refresh_platform_reads_agent_health() {
        // A node that never reported its architecture at registration time —
        // the pre-multi-arch agent case — can still be identified by asking it.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":"linux/aarch64"},"error":null}"#,
        )
        .await;

        let deployer =
            RemoteNodeDeployer::new(url, "token".to_string(), "worker-arm".to_string()).unwrap();

        assert_eq!(
            deployer.refresh_platform().await.as_deref(),
            Some("linux/arm64")
        );
        // Cached afterwards, so the deploy path pays at most one round-trip.
        assert_eq!(deployer.platform().as_deref(), Some("linux/arm64"));
        assert_eq!(deployer.get_native_platform(), "linux/arm64");
    }

    #[tokio::test]
    async fn test_refresh_platform_returns_none_when_agent_omits_it() {
        // An agent too old to report a platform must leave us with `None` —
        // "unknown" — never a guess that would silently pass a compatibility
        // check.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":""},"error":null}"#,
        )
        .await;

        let deployer =
            RemoteNodeDeployer::new(url, "token".to_string(), "legacy-worker".to_string()).unwrap();

        assert_eq!(deployer.refresh_platform().await, None);
        assert_eq!(deployer.platform(), None);
    }

    #[tokio::test]
    async fn test_refresh_platform_returns_none_when_agent_unreachable() {
        let deployer = RemoteNodeDeployer::new(
            "http://127.0.0.1:1".to_string(),
            "token".to_string(),
            "dead-worker".to_string(),
        )
        .unwrap();

        assert_eq!(deployer.refresh_platform().await, None);
    }

    #[tokio::test]
    async fn test_with_platform_wins_over_agent_query() {
        // When the node row already carries the architecture we must not spend
        // a round-trip; point the deployer at a server that would answer with
        // a different value and verify it is never consulted.
        let url = spawn_fake_agent(
            r#"{"success":true,"data":{"cpu_percent":1.0,"memory_used_bytes":1,"memory_total_bytes":2,"disk_used_bytes":1,"disk_total_bytes":2,"running_containers":0,"platform":"linux/amd64"},"error":null}"#,
        )
        .await;

        let deployer = RemoteNodeDeployer::new(url, "token".to_string(), "worker-arm".to_string())
            .unwrap()
            .with_platform(Some("linux/arm64".to_string()));

        assert_eq!(
            deployer.refresh_platform().await.as_deref(),
            Some("linux/arm64")
        );
    }

    #[tokio::test]
    async fn test_deploy_container_unreachable_returns_network_error() {
        let deployer = RemoteNodeDeployer::new(
            "https://192.0.2.1:3100".to_string(), // Non-routable address
            "token".to_string(),
            "test-node".to_string(),
        )
        .unwrap();

        let request = DeployRequest {
            image_name: "nginx:latest".to_string(),
            container_name: "test-container".to_string(),
            environment_vars: std::collections::HashMap::new(),
            secrets: std::collections::HashMap::new(),
            port_mappings: vec![],
            network_name: None,
            extra_networks: Vec::new(),
            resource_limits: crate::ResourceLimits::default(),
            restart_policy: crate::RestartPolicy::default(),
            log_path: PathBuf::from("/tmp/deploy.log"),
            command: None,
            log_config: None,
            labels: std::collections::HashMap::new(),
        };

        let result = deployer.deploy_container(request).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            DeployerError::NetworkError(msg) => {
                assert!(
                    msg.contains("test-node"),
                    "Error should mention node name: {}",
                    msg
                );
            }
            other => panic!("Expected NetworkError, got {:?}", other),
        }
    }
}
