//! Managed S3-compatible backend protocol.
//!
//! This module intentionally defines Garage support as an out-of-process
//! provider contract rather than a compiled `garage` implementation. Temps can
//! stay permissively licensed while provider binaries manage AGPL services over
//! standard process/API boundaries (S3, Garage admin HTTP/CLI, Docker, etc.).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

use super::HealthProbeResult;

pub const DEFAULT_MANAGED_S3_BACKEND: ManagedS3BackendKind = ManagedS3BackendKind::Rustfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ManagedS3BackendKind {
    Rustfs,
    Garage,
}

impl ManagedS3BackendKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "" | "rustfs" => Ok(Self::Rustfs),
            "garage" => Ok(Self::Garage),
            other => Err(anyhow!(
                "unsupported managed S3 backend '{}'; supported backends are rustfs and garage",
                other
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rustfs => "rustfs",
            Self::Garage => "garage",
        }
    }

    pub fn requires_external_provider(self) -> bool {
        matches!(self, Self::Garage)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3BackendSelection {
    #[serde(default = "default_backend_kind")]
    pub backend: ManagedS3BackendKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_socket: Option<String>,
}

impl Default for ManagedS3BackendSelection {
    fn default() -> Self {
        Self {
            backend: DEFAULT_MANAGED_S3_BACKEND,
            buckets: Vec::new(),
            provider_socket: None,
        }
    }
}

fn default_backend_kind() -> ManagedS3BackendKind {
    DEFAULT_MANAGED_S3_BACKEND
}

impl ManagedS3BackendSelection {
    pub fn from_parameters(parameters: &serde_json::Value) -> Result<Self> {
        let backend = parameters
            .get("backend")
            .and_then(|v| v.as_str())
            .map(ManagedS3BackendKind::parse)
            .transpose()?
            .unwrap_or(DEFAULT_MANAGED_S3_BACKEND);

        let buckets = parameters
            .get("buckets")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(|bucket| bucket.trim().to_string())
                    .filter(|bucket| !bucket.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let provider_socket = parameters
            .get("provider_socket")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        Ok(Self {
            backend,
            buckets,
            provider_socket,
        })
    }

    pub fn validate_for_service_create(&self) -> Result<()> {
        if self.backend.requires_external_provider() && self.provider_socket.is_none() {
            return Err(anyhow!(
                "managed S3 backend '{}' must be handled by an out-of-process provider; set provider_socket to the external provider Unix socket path",
                self.backend.as_str()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3ProvisionRequest {
    pub service_name: String,
    pub backend: ManagedS3BackendKind,
    #[serde(default)]
    pub buckets: Vec<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3ProvisionResponse {
    pub endpoint: String,
    pub region: String,
    #[serde(default)]
    pub parameters: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3AccessKeyRequest {
    pub service_name: String,
    pub project_id: String,
    pub environment: String,
    #[serde(default)]
    pub buckets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedS3AccessKeyResponse {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub bucket_env: HashMap<String, String>,
}

#[async_trait]
pub trait ManagedS3Backend: Send + Sync {
    fn kind(&self) -> ManagedS3BackendKind;
    async fn provision_service(
        &self,
        request: ManagedS3ProvisionRequest,
    ) -> Result<ManagedS3ProvisionResponse>;
    async fn create_bucket(&self, service_name: &str, bucket: &str) -> Result<()>;
    async fn create_access_key(
        &self,
        request: ManagedS3AccessKeyRequest,
    ) -> Result<ManagedS3AccessKeyResponse>;
    async fn render_runtime_env(
        &self,
        request: ManagedS3AccessKeyRequest,
    ) -> Result<HashMap<String, String>>;
    async fn health_check(&self, service_name: &str) -> Result<HealthProbeResult>;
    async fn backup(&self, service_name: &str, destination: &str) -> Result<()>;
    async fn restore(&self, service_name: &str, source: &str) -> Result<()>;
    async fn migrate_from(
        &self,
        service_name: &str,
        source_backend: ManagedS3BackendKind,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_is_rustfs() {
        let selection = ManagedS3BackendSelection::from_parameters(&serde_json::json!({})).unwrap();
        assert_eq!(selection.backend, ManagedS3BackendKind::Rustfs);
    }

    #[test]
    fn parses_garage_backend_and_buckets() {
        let selection = ManagedS3BackendSelection::from_parameters(&serde_json::json!({
            "backend": "garage",
            "provider_socket": "/run/temps/providers/garage.sock",
            "buckets": ["uploads", "private-files", ""]
        }))
        .unwrap();
        assert_eq!(selection.backend, ManagedS3BackendKind::Garage);
        assert_eq!(selection.buckets, vec!["uploads", "private-files"]);
        selection.validate_for_service_create().unwrap();
    }

    #[test]
    fn garage_requires_external_provider_socket() {
        let selection = ManagedS3BackendSelection::from_parameters(&serde_json::json!({
            "backend": "garage"
        }))
        .unwrap();
        let error = selection
            .validate_for_service_create()
            .unwrap_err()
            .to_string();
        assert!(error.contains("out-of-process provider"));
    }
}
