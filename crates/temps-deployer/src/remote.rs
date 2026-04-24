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
        })
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
        _container_id: &str,
    ) -> Result<ContainerStats, DeployerError> {
        Err(DeployerError::Other(
            "Stats not yet supported on remote nodes".into(),
        ))
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
        // Unknown for remote — will need to query agent in Phase 2
        "linux/amd64".to_string()
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
    async fn test_get_container_stats_not_supported() {
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        let result = deployer.get_container_stats("test-container").await;
        assert!(result.is_err());
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
        let deployer = RemoteNodeDeployer::new(
            "https://10.100.0.2:3100".to_string(),
            "token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap();
        assert_eq!(deployer.get_native_platform(), "linux/amd64");
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
