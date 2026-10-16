// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Garage (<https://garagehq.deuxfleurs.fr>) as a managed S3 backend.
//!
//! Garage is a small, single-binary, S3-compatible object store. Temps runs it
//! as one container per service with a single-node layout (replication factor
//! 1), generates its configuration, and administers it through the Garage
//! admin API (v2) from the control plane:
//!
//! * `garage.toml` is rendered by [`garage_toml`] and uploaded into the
//!   container before it starts. It contains no secrets: the RPC secret and
//!   the admin token are passed as `GARAGE_RPC_SECRET` / `GARAGE_ADMIN_TOKEN`.
//! * A fresh node has no role in the cluster layout and refuses writes, so
//!   [`GarageService::provision`] assigns it a role and applies the layout.
//! * The service's own S3 key (the "admin key" mirrored and backed up with) is
//!   imported with a Temps-chosen id and secret, so it is known up front.
//! * Every linked project environment gets its own key, derived from the
//!   service secret ([`super::managed_s3::derive_env_credentials`]) and
//!   granted read/write on exactly that environment's buckets.
//!
//! Garage never lets a deleted key id be imported again, so revoking an
//! environment denies the key on every bucket instead of deleting it; a later
//! re-link then re-grants the very same key.

use anyhow::Result;
use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use bollard::query_parameters::{InspectContainerOptions, StopContainerOptions};
use bollard::Docker;
use rand::RngExt;
use schemars::JsonSchema;
use sea_orm::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::EncryptionService;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::readiness::{ReadinessObservation, ReadinessPolicy, ReadinessTarget};
use crate::utils::ensure_network_exists;

use super::managed_s3::{
    derive_grant, CredentialFormat, ManagedS3AccessKeyRequest, ManagedS3Backend,
    ManagedS3BackendKind, ManagedS3BackendSelection, ManagedS3Endpoint, ManagedS3Error,
    ManagedS3Grant, S3Location,
};
use super::port_util::{find_available_port, find_available_port_async, is_port_available_async};
use super::{
    ExternalService, HealthProbeResult, ServiceConfig, ServiceResourceLimits, ServiceType,
};

/// Pinned Garage image. Tag-only on purpose (see `DEFAULT_RUSTFS_IMAGE`): the
/// version shown for a service is the part after `:`. Existing services keep
/// the image they were created with.
pub const DEFAULT_GARAGE_IMAGE: &str = "dxflrs/garage:v2.1.0";
const DEFAULT_GARAGE_VERSION: &str = "v2.1.0";
/// S3 API port inside the container.
pub const GARAGE_S3_PORT: u16 = 3900;
/// Inter-node RPC port inside the container. Never published: a single node
/// talks to itself.
pub const GARAGE_RPC_PORT: u16 = 3901;
/// Admin API port inside the container.
pub const GARAGE_ADMIN_PORT: u16 = 3903;

const GARAGE_DATA_MOUNT: &str = "/var/lib/garage";
const GARAGE_CONFIG_DIR: &str = "/etc";
const GARAGE_CONFIG_FILE: &str = "garage.toml";
const GARAGE_LAYOUT_ZONE: &str = "temps";
/// Nominal capacity assigned to the node. Garage uses it to weigh nodes
/// against each other, it is not a quota; a single node only needs it > 0.
const GARAGE_LAYOUT_CAPACITY_BYTES: u64 = 1 << 40;
const GARAGE_ADMIN_TIMEOUT: Duration = Duration::from_secs(10);
const GARAGE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const GARAGE_STARTUP_DEADLINE: Duration = Duration::from_secs(120);
const GARAGE_READINESS_DEADLINE: Duration = Duration::from_secs(120);
const GARAGE_READINESS_POLL_INTERVAL: Duration = Duration::from_secs(2);
const GARAGE_READINESS_MAX_RESTARTS: i64 = 3;
const BACKEND_NAME: &str = "garage";
/// Longest slice of an admin API error body quoted back in an error.
const ADMIN_BODY_EXCERPT_CHARS: usize = 300;

/// Shape Garage requires of S3 credentials: `GK` + 24 hex, 64 hex secret.
pub(crate) const GARAGE_CREDENTIAL_FORMAT: CredentialFormat = CredentialFormat {
    access_prefix: "GK",
    access_hex_len: 24,
    secret_hex_len: 64,
    upper_case_access: false,
};

/// Input configuration for creating a Garage service.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(
    title = "Garage Configuration",
    description = "Configuration for the Garage S3-compatible storage backend"
)]
pub struct GarageInputConfig {
    /// S3 API host port (auto-assigned if not provided)
    #[schemars(example = example_s3_port())]
    pub port: Option<String>,

    /// Admin API host port (auto-assigned if not provided)
    #[schemars(example = example_admin_port())]
    pub admin_port: Option<String>,

    /// Service access key id, `GK` + 24 hex (auto-generated if empty)
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    #[schemars(with = "Option<String>")]
    pub access_key: Option<String>,

    /// Service secret key, 64 hex (auto-generated if empty)
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    #[schemars(with = "Option<String>")]
    pub secret_key: Option<String>,

    /// Admin API bearer token (auto-generated; never shown)
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    #[schemars(skip)]
    pub admin_token: Option<String>,

    /// Cluster RPC secret, 64 hex (auto-generated; never shown)
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    #[schemars(skip)]
    pub rpc_secret: Option<String>,

    /// Host address
    #[serde(default = "default_host")]
    #[schemars(default = "default_host")]
    pub host: String,

    /// S3 region reported to clients
    #[serde(default = "default_region")]
    #[schemars(default = "default_region")]
    pub region: String,

    /// Docker image to use for Garage
    #[serde(default = "default_image")]
    #[schemars(default = "default_image", example = example_image())]
    pub docker_image: String,

    /// Always `garage`; accepted so a stored service round-trips.
    #[serde(default)]
    #[schemars(skip)]
    pub backend: Option<String>,

    /// Logical bucket names every linked environment receives.
    #[serde(default)]
    #[schemars(with = "Option<Vec<String>>")]
    pub buckets: Option<serde_json::Value>,
}

/// Runtime configuration of a Garage service, after defaults and generated
/// secrets have been filled in.
#[derive(Clone, Serialize, Deserialize)]
pub struct GarageConfig {
    pub port: String,
    pub admin_port: String,
    pub access_key: String,
    pub secret_key: String,
    pub admin_token: String,
    pub rpc_secret: String,
    pub host: String,
    pub region: String,
    pub docker_image: String,
}

/// Hand-written so a `{:?}` in a log line cannot print a secret.
impl std::fmt::Debug for GarageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GarageConfig")
            .field("port", &self.port)
            .field("admin_port", &self.admin_port)
            .field("access_key", &self.access_key)
            .field("secret_key", &"[REDACTED]")
            .field("admin_token", &"[REDACTED]")
            .field("rpc_secret", &"[REDACTED]")
            .field("host", &self.host)
            .field("region", &self.region)
            .field("docker_image", &self.docker_image)
            .finish()
    }
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_image() -> String {
    DEFAULT_GARAGE_IMAGE.to_string()
}

fn example_s3_port() -> &'static str {
    "3900"
}

fn example_admin_port() -> &'static str {
    "3903"
}

fn example_image() -> &'static str {
    DEFAULT_GARAGE_IMAGE
}

fn random_hex(len: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| HEX[rng.random_range(0..HEX.len())] as char)
        .collect()
}

pub(crate) fn generate_access_key() -> String {
    format!("GK{}", random_hex(24))
}

pub(crate) fn generate_secret_key() -> String {
    random_hex(64)
}

pub(crate) fn generate_admin_token() -> String {
    random_hex(48)
}

pub(crate) fn generate_rpc_secret() -> String {
    random_hex(64)
}

/// Docker container name of a Garage service named `name`.
pub(crate) fn garage_container_name(name: &str) -> String {
    format!("garage-{name}")
}

/// Docker volumes of a Garage service named `name`.
pub(crate) fn garage_volume_names(name: &str) -> [String; 1] {
    [format!("garage_{name}_data")]
}

/// Regions end up inside a TOML string, so only a conservative alphabet is
/// accepted rather than escaping arbitrary input.
fn validate_region(service: &str, region: &str) -> Result<(), ManagedS3Error> {
    let valid = !region.is_empty()
        && region.len() <= 64
        && region
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(ManagedS3Error::InvalidConfig {
            backend: BACKEND_NAME,
            service: service.to_string(),
            reason: format!(
                "region '{region}' must be 1-64 characters of letters, digits, '-' or '_'"
            ),
        })
    }
}

/// The `garage.toml` for a single-node Temps deployment.
///
/// Contains no secrets (those travel as environment variables), so it is safe
/// to log and to keep in a container layer.
pub(crate) fn garage_toml(service: &str, config: &GarageConfig) -> Result<String, ManagedS3Error> {
    validate_region(service, &config.region)?;
    Ok(format!(
        "metadata_dir = \"{data}/meta\"\n\
         data_dir = \"{data}/data\"\n\
         db_engine = \"lmdb\"\n\
         replication_factor = 1\n\
         \n\
         rpc_bind_addr = \"0.0.0.0:{rpc}\"\n\
         rpc_public_addr = \"127.0.0.1:{rpc}\"\n\
         \n\
         [s3_api]\n\
         s3_region = \"{region}\"\n\
         api_bind_addr = \"0.0.0.0:{s3}\"\n\
         \n\
         [admin]\n\
         api_bind_addr = \"0.0.0.0:{admin}\"\n",
        data = GARAGE_DATA_MOUNT,
        rpc = GARAGE_RPC_PORT,
        s3 = GARAGE_S3_PORT,
        admin = GARAGE_ADMIN_PORT,
        region = config.region,
    ))
}

impl GarageConfig {
    /// Fill defaults and generated secrets for a new or re-initialised
    /// service, moving off host ports that are taken.
    async fn from_input_async(input: GarageInputConfig, docker: &Docker) -> Self {
        let port = Self::pick_port(docker, input.port.as_deref(), GARAGE_S3_PORT).await;
        let admin_start = std::cmp::max(
            port.parse::<u16>()
                .unwrap_or(GARAGE_S3_PORT)
                .saturating_add(1),
            GARAGE_ADMIN_PORT,
        );
        let admin_port = Self::pick_port(docker, input.admin_port.as_deref(), admin_start).await;
        Self {
            port,
            admin_port,
            access_key: input.access_key.unwrap_or_else(generate_access_key),
            secret_key: input.secret_key.unwrap_or_else(generate_secret_key),
            admin_token: input.admin_token.unwrap_or_else(generate_admin_token),
            rpc_secret: input.rpc_secret.unwrap_or_else(generate_rpc_secret),
            host: input.host,
            region: input.region,
            docker_image: input.docker_image,
        }
    }

    async fn pick_port(docker: &Docker, requested: Option<&str>, base: u16) -> String {
        if let Some(requested) = requested {
            if let Ok(port) = requested.parse::<u16>() {
                if is_port_available_async(docker, port).await {
                    return requested.to_string();
                }
                warn!("Requested Garage port {port} is not available, finding another");
            }
        }
        find_available_port_async(docker, base)
            .await
            .map(|p| p.to_string())
            .unwrap_or_else(|| base.to_string())
    }

    /// Parse the configuration of a service that already exists. Unlike
    /// creation this never generates anything: a generated secret could not
    /// match the running container.
    pub(crate) fn from_stored(
        service: &str,
        parameters: &serde_json::Value,
    ) -> Result<Self, ManagedS3Error> {
        let input: GarageInputConfig = serde_json::from_value(parameters.clone()).map_err(|e| {
            ManagedS3Error::InvalidConfig {
                backend: BACKEND_NAME,
                service: service.to_string(),
                reason: format!("stored configuration did not parse: {e}"),
            }
        })?;
        let missing = |field: &str| ManagedS3Error::InvalidConfig {
            backend: BACKEND_NAME,
            service: service.to_string(),
            reason: format!("stored configuration is missing required field '{field}'"),
        };
        Ok(Self {
            port: input.port.ok_or_else(|| missing("port"))?,
            admin_port: input.admin_port.ok_or_else(|| missing("admin_port"))?,
            access_key: input.access_key.ok_or_else(|| missing("access_key"))?,
            secret_key: input.secret_key.ok_or_else(|| missing("secret_key"))?,
            admin_token: input.admin_token.ok_or_else(|| missing("admin_token"))?,
            rpc_secret: input.rpc_secret.ok_or_else(|| missing("rpc_secret"))?,
            host: input.host,
            region: input.region,
            docker_image: input.docker_image,
        })
    }

    fn s3_port(&self, service: &str) -> Result<u16, ManagedS3Error> {
        parse_port(service, "port", &self.port)
    }

    fn admin_port(&self, service: &str) -> Result<u16, ManagedS3Error> {
        parse_port(service, "admin_port", &self.admin_port)
    }
}

fn parse_port(service: &str, field: &str, value: &str) -> Result<u16, ManagedS3Error> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| ManagedS3Error::InvalidConfig {
            backend: BACKEND_NAME,
            service: service.to_string(),
            reason: format!("'{field}' value '{value}' is not a valid TCP port"),
        })
}

// ---------------------------------------------------------------------------
// Admin API client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClusterStatus {
    layout_version: u64,
    nodes: Vec<ClusterNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClusterNode {
    id: String,
    is_up: bool,
}

#[derive(Debug, Deserialize)]
struct BucketInfo {
    id: String,
}

#[derive(Debug, Deserialize)]
struct KeyInfo {
    #[serde(default)]
    buckets: Vec<KeyBucket>,
}

#[derive(Debug, Deserialize)]
struct KeyBucket {
    id: String,
}

/// Thin typed client for the handful of Garage admin API v2 calls Temps uses.
struct GarageAdmin {
    http: reqwest::Client,
    base_url: String,
    token: String,
    service: String,
}

impl GarageAdmin {
    fn new(service: &str, config: &GarageConfig) -> Result<Self, ManagedS3Error> {
        let port = config.admin_port(service)?;
        let http = reqwest::Client::builder()
            .timeout(GARAGE_ADMIN_TIMEOUT)
            .build()
            .map_err(|e| ManagedS3Error::Admin {
                backend: BACKEND_NAME,
                service: service.to_string(),
                operation: "building the admin API client".to_string(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            http,
            base_url: format!("http://{}:{}", config.host, port),
            token: config.admin_token.clone(),
            service: service.to_string(),
        })
    }

    fn error(&self, operation: &str, reason: impl Into<String>) -> ManagedS3Error {
        ManagedS3Error::Admin {
            backend: BACKEND_NAME,
            service: self.service.clone(),
            operation: operation.to_string(),
            reason: reason.into(),
        }
    }

    async fn send(
        &self,
        operation: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ManagedS3Error> {
        request.bearer_auth(&self.token).send().await.map_err(|e| {
            self.error(
                operation,
                format!("no response from {}: {e}", self.base_url),
            )
        })
    }

    /// Fail with the status and a bounded excerpt of the body unless the
    /// response is a success.
    async fn expect_success(
        &self,
        operation: &str,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ManagedS3Error> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(ADMIN_BODY_EXCERPT_CHARS).collect();
        Err(self.error(operation, format!("HTTP {status}: {excerpt}")))
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        response: reqwest::Response,
    ) -> Result<T, ManagedS3Error> {
        response
            .json::<T>()
            .await
            .map_err(|e| self.error(operation, format!("unexpected response body: {e}")))
    }

    /// `GET /health`: 200 once the cluster has quorum for every partition,
    /// i.e. once the layout has been applied and the node can take writes.
    async fn health_status(&self) -> Result<u16, ManagedS3Error> {
        let response = self
            .send(
                "health check",
                self.http.get(format!("{}/health", self.base_url)),
            )
            .await?;
        Ok(response.status().as_u16())
    }

    async fn cluster_status(&self) -> Result<ClusterStatus, ManagedS3Error> {
        let operation = "reading the cluster status";
        let response = self
            .send(
                operation,
                self.http
                    .get(format!("{}/v2/GetClusterStatus", self.base_url)),
            )
            .await?;
        let response = self.expect_success(operation, response).await?;
        self.decode(operation, response).await
    }

    /// Give the node a role and apply the layout, unless a layout exists.
    async fn ensure_layout(&self) -> Result<(), ManagedS3Error> {
        let status = self.cluster_status().await?;
        if status.layout_version > 0 {
            return Ok(());
        }
        let node = status.nodes.iter().find(|node| node.is_up).ok_or_else(|| {
            self.error(
                "assigning the cluster layout",
                "the node reports no live members",
            )
        })?;

        let operation = "staging the cluster layout";
        let body = serde_json::json!({
            "roles": [{
                "id": node.id,
                "zone": GARAGE_LAYOUT_ZONE,
                "capacity": GARAGE_LAYOUT_CAPACITY_BYTES,
                "tags": [],
            }]
        });
        let response = self
            .send(
                operation,
                self.http
                    .post(format!("{}/v2/UpdateClusterLayout", self.base_url))
                    .json(&body),
            )
            .await?;
        self.expect_success(operation, response).await?;

        let operation = "applying the cluster layout";
        let response = self
            .send(
                operation,
                self.http
                    .post(format!("{}/v2/ApplyClusterLayout", self.base_url))
                    .json(&serde_json::json!({ "version": status.layout_version + 1 })),
            )
            .await?;
        self.expect_success(operation, response).await?;
        info!(service = %self.service, node = %node.id, "Applied Garage cluster layout");
        Ok(())
    }

    async fn bucket_by_alias(&self, alias: &str) -> Result<Option<BucketInfo>, ManagedS3Error> {
        let operation = format!("looking up bucket '{alias}'");
        let response = self
            .send(
                &operation,
                self.http
                    .get(format!("{}/v2/GetBucketInfo", self.base_url))
                    .query(&[("globalAlias", alias)]),
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = self.expect_success(&operation, response).await?;
        self.decode(&operation, response).await.map(Some)
    }

    /// Create the bucket if it does not exist and return it.
    async fn ensure_bucket(&self, alias: &str) -> Result<BucketInfo, ManagedS3Error> {
        if let Some(bucket) = self.bucket_by_alias(alias).await? {
            return Ok(bucket);
        }
        let operation = format!("creating bucket '{alias}'");
        let response = self
            .send(
                &operation,
                self.http
                    .post(format!("{}/v2/CreateBucket", self.base_url))
                    .json(&serde_json::json!({ "globalAlias": alias })),
            )
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            // Created concurrently between the lookup and the create.
            return self.bucket_by_alias(alias).await?.ok_or_else(|| {
                self.error(
                    &operation,
                    "Garage reported the bucket exists but cannot find it",
                )
            });
        }
        let response = self.expect_success(&operation, response).await?;
        self.decode(&operation, response).await
    }

    async fn key_info(&self, access_key_id: &str) -> Result<Option<KeyInfo>, ManagedS3Error> {
        let operation = format!("looking up access key '{access_key_id}'");
        let response = self
            .send(
                &operation,
                self.http
                    .get(format!("{}/v2/GetKeyInfo", self.base_url))
                    .query(&[("id", access_key_id)]),
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = self.expect_success(&operation, response).await?;
        self.decode(&operation, response).await.map(Some)
    }

    /// Import the key with the id and secret Temps chose. A key that already
    /// exists is left alone: the secret is derived, so it is the same one.
    async fn ensure_key(
        &self,
        access_key_id: &str,
        secret_access_key: &str,
        name: &str,
    ) -> Result<KeyInfo, ManagedS3Error> {
        if let Some(info) = self.key_info(access_key_id).await? {
            return Ok(info);
        }
        let operation = format!("importing access key '{access_key_id}'");
        let response = self
            .send(
                &operation,
                self.http
                    .post(format!("{}/v2/ImportKey", self.base_url))
                    .json(&serde_json::json!({
                        "accessKeyId": access_key_id,
                        "secretAccessKey": secret_access_key,
                        "name": name,
                    })),
            )
            .await?;
        if response.status() != reqwest::StatusCode::CONFLICT {
            self.expect_success(&operation, response).await?;
        }
        Ok(KeyInfo {
            buckets: Vec::new(),
        })
    }

    async fn set_permissions(
        &self,
        verb: &str,
        bucket_id: &str,
        access_key_id: &str,
        read: bool,
        write: bool,
        owner: bool,
    ) -> Result<(), ManagedS3Error> {
        let operation = format!("{verb} on bucket {bucket_id} for key '{access_key_id}'");
        let response = self
            .send(
                &operation,
                self.http
                    .post(format!("{}/v2/{verb}", self.base_url))
                    .json(&serde_json::json!({
                        "bucketId": bucket_id,
                        "accessKeyId": access_key_id,
                        "permissions": { "read": read, "write": write, "owner": owner },
                    })),
            )
            .await?;
        self.expect_success(&operation, response).await?;
        Ok(())
    }

    async fn allow(
        &self,
        bucket_id: &str,
        access_key_id: &str,
        owner: bool,
    ) -> Result<(), ManagedS3Error> {
        self.set_permissions(
            "AllowBucketKey",
            bucket_id,
            access_key_id,
            true,
            true,
            owner,
        )
        .await
    }

    async fn deny_all(&self, bucket_id: &str, access_key_id: &str) -> Result<(), ManagedS3Error> {
        self.set_permissions("DenyBucketKey", bucket_id, access_key_id, true, true, true)
            .await
    }
}

/// Container evidence for the readiness gate.
type ContainerEvidence = Option<(Arc<Docker>, String)>;

/// Readiness gate: the admin API reports quorum (layout applied) and an
/// authenticated `ListBuckets` with the service key succeeds.
pub(crate) struct GarageReadinessTarget {
    service: String,
    config: GarageConfig,
    container: ContainerEvidence,
}

impl GarageReadinessTarget {
    pub(crate) fn new(service: String, config: GarageConfig, container: ContainerEvidence) -> Self {
        Self {
            service,
            config,
            container,
        }
    }

    async fn probe(&self) -> Option<String> {
        match tokio::time::timeout(
            GARAGE_PROBE_TIMEOUT,
            authenticated_probe(&self.service, &self.config),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(reason)) => Some(reason),
            Err(_) => Some(format!(
                "garage probe timed out after {}s",
                GARAGE_PROBE_TIMEOUT.as_secs()
            )),
        }
    }
}

#[async_trait]
impl ReadinessTarget for GarageReadinessTarget {
    fn policy(&self) -> ReadinessPolicy {
        ReadinessPolicy {
            deadline: GARAGE_READINESS_DEADLINE,
            poll_interval: GARAGE_READINESS_POLL_INTERVAL,
            max_restarts: GARAGE_READINESS_MAX_RESTARTS,
        }
    }

    async fn observe(&self) -> ReadinessObservation {
        let Some(reason) = self.probe().await else {
            return ReadinessObservation::default();
        };
        let restart_count = match &self.container {
            Some((docker, name)) => super::rustfs::container_restart_count(docker, name).await,
            None => None,
        };
        ReadinessObservation {
            not_ready_reason: Some(reason),
            restart_count,
            fatal_log_lines: Vec::new(),
        }
    }

    async fn log_excerpt(&self) -> Vec<String> {
        // Garage logs through the Docker log driver; the container's own
        // restart evidence is what the gate reports, and the configuration
        // contains secrets worth not echoing, so there is no excerpt.
        Vec::new()
    }
}

/// The two-step end-to-end probe: admin `/health` (quorum, layout applied),
/// then an authenticated S3 `ListBuckets` with the service key. Errors are
/// worded for an operator and never contain a secret.
async fn authenticated_probe(service: &str, config: &GarageConfig) -> Result<(), String> {
    let admin = GarageAdmin::new(service, config).map_err(|e| e.to_string())?;
    match admin.health_status().await {
        Ok(200) => {}
        Ok(503) => {
            return Err(format!(
                "garage admin API at {} reports no quorum: the cluster layout is not applied yet",
                admin.base_url
            ))
        }
        Ok(status) => {
            return Err(format!(
                "garage admin API at {} answered HTTP {status} to /health",
                admin.base_url
            ))
        }
        Err(e) => return Err(e.to_string()),
    }

    let endpoint = format!("http://{}:{}", config.host, config.port);
    let credentials = aws_sdk_s3::config::Credentials::new(
        config.access_key.clone(),
        config.secret_key.clone(),
        None,
        None,
        "garage-health-probe",
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .region(Region::new(config.region.clone()))
        .endpoint_url(endpoint.clone())
        .credentials_provider(credentials)
        .force_path_style(true)
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
        .list_buckets()
        .send()
        .await
        .map(|_| ())
        .map_err(|e| {
            let status = e.raw_response().map(|r| r.status().as_u16());
            match status {
                Some(401 | 403) => format!(
                    "garage at {endpoint} rejected the service access key (HTTP {})",
                    status.unwrap_or_default()
                ),
                Some(status) => format!("ListBuckets against {endpoint} failed with HTTP {status}"),
                None => format!("garage at {endpoint} is not reachable over S3"),
            }
        })
}

/// Host ports published by a running container on exactly `docker_image`,
/// as `(s3, admin)`. Anything else means the container is about to be
/// replaced and freshly probed ports are the right ones.
fn adopted_ports_from_inspect(
    info: &bollard::models::ContainerInspectResponse,
    docker_image: &str,
) -> Option<(String, String)> {
    let running = info
        .state
        .as_ref()
        .and_then(|state| state.status)
        .is_some_and(|status| status == bollard::models::ContainerStateStatusEnum::RUNNING);
    let image_matches = info
        .config
        .as_ref()
        .and_then(|config| config.image.as_deref())
        .is_some_and(|image| image == docker_image);
    if !running || !image_matches {
        return None;
    }
    let ports = info.network_settings.as_ref()?.ports.as_ref()?;
    let host_port = |internal: &str| -> Option<String> {
        ports
            .get(internal)?
            .as_ref()?
            .first()?
            .host_port
            .clone()
            .filter(|port| !port.is_empty())
    };
    Some((
        host_port(&format!("{GARAGE_S3_PORT}/tcp"))?,
        host_port(&format!("{GARAGE_ADMIN_PORT}/tcp"))?,
    ))
}

fn docker_error_is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn docker_error_is_not_modified(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 304,
            ..
        }
    )
}

/// One Garage-backed managed S3 service.
pub struct GarageService {
    name: String,
    service_type: ServiceType,
    config: Arc<RwLock<Option<GarageConfig>>>,
    resource_limits: Arc<RwLock<ServiceResourceLimits>>,
    docker: Arc<Docker>,
    /// Decrypts the backup destination's stored credentials.
    encryption_service: Arc<EncryptionService>,
}

impl GarageService {
    pub fn new(
        name: String,
        docker: Arc<Docker>,
        encryption_service: Arc<EncryptionService>,
        service_type: ServiceType,
    ) -> Self {
        Self {
            name,
            service_type,
            config: Arc::new(RwLock::new(None)),
            resource_limits: Arc::new(RwLock::new(ServiceResourceLimits::default())),
            docker,
            encryption_service,
        }
    }

    pub fn get_container_name(&self) -> String {
        garage_container_name(&self.name)
    }

    pub fn parameter_schema() -> Option<serde_json::Value> {
        serde_json::to_value(schemars::schema_for!(GarageInputConfig)).ok()
    }

    fn docker_error(&self, operation: &str, error: impl std::fmt::Display) -> ManagedS3Error {
        ManagedS3Error::Docker {
            backend: BACKEND_NAME,
            service: self.name.clone(),
            operation: operation.to_string(),
            reason: error.to_string(),
        }
    }

    fn stored_config(&self, config: &ServiceConfig) -> Result<GarageConfig, ManagedS3Error> {
        GarageConfig::from_stored(&self.name, &config.parameters)
    }

    async fn running_container_ports(&self, docker_image: &str) -> Option<(String, String)> {
        let info = self
            .docker
            .inspect_container(&self.get_container_name(), None::<InspectContainerOptions>)
            .await
            .ok()?;
        adopted_ports_from_inspect(&info, docker_image)
    }

    /// Create the container (volume, config file, ports) and start it. A
    /// container already running on this image is left alone. With
    /// `require_new` an existing container or volume is an error and anything
    /// created here is removed again on failure.
    async fn ensure_container(
        &self,
        config: &GarageConfig,
        limits: &ServiceResourceLimits,
        require_new: bool,
    ) -> Result<(), ManagedS3Error> {
        let docker = self.docker.as_ref();
        let container_name = self.get_container_name();
        let [data_volume] = garage_volume_names(&self.name);
        let toml = garage_toml(&self.name, config)?;
        let s3_port = config.s3_port(&self.name)?;
        let admin_port = config.admin_port(&self.name)?;

        info!(service = %self.name, image = %config.docker_image, "Pulling Garage image");
        crate::utils::pull_image_with_retry(docker, &config.docker_image, None)
            .await
            .map_err(|e| self.docker_error("pulling the image", format!("{e:?}")))?;

        let existing = docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "name".to_string(),
                    vec![super::exact_container_name_filter(&container_name)],
                )])),
                ..Default::default()
            }))
            .await
            .map_err(|e| self.docker_error("listing containers", e))?;

        if let Some(container) = existing.first() {
            if require_new {
                return Err(self.docker_error(
                    "creating the restored service",
                    format!("container '{container_name}' already exists"),
                ));
            }
            let is_running =
                container.state == Some(bollard::models::ContainerSummaryStateEnum::RUNNING);
            if is_running && container.image.as_deref() == Some(config.docker_image.as_str()) {
                debug!(service = %self.name, "Garage container already running on the requested image");
                return Ok(());
            }
            info!(
                service = %self.name,
                running = is_running,
                "Recreating Garage container (volume is kept)"
            );
            let _ = docker
                .stop_container(&container_name, None::<StopContainerOptions>)
                .await;
            docker
                .remove_container(
                    &container_name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| self.docker_error("removing the outdated container", e))?;
        }

        if require_new {
            match docker.inspect_volume(&data_volume).await {
                Ok(_) => {
                    return Err(self.docker_error(
                        "creating the restored service",
                        format!("derived volume '{data_volume}' already exists"),
                    ))
                }
                Err(e) if docker_error_is_not_found(&e) => {}
                Err(e) => {
                    return Err(self.docker_error(&format!("checking volume '{data_volume}'"), e))
                }
            }
        }

        ensure_network_exists(docker)
            .await
            .map_err(|e| self.docker_error("ensuring the Temps network", format!("{e:?}")))?;
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(data_volume.clone()),
                ..Default::default()
            })
            .await
            .map_err(|e| self.docker_error(&format!("creating volume '{data_volume}'"), e))?;

        let result = self
            .create_and_start(
                config,
                limits,
                &container_name,
                &data_volume,
                &toml,
                s3_port,
                admin_port,
            )
            .await;
        if result.is_err() && require_new {
            self.remove_unowned(&container_name, &data_volume).await;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_and_start(
        &self,
        config: &GarageConfig,
        limits: &ServiceResourceLimits,
        container_name: &str,
        data_volume: &str,
        toml: &str,
        s3_port: u16,
        admin_port: u16,
    ) -> Result<(), ManagedS3Error> {
        let docker = self.docker.as_ref();
        let labels = HashMap::from([
            (
                format!("{}service_type", temps_core::DOCKER_LABEL_PREFIX),
                BACKEND_NAME.to_string(),
            ),
            (
                format!("{}service_name", temps_core::DOCKER_LABEL_PREFIX),
                self.name.clone(),
            ),
        ]);

        let mut port_bindings = crate::utils::local_port_binding(
            &format!("{GARAGE_S3_PORT}/tcp"),
            &s3_port.to_string(),
        );
        port_bindings.extend(crate::utils::local_port_binding(
            &format!("{GARAGE_ADMIN_PORT}/tcp"),
            &admin_port.to_string(),
        ));

        let mut host_config = bollard::models::HostConfig {
            port_bindings: Some(port_bindings),
            mounts: Some(vec![bollard::models::Mount {
                target: Some(GARAGE_DATA_MOUNT.to_string()),
                source: Some(data_volume.to_string()),
                typ: Some(bollard::models::MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
            log_config: Some(crate::utils::default_service_log_config()),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            pids_limit: Some(512),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };
        limits.apply_to_host_config(&mut host_config);

        let container_config = bollard::models::ContainerCreateBody {
            image: Some(config.docker_image.clone()),
            networking_config: Some(bollard::models::NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    temps_core::NETWORK_NAME.to_string(),
                    bollard::models::EndpointSettings::default(),
                )])),
            }),
            exposed_ports: Some(vec![
                format!("{GARAGE_S3_PORT}/tcp"),
                format!("{GARAGE_ADMIN_PORT}/tcp"),
            ]),
            env: Some(vec![
                format!("GARAGE_RPC_SECRET={}", config.rpc_secret),
                format!("GARAGE_ADMIN_TOKEN={}", config.admin_token),
            ]),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(container_name)
                        .build(),
                ),
                container_config,
            )
            .await
            .map_err(|e| self.docker_error(&format!("creating container '{container_name}'"), e))?;

        super::container_upload::upload_bytes_to_container(
            docker,
            &container.id,
            toml.as_bytes().to_vec(),
            GARAGE_CONFIG_DIR,
            GARAGE_CONFIG_FILE,
            0o644,
        )
        .await
        .map_err(|e| self.docker_error("uploading garage.toml", e))?;

        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| self.docker_error(&format!("starting container '{container_name}'"), e))?;
        info!(service = %self.name, container = %container_name, "Garage container started");
        Ok(())
    }

    /// Best-effort removal of a restore target that has no database owner yet.
    async fn remove_unowned(&self, container_name: &str, data_volume: &str) {
        let _ = self
            .docker
            .remove_container(
                container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let _ = self
            .docker
            .remove_volume(
                data_volume,
                None::<bollard::query_parameters::RemoveVolumeOptions>,
            )
            .await;
    }

    /// Wait for the admin API, apply the layout, wait for quorum and import
    /// the service's own key.
    async fn bring_up(&self, config: &GarageConfig) -> Result<(), ManagedS3Error> {
        let admin = GarageAdmin::new(&self.name, config)?;
        let started = Instant::now();
        let mut last_error = String::from("no attempt completed");
        let mut delay = Duration::from_millis(250);
        while started.elapsed() < GARAGE_STARTUP_DEADLINE {
            match self.try_bring_up(&admin, config).await {
                Ok(()) => return Ok(()),
                Err(e) => last_error = e.to_string(),
            }
            tokio::time::sleep(delay).await;
            delay = std::cmp::min(delay.mul_f32(1.5), Duration::from_secs(2));
        }
        Err(ManagedS3Error::NotReady {
            backend: BACKEND_NAME,
            service: self.name.clone(),
            waited_secs: GARAGE_STARTUP_DEADLINE.as_secs(),
            last_error,
        })
    }

    async fn try_bring_up(
        &self,
        admin: &GarageAdmin,
        config: &GarageConfig,
    ) -> Result<(), ManagedS3Error> {
        admin.ensure_layout().await?;
        if admin.health_status().await? != 200 {
            return Err(admin.error(
                "waiting for quorum",
                "the layout is applied but /health is not 200 yet",
            ));
        }
        admin
            .ensure_key(&config.access_key, &config.secret_key, "temps-service")
            .await?;
        Ok(())
    }

    /// Start (or adopt) the container and make it ready. Shared by `init` and
    /// the restore-to-new-service path.
    async fn provision(
        &self,
        config: &ServiceConfig,
    ) -> Result<(GarageConfig, ServiceResourceLimits), ManagedS3Error> {
        let limits = ServiceResourceLimits::from_parameters(&config.parameters);
        limits
            .validate()
            .map_err(|reason| ManagedS3Error::InvalidConfig {
                backend: BACKEND_NAME,
                service: self.name.clone(),
                reason: format!("invalid resource limits: {reason}"),
            })?;
        *self.resource_limits.write().await = limits.clone();

        let input: GarageInputConfig =
            serde_json::from_value(config.parameters.clone()).map_err(|e| {
                ManagedS3Error::InvalidConfig {
                    backend: BACKEND_NAME,
                    service: self.name.clone(),
                    reason: format!("configuration did not parse: {e}"),
                }
            })?;
        let mut runtime = GarageConfig::from_input_async(input, &self.docker).await;

        // A container already running on this image owns its ports, so the
        // freshly probed ones are fiction (see the RustFS equivalent).
        if let Some((s3, admin)) = self.running_container_ports(&runtime.docker_image).await {
            runtime.port = s3;
            runtime.admin_port = admin;
        }

        self.ensure_container(&runtime, &limits, false).await?;
        self.bring_up(&runtime).await?;
        *self.config.write().await = Some(runtime.clone());
        Ok((runtime, limits))
    }

    fn inferred_parameters(config: &GarageConfig) -> HashMap<String, String> {
        HashMap::from([
            ("backend".to_string(), BACKEND_NAME.to_string()),
            ("port".to_string(), config.port.clone()),
            ("admin_port".to_string(), config.admin_port.clone()),
            ("access_key".to_string(), config.access_key.clone()),
            ("secret_key".to_string(), config.secret_key.clone()),
            ("admin_token".to_string(), config.admin_token.clone()),
            ("rpc_secret".to_string(), config.rpc_secret.clone()),
            ("host".to_string(), config.host.clone()),
            ("region".to_string(), config.region.clone()),
            ("docker_image".to_string(), config.docker_image.clone()),
        ])
    }

    fn s3_location_from_source(
        s3_source: &temps_entities::s3_sources::Model,
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
        prefix: &str,
    ) -> S3Location {
        S3Location {
            endpoint: s3_source.endpoint.clone(),
            access_key,
            secret_key,
            session_token,
            bucket: s3_source.bucket_name.clone(),
            prefix: prefix.trim_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl ManagedS3Backend for GarageService {
    fn kind(&self) -> ManagedS3BackendKind {
        ManagedS3BackendKind::Garage
    }

    fn service_name(&self) -> String {
        self.name.clone()
    }

    fn docker(&self) -> Arc<Docker> {
        self.docker.clone()
    }

    fn endpoint(&self, config: &ServiceConfig) -> Result<ManagedS3Endpoint, ManagedS3Error> {
        let stored = self.stored_config(config)?;
        Ok(ManagedS3Endpoint {
            internal_host: self.get_container_name(),
            internal_port: GARAGE_S3_PORT,
            host: stored.host.clone(),
            host_port: stored.s3_port(&self.name)?,
            region: stored.region,
        })
    }

    fn admin_credentials(
        &self,
        config: &ServiceConfig,
    ) -> Result<(String, String), ManagedS3Error> {
        let stored = self.stored_config(config)?;
        Ok((stored.access_key, stored.secret_key))
    }

    async fn provision_service(
        &self,
        config: ServiceConfig,
    ) -> Result<HashMap<String, String>, ManagedS3Error> {
        let (runtime, _) = self.provision(&config).await?;
        Ok(Self::inferred_parameters(&runtime))
    }

    async fn create_bucket(
        &self,
        config: &ServiceConfig,
        bucket: &str,
    ) -> Result<(), ManagedS3Error> {
        let stored = self.stored_config(config)?;
        let admin = GarageAdmin::new(&self.name, &stored)?;
        admin
            .ensure_key(&stored.access_key, &stored.secret_key, "temps-service")
            .await?;
        let info = admin.ensure_bucket(bucket).await?;
        admin.allow(&info.id, &stored.access_key, true).await
    }

    async fn create_access_key(
        &self,
        config: &ServiceConfig,
        request: &ManagedS3AccessKeyRequest,
    ) -> Result<ManagedS3Grant, ManagedS3Error> {
        let stored = self.stored_config(config)?;
        let admin = GarageAdmin::new(&self.name, &stored)?;
        let grant = derive_grant(
            ManagedS3BackendKind::Garage,
            &stored.secret_key,
            &request.scope(),
            &request.buckets,
            GARAGE_CREDENTIAL_FORMAT,
        );

        // Buckets first (the service key keeps access for mirroring), then
        // the environment key, then the grants between them.
        let mut granted = Vec::with_capacity(grant.buckets.len());
        for plan in &grant.buckets {
            self.create_bucket(config, &plan.physical).await?;
            let info = admin.ensure_bucket(&plan.physical).await?;
            granted.push(info.id);
        }
        let key = admin
            .ensure_key(
                &grant.access_key_id,
                &grant.secret_access_key,
                &format!("temps-env-{}", request.scope()),
            )
            .await?;
        for bucket_id in &granted {
            admin.allow(bucket_id, &grant.access_key_id, false).await?;
        }
        // A bucket the environment no longer declares must stop being reachable.
        for stale in key.buckets.iter().filter(|b| !granted.contains(&b.id)) {
            admin.deny_all(&stale.id, &grant.access_key_id).await?;
        }
        info!(
            service = %self.name,
            scope = %request.scope(),
            buckets = grant.buckets.len(),
            "Provisioned Garage environment key"
        );
        Ok(grant)
    }

    async fn revoke_access_key(
        &self,
        config: &ServiceConfig,
        project_scope: &str,
        environment_scope: &str,
    ) -> Result<(), ManagedS3Error> {
        let stored = self.stored_config(config)?;
        let admin = GarageAdmin::new(&self.name, &stored)?;
        let scope = super::managed_s3::scope_slug(project_scope, environment_scope);
        let (access_key_id, _) = super::managed_s3::derive_env_credentials(
            ManagedS3BackendKind::Garage,
            &stored.secret_key,
            &scope,
            GARAGE_CREDENTIAL_FORMAT,
        );
        // Garage never accepts a deleted key id again, so the key stays and
        // loses every bucket instead.
        let Some(key) = admin.key_info(&access_key_id).await? else {
            return Ok(());
        };
        for bucket in &key.buckets {
            admin.deny_all(&bucket.id, &access_key_id).await?;
        }
        info!(service = %self.name, scope = %scope, "Revoked Garage environment key");
        Ok(())
    }

    async fn health_check(
        &self,
        config: &ServiceConfig,
    ) -> Result<HealthProbeResult, ManagedS3Error> {
        const DEGRADED_MS: u128 = 2000;
        let stored = self.stored_config(config)?;
        let started = Instant::now();
        let probe = tokio::time::timeout(
            GARAGE_PROBE_TIMEOUT,
            authenticated_probe(&self.name, &stored),
        )
        .await;
        let elapsed_ms = started.elapsed().as_millis();
        let response_time = i32::try_from(elapsed_ms).ok();
        Ok(match probe {
            Err(_) => HealthProbeResult::down(format!(
                "garage probe timed out after {}s",
                GARAGE_PROBE_TIMEOUT.as_secs()
            )),
            Ok(Err(reason)) => HealthProbeResult::down(reason),
            Ok(Ok(())) if elapsed_ms > DEGRADED_MS => HealthProbeResult::degraded(
                format!("garage responded in {elapsed_ms}ms (>{DEGRADED_MS}ms)"),
                response_time,
            ),
            Ok(Ok(())) => HealthProbeResult::operational(response_time),
        })
    }
}

#[async_trait]
impl ExternalService for GarageService {
    fn docker_resource_names(&self) -> Option<super::DockerResourceNames> {
        Some(super::DockerResourceNames {
            containers: vec![self.get_container_name()],
            volumes: garage_volume_names(&self.name).to_vec(),
        })
    }

    fn readiness_target(&self, service_config: &ServiceConfig) -> Option<Box<dyn ReadinessTarget>> {
        match self.stored_config(service_config) {
            Ok(config) => Some(Box::new(GarageReadinessTarget::new(
                self.name.clone(),
                config,
                Some((self.docker.clone(), self.get_container_name())),
            ))),
            Err(e) => {
                warn!(service = %self.name, "Garage service has no readiness gate: {e}");
                None
            }
        }
    }

    async fn init(&self, config: ServiceConfig) -> Result<HashMap<String, String>> {
        info!(service = %self.name, "Initializing Garage service");
        Ok(self.provision_service(config).await?)
    }

    async fn health_check(&self) -> Result<bool> {
        let stored = self.config.read().await.clone();
        match stored {
            Some(config) => Ok(authenticated_probe(&self.name, &config).await.is_ok()),
            None => Ok(false),
        }
    }

    async fn health_probe(&self, service_config: ServiceConfig) -> Result<HealthProbeResult> {
        match ManagedS3Backend::health_check(self, &service_config).await {
            Ok(result) => Ok(result),
            Err(e) => Ok(HealthProbeResult::down(format!(
                "invalid garage config: {e}"
            ))),
        }
    }

    fn get_type(&self) -> ServiceType {
        self.service_type
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_connection_info(&self) -> Result<String> {
        let config = self
            .config
            .try_read()
            .map_err(|_| anyhow::anyhow!("Garage config for service '{}' is locked", self.name))?;
        match config.as_ref() {
            Some(cfg) => Ok(format!("http://{}:{}", cfg.host, cfg.port)),
            None => Err(anyhow::anyhow!(
                "Garage service '{}' is not initialized",
                self.name
            )),
        }
    }

    async fn cleanup(&self) -> Result<()> {
        let _ = self.stop().await;
        self.remove().await
    }

    fn get_parameter_schema(&self) -> Option<serde_json::Value> {
        Self::parameter_schema()
    }

    async fn start(&self) -> Result<()> {
        match self
            .docker
            .start_container(
                &self.get_container_name(),
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
            Ok(()) => Ok(()),
            // Already running.
            Err(e) if docker_error_is_not_modified(&e) => Ok(()),
            Err(e) => Err(self.docker_error("starting the container", e).into()),
        }
    }

    async fn stop(&self) -> Result<()> {
        match self
            .docker
            .stop_container(&self.get_container_name(), None::<StopContainerOptions>)
            .await
        {
            Ok(()) => Ok(()),
            // Already stopped.
            Err(e) if docker_error_is_not_modified(&e) => Ok(()),
            Err(e) => Err(self.docker_error("stopping the container", e).into()),
        }
    }

    async fn remove(&self) -> Result<()> {
        let container_name = self.get_container_name();
        let _ = self
            .docker
            .stop_container(&container_name, None::<StopContainerOptions>)
            .await;
        match self
            .docker
            .remove_container(
                &container_name,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => {}
            Err(e) if docker_error_is_not_found(&e) => {}
            Err(e) => return Err(self.docker_error("removing the container", e).into()),
        }
        for volume in garage_volume_names(&self.name) {
            if let Err(e) = self
                .docker
                .remove_volume(
                    &volume,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                if !docker_error_is_not_found(&e) {
                    warn!(service = %self.name, "Could not remove Garage volume '{volume}': {e}");
                }
            }
        }
        Ok(())
    }

    fn get_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let host = parameters
            .get("host")
            .cloned()
            .unwrap_or_else(|| "localhost".to_string());
        let port = parameters
            .get("port")
            .cloned()
            .unwrap_or_else(|| GARAGE_S3_PORT.to_string());
        Ok(service_env(&format!("http://{host}:{port}"), parameters))
    }

    fn get_docker_environment_variables(
        &self,
        parameters: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        Ok(service_env(
            &format!("http://{}:{}", self.get_container_name(), GARAGE_S3_PORT),
            parameters,
        ))
    }

    fn get_runtime_env_definitions(&self) -> Vec<super::RuntimeEnvVar> {
        vec![
            super::RuntimeEnvVar {
                name: "S3_BUCKET".to_string(),
                description: "Primary S3 bucket for this project/environment".to_string(),
                example: "project-123-production".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "S3_BUCKET_<NAME>".to_string(),
                description: "One variable per declared bucket, e.g. S3_BUCKET_UPLOADS".to_string(),
                example: "project-123-production-uploads".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "S3_ENDPOINT".to_string(),
                description: "S3-compatible endpoint URL (internal container name)".to_string(),
                example: "http://garage-my-service:3900".to_string(),
                sensitive: false,
            },
            super::RuntimeEnvVar {
                name: "S3_ACCESS_KEY".to_string(),
                description: "Access key scoped to this environment's buckets".to_string(),
                example: "GK0123456789abcdef01234567".to_string(),
                sensitive: true,
            },
            super::RuntimeEnvVar {
                name: "S3_SECRET_KEY".to_string(),
                description: "Secret key for the environment access key".to_string(),
                example: "********".to_string(),
                sensitive: true,
            },
        ]
    }

    async fn get_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let selection = ManagedS3BackendSelection::from_parameters(&config.parameters)?;
        let request = ManagedS3AccessKeyRequest {
            project_scope: project_id.to_string(),
            environment_scope: environment.to_string(),
            buckets: selection.buckets,
        };
        let grant = self.create_access_key(&config, &request).await?;
        Ok(self.render_runtime_env(&config, &grant)?)
    }

    async fn preview_runtime_env_vars(
        &self,
        config: ServiceConfig,
        project_id: &str,
        environment: &str,
    ) -> Result<HashMap<String, String>> {
        let selection = ManagedS3BackendSelection::from_parameters(&config.parameters)?;
        let stored = self.stored_config(&config)?;
        let request = ManagedS3AccessKeyRequest {
            project_scope: project_id.to_string(),
            environment_scope: environment.to_string(),
            buckets: selection.buckets,
        };
        // No admin API call: the grant is a pure function of the stored secret.
        let grant = derive_grant(
            ManagedS3BackendKind::Garage,
            &stored.secret_key,
            &request.scope(),
            &request.buckets,
            GARAGE_CREDENTIAL_FORMAT,
        );
        Ok(self.render_runtime_env(&config, &grant)?)
    }

    async fn deprovision_resource(&self, _project_id: &str, _environment: &str) -> Result<()> {
        // Revocation needs the service configuration; callers use
        // `ManagedS3Backend::revoke_access_key`.
        Ok(())
    }

    fn get_local_address(&self, service_config: ServiceConfig) -> Result<String> {
        let stored = self.stored_config(&service_config)?;
        Ok(format!("localhost:{}", stored.port))
    }

    fn get_effective_address(&self, service_config: ServiceConfig) -> Result<(String, String)> {
        let stored = self.stored_config(&service_config)?;
        Ok((self.get_container_name(), stored.port))
    }

    fn get_docker_container_name(&self) -> String {
        self.get_container_name()
    }

    fn get_docker_internal_port(&self) -> String {
        GARAGE_S3_PORT.to_string()
    }

    fn get_default_docker_image(&self) -> (String, String) {
        (
            "dxflrs/garage".to_string(),
            DEFAULT_GARAGE_VERSION.to_string(),
        )
    }

    async fn get_current_docker_image(&self) -> Result<(String, String)> {
        let info = self
            .docker
            .inspect_container(&self.get_container_name(), None::<InspectContainerOptions>)
            .await
            .map_err(|e| self.docker_error("inspecting the container", e))?;
        let image = info.config.and_then(|config| config.image).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine the Docker image of Garage container '{}'",
                self.get_container_name()
            )
        })?;
        Ok(match image.rsplit_once(':') {
            Some((name, tag)) if !tag.contains('/') => (name.to_string(), tag.to_string()),
            _ => (image, "latest".to_string()),
        })
    }

    fn get_default_version(&self) -> String {
        DEFAULT_GARAGE_VERSION.to_string()
    }

    async fn get_current_version(&self) -> Result<String> {
        Ok(self.get_current_docker_image().await?.1)
    }

    async fn backup_to_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        s3_credentials: &super::S3Credentials,
        backup: temps_entities::backups::Model,
        s3_source: &temps_entities::s3_sources::Model,
        _subpath: &str,
        subpath_root: &str,
        pool: &temps_database::DbConnection,
        external_service: &temps_entities::external_services::Model,
        service_config: ServiceConfig,
    ) -> Result<super::BackupOutcome> {
        use chrono::Utc;
        use sea_orm::*;

        let record = temps_entities::external_service_backups::Entity::insert(
            temps_entities::external_service_backups::ActiveModel {
                service_id: Set(external_service.id),
                backup_id: Set(backup.id),
                backup_type: Set("full".to_string()),
                state: Set("running".to_string()),
                started_at: Set(Utc::now()),
                s3_location: Set(subpath_root.to_string()),
                metadata: Set(serde_json::json!({
                    "service_type": BACKEND_NAME,
                    "service_name": self.name,
                    "timestamp": Utc::now().to_rfc3339(),
                })),
                compression_type: Set("none".to_string()),
                created_by: Set(0),
                ..Default::default()
            },
        )
        .exec_with_returning(pool)
        .await?;

        let decrypt = |what: &str, value: &str| {
            self.encryption_service.decrypt_string(value).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to decrypt the {what} of backup destination '{}' for Garage service '{}': {e}",
                    s3_source.name,
                    self.name
                )
            })
        };
        let access_key = decrypt("access key", &s3_source.access_key_id)?;
        let secret_key = decrypt("secret key", &s3_source.secret_key)?;
        let session_token = temps_entities::s3_sources::decrypt_session_token(
            self.encryption_service.as_ref(),
            s3_source,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to decrypt the session token of backup destination '{}': {e}",
                s3_source.name
            )
        })?;
        let destination = Self::s3_location_from_source(
            s3_source,
            access_key,
            secret_key,
            session_token,
            subpath_root,
        );

        let outcome = ManagedS3Backend::backup(self, &service_config, &destination).await;
        let mut update: temps_entities::external_service_backups::ActiveModel =
            record.clone().into();
        update.finished_at = Set(Some(Utc::now()));
        match outcome {
            Ok(report) => {
                let size_bytes = match super::s3_util::list_total_size(
                    &s3_credentials.build_s3_client().await,
                    &s3_credentials.bucket_name,
                    &format!("{}/", subpath_root.trim_matches('/')),
                )
                .await
                {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!(service = %self.name, "Garage backup finished but its size could not be read: {e}");
                        None
                    }
                };
                update.state = Set("completed".to_string());
                update.size_bytes = Set(size_bytes);
                temps_entities::external_service_backups::Entity::update(update)
                    .exec(pool)
                    .await?;
                info!(service = %self.name, buckets = ?report.buckets, "Garage backup completed");
                Ok(super::BackupOutcome::new(
                    subpath_root.to_string(),
                    size_bytes,
                ))
            }
            Err(e) => {
                update.state = Set("failed".to_string());
                update.error_message = Set(Some(e.to_string()));
                temps_entities::external_service_backups::Entity::update(update)
                    .exec(pool)
                    .await?;
                Err(e.into())
            }
        }
    }

    async fn restore_from_s3(
        &self,
        _s3_client: &aws_sdk_s3::Client,
        _s3_credentials: &super::S3Credentials,
        backup_location: &str,
        s3_source: &temps_entities::s3_sources::Model,
        service_config: ServiceConfig,
    ) -> Result<()> {
        info!(service = %self.name, backup_location, "Restoring Garage service from backup");
        self.start().await?;
        // `s3_source` is the orchestrator's decrypted copy.
        let source = Self::s3_location_from_source(
            s3_source,
            s3_source.access_key_id.clone(),
            s3_source.secret_key.clone(),
            s3_source.session_token.clone(),
            backup_location,
        );
        // In place: the buckets end up exactly as backed up.
        ManagedS3Backend::restore(self, &service_config, &source, true).await?;
        Ok(())
    }

    async fn restore_capabilities(
        &self,
        _service_config: ServiceConfig,
    ) -> Result<super::RestoreCapabilities> {
        Ok(super::RestoreCapabilities {
            restore_in_place: true,
            restore_to_new_service: true,
            pitr: false,
            earliest_pitr_time: None,
            latest_pitr_time: None,
        })
    }

    async fn restore_to_new_service(
        &self,
        ctx: super::RestoreContext<'_>,
        new_service_name: String,
        parameter_overrides: serde_json::Value,
    ) -> Result<super::NewServiceRestoreResult> {
        info!(
            source = %self.name,
            target = %new_service_name,
            "Provisioning a new Garage service from backup"
        );
        if temps_entities::external_services::Entity::find()
            .filter(temps_entities::external_services::Column::Name.eq(&new_service_name))
            .one(ctx.pool)
            .await?
            .is_some()
        {
            return Err(anyhow::anyhow!(
                "Cannot restore to new Garage service '{new_service_name}': a service with that name already exists"
            ));
        }

        // Same image and region as the source, but its own ports and secrets.
        let mut new_config = self.stored_config(&ctx.source_config)?;
        let s3_port = find_available_port(GARAGE_S3_PORT).ok_or_else(|| {
            anyhow::anyhow!("No available S3 port for new Garage service '{new_service_name}'")
        })?;
        let admin_port = find_available_port(std::cmp::max(s3_port + 1, GARAGE_ADMIN_PORT))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No available admin port for new Garage service '{new_service_name}'"
                )
            })?;
        new_config.port = s3_port.to_string();
        new_config.admin_port = admin_port.to_string();
        new_config.access_key = generate_access_key();
        new_config.secret_key = generate_secret_key();
        new_config.admin_token = generate_admin_token();
        new_config.rpc_secret = generate_rpc_secret();
        if let Some(overrides) = parameter_overrides.as_object() {
            let text = |key: &str| overrides.get(key).and_then(|v| v.as_str());
            if let Some(port) = text("port") {
                new_config.port = port.to_string();
            }
            if let Some(port) = text("admin_port") {
                new_config.admin_port = port.to_string();
            }
            if let Some(image) = text("docker_image") {
                new_config.docker_image = image.to_string();
            }
        }

        let target = GarageService::new(
            new_service_name.clone(),
            self.docker.clone(),
            self.encryption_service.clone(),
            self.service_type,
        );
        let limits = ServiceResourceLimits::from_parameters(&ctx.source_config.parameters);
        let [data_volume] = garage_volume_names(&new_service_name);

        target.ensure_container(&new_config, &limits, true).await?;
        let mut target_parameters = serde_json::to_value(&new_config)?;
        if let (Some(map), Some(source_map)) = (
            target_parameters.as_object_mut(),
            ctx.source_config.parameters.as_object(),
        ) {
            map.insert("backend".to_string(), BACKEND_NAME.into());
            if let Some(buckets) = source_map.get("buckets") {
                map.insert("buckets".to_string(), buckets.clone());
            }
        }
        let target_config = ServiceConfig {
            name: new_service_name.clone(),
            service_type: self.service_type,
            version: None,
            parameters: target_parameters.clone(),
        };

        let restored = async {
            target.bring_up(&new_config).await?;
            let source = Self::s3_location_from_source(
                ctx.s3_source,
                ctx.s3_source.access_key_id.clone(),
                ctx.s3_source.secret_key.clone(),
                ctx.s3_source.session_token.clone(),
                ctx.backup_location,
            );
            // A new, empty target only needs adding to.
            ManagedS3Backend::restore(&target, &target_config, &source, false).await
        }
        .await;
        if let Err(e) = restored {
            target
                .remove_unowned(&target.get_container_name(), &data_volume)
                .await;
            return Err(e.into());
        }

        let mut parameters = HashMap::new();
        if let Some(map) = target_parameters.as_object() {
            for (key, value) in map {
                match value {
                    serde_json::Value::String(s) => {
                        parameters.insert(key.clone(), s.clone());
                    }
                    serde_json::Value::Array(_) => {
                        parameters.insert(key.clone(), value.to_string());
                    }
                    _ => {}
                }
            }
        }
        Ok(super::NewServiceRestoreResult {
            parameters,
            connection_info: format!(
                "s3://{}:***@{}:{}",
                new_config.access_key, new_config.host, new_config.port
            ),
        })
    }
}

/// Service-level (not environment-scoped) connection variables, mirroring the
/// shape every managed S3 service reports.
fn service_env(endpoint: &str, parameters: &HashMap<String, String>) -> HashMap<String, String> {
    let access_key = parameters.get("access_key").cloned().unwrap_or_default();
    let secret_key = parameters.get("secret_key").cloned().unwrap_or_default();
    let region = parameters
        .get("region")
        .cloned()
        .unwrap_or_else(|| "us-east-1".to_string());
    HashMap::from([
        ("BLOB_ENDPOINT".to_string(), endpoint.to_string()),
        ("BLOB_ACCESS_KEY".to_string(), access_key.clone()),
        ("BLOB_SECRET_KEY".to_string(), secret_key.clone()),
        ("BLOB_REGION".to_string(), region),
        ("S3_ENDPOINT".to_string(), endpoint.to_string()),
        ("AWS_ACCESS_KEY_ID".to_string(), access_key),
        ("AWS_SECRET_ACCESS_KEY".to_string(), secret_key),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::managed_s3::{derive_env_credentials, plan_buckets};

    fn config() -> GarageConfig {
        GarageConfig {
            port: "39000".to_string(),
            admin_port: "39003".to_string(),
            access_key: "GK0123456789abcdef01234567".to_string(),
            secret_key: "s".repeat(64),
            admin_token: "token-value".to_string(),
            rpc_secret: "r".repeat(64),
            host: "localhost".to_string(),
            region: "us-east-1".to_string(),
            docker_image: DEFAULT_GARAGE_IMAGE.to_string(),
        }
    }

    fn stored_parameters() -> serde_json::Value {
        serde_json::json!({
            "backend": "garage",
            "port": "39000",
            "admin_port": "39003",
            "access_key": "GK0123456789abcdef01234567",
            "secret_key": "s".repeat(64),
            "admin_token": "token-value",
            "rpc_secret": "r".repeat(64),
            "host": "localhost",
            "region": "us-east-1",
            "docker_image": DEFAULT_GARAGE_IMAGE,
            "buckets": ["uploads", "private-files"],
        })
    }

    #[test]
    fn garage_toml_describes_a_single_node_and_holds_no_secrets() {
        let cfg = config();
        let toml = garage_toml("media", &cfg).expect("valid config renders");

        assert!(toml.contains("replication_factor = 1"));
        assert!(toml.contains("db_engine = \"lmdb\""));
        assert!(toml.contains("metadata_dir = \"/var/lib/garage/meta\""));
        assert!(toml.contains("data_dir = \"/var/lib/garage/data\""));
        assert!(toml.contains("rpc_bind_addr = \"0.0.0.0:3901\""));
        assert!(toml.contains("s3_region = \"us-east-1\""));
        assert!(
            toml.contains("[s3_api]\ns3_region = \"us-east-1\"\napi_bind_addr = \"0.0.0.0:3900\"")
        );
        assert!(toml.contains("[admin]\napi_bind_addr = \"0.0.0.0:3903\""));
        for secret in [&cfg.rpc_secret, &cfg.admin_token, &cfg.secret_key] {
            assert!(
                !toml.contains(secret.as_str()),
                "secrets must travel as environment variables, not in garage.toml"
            );
        }
        assert!(
            !toml.contains("rpc_secret") && !toml.contains("admin_token"),
            "the toml must leave both to GARAGE_RPC_SECRET / GARAGE_ADMIN_TOKEN"
        );
    }

    #[test]
    fn garage_toml_rejects_a_region_that_would_break_out_of_the_string() {
        for region in ["", "us east", "x\"\n[admin]", &"a".repeat(65)] {
            let mut cfg = config();
            cfg.region = region.to_string();
            let error = garage_toml("media", &cfg).expect_err("hostile region is rejected");
            assert!(
                matches!(&error, ManagedS3Error::InvalidConfig { service, .. } if service == "media"),
                "unexpected error for region {region:?}: {error}"
            );
        }
    }

    #[test]
    fn generated_secrets_have_the_shapes_garage_requires() {
        let access = generate_access_key();
        assert_eq!(access.len(), 26);
        assert!(access.starts_with("GK"));
        assert!(access[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(generate_secret_key().len(), 64);
        assert_eq!(
            generate_rpc_secret().len(),
            64,
            "Garage needs 32 bytes of hex"
        );
        assert!(generate_rpc_secret().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(generate_secret_key(), generate_secret_key());
    }

    #[test]
    fn derived_environment_credentials_are_valid_garage_keys() {
        let (access, secret) = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            &"s".repeat(64),
            "shop-production",
            GARAGE_CREDENTIAL_FORMAT,
        );
        assert_eq!(access.len(), 26);
        assert!(access.starts_with("GK"));
        assert!(access[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));

        let again = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            &"s".repeat(64),
            "shop-production",
            GARAGE_CREDENTIAL_FORMAT,
        );
        assert_eq!(
            (access.clone(), secret.clone()),
            again,
            "derivation is stable"
        );

        let other = derive_env_credentials(
            ManagedS3BackendKind::Garage,
            &"s".repeat(64),
            "shop-staging",
            GARAGE_CREDENTIAL_FORMAT,
        );
        assert_ne!(access, other.0, "environments never share a key");
    }

    #[test]
    fn stored_config_requires_every_secret_instead_of_generating_one() {
        for field in [
            "port",
            "admin_port",
            "access_key",
            "secret_key",
            "admin_token",
            "rpc_secret",
        ] {
            let mut parameters = stored_parameters();
            parameters.as_object_mut().expect("object").remove(field);
            let error = GarageConfig::from_stored("media", &parameters)
                .expect_err("a stored config without the field must not parse");
            let message = error.to_string();
            assert!(
                message.contains(field) && message.contains("media"),
                "error should name the field and the service: {message}"
            );
        }
        let parsed = GarageConfig::from_stored("media", &stored_parameters())
            .expect("complete config parses");
        assert_eq!(parsed.port, "39000");
        assert_eq!(parsed.docker_image, DEFAULT_GARAGE_IMAGE);
    }

    #[test]
    fn stored_config_rejects_an_unusable_port() {
        let mut parameters = stored_parameters();
        parameters["port"] = serde_json::json!("not-a-port");
        let cfg = GarageConfig::from_stored("media", &parameters).expect("string parses");
        let error = cfg
            .s3_port("media")
            .expect_err("non-numeric port is rejected");
        assert!(error.to_string().contains("not-a-port"));
    }

    #[test]
    fn debug_output_never_contains_secrets() {
        let rendered = format!("{:?}", config());
        for secret in ["token-value", &"r".repeat(64), &"s".repeat(64)] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
    }

    #[test]
    fn names_are_derived_from_the_service_name() {
        assert_eq!(garage_container_name("media"), "garage-media");
        assert_eq!(
            garage_volume_names("media"),
            ["garage_media_data".to_string()]
        );
    }

    #[test]
    fn default_image_is_pinned() {
        assert!(DEFAULT_GARAGE_IMAGE.ends_with(DEFAULT_GARAGE_VERSION));
        assert!(!DEFAULT_GARAGE_IMAGE.contains("latest"));
    }

    fn service() -> GarageService {
        let docker = Arc::new(
            Docker::connect_with_http_defaults()
                .expect("constructing a Docker handle does not contact the daemon"),
        );
        let key = EncryptionService::generate_raw_key().expect("key");
        let encryption = Arc::new(EncryptionService::new(&key).expect("encryption service"));
        GarageService::new("media".to_string(), docker, encryption, ServiceType::S3)
    }

    fn service_config() -> ServiceConfig {
        ServiceConfig {
            name: "media".to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: stored_parameters(),
        }
    }

    #[tokio::test]
    async fn preview_renders_every_declared_bucket_without_contacting_garage() {
        let env = service()
            .preview_runtime_env_vars(service_config(), "shop", "production")
            .await
            .expect("preview renders from the stored secret alone");

        assert_eq!(env["S3_BUCKET"], "shop-production-uploads");
        assert_eq!(env["S3_BUCKET_UPLOADS"], "shop-production-uploads");
        assert_eq!(
            env["S3_BUCKET_PRIVATE_FILES"],
            "shop-production-private-files"
        );
        assert_eq!(env["S3_ENDPOINT"], "http://garage-media:3900");
        assert_eq!(env["AWS_ENDPOINT_URL"], "http://garage-media:3900");
        assert_eq!(env["S3_REGION"], "us-east-1");
        assert_eq!(env["S3_ACCESS_KEY"], env["AWS_ACCESS_KEY_ID"]);
        assert_eq!(env["S3_SECRET_KEY"], env["AWS_SECRET_ACCESS_KEY"]);
        assert!(env["S3_ACCESS_KEY"].starts_with("GK"));
        assert_ne!(
            env["S3_SECRET_KEY"],
            "s".repeat(64),
            "apps must never receive the service secret"
        );
        assert_ne!(env["S3_ACCESS_KEY"], "GK0123456789abcdef01234567");
    }

    #[tokio::test]
    async fn preview_keeps_the_single_legacy_bucket_when_none_are_declared() {
        let mut config = service_config();
        config
            .parameters
            .as_object_mut()
            .expect("object")
            .remove("buckets");
        let env = service()
            .preview_runtime_env_vars(config, "shop", "production")
            .await
            .expect("preview renders");
        assert_eq!(env["S3_BUCKET"], "shop-production");
        assert!(
            !env.keys().any(|k| k.starts_with("S3_BUCKET_")),
            "no per-bucket variables without declared buckets: {env:?}"
        );
    }

    #[tokio::test]
    async fn preview_for_a_service_without_secrets_reports_the_missing_field() {
        let mut config = service_config();
        config
            .parameters
            .as_object_mut()
            .expect("object")
            .remove("secret_key");
        let error = service()
            .preview_runtime_env_vars(config, "shop", "production")
            .await
            .expect_err("no stored secret, no credentials");
        assert!(error.to_string().contains("secret_key"), "{error}");
    }

    #[test]
    fn endpoint_addresses_the_container_for_apps_and_loopback_for_the_control_plane() {
        let endpoint = service().endpoint(&service_config()).expect("endpoint");
        assert_eq!(endpoint.internal_url(), "http://garage-media:3900");
        assert_eq!(endpoint.host_url(), "http://localhost:39000");
        assert_eq!(endpoint.region, "us-east-1");
    }

    #[test]
    fn admin_credentials_come_from_the_stored_service_key() {
        let (access, secret) = service()
            .admin_credentials(&service_config())
            .expect("credentials");
        assert_eq!(access, "GK0123456789abcdef01234567");
        assert_eq!(secret, "s".repeat(64));
    }

    #[test]
    fn environment_keys_for_declared_buckets_match_the_plan() {
        let plan = plan_buckets(
            "shop-production",
            &["uploads".to_string(), "private-files".to_string()],
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].physical, "shop-production-uploads");
    }

    fn inspect(
        running: bool,
        image: &str,
        ports: Vec<(&str, &str)>,
    ) -> bollard::models::ContainerInspectResponse {
        let port_map = ports
            .into_iter()
            .map(|(internal, host)| {
                (
                    internal.to_string(),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("127.0.0.1".to_string()),
                        host_port: Some(host.to_string()),
                    }]),
                )
            })
            .collect();
        bollard::models::ContainerInspectResponse {
            state: Some(bollard::models::ContainerState {
                status: Some(if running {
                    bollard::models::ContainerStateStatusEnum::RUNNING
                } else {
                    bollard::models::ContainerStateStatusEnum::EXITED
                }),
                ..Default::default()
            }),
            config: Some(bollard::models::ContainerConfig {
                image: Some(image.to_string()),
                ..Default::default()
            }),
            network_settings: Some(bollard::models::NetworkSettings {
                ports: Some(port_map),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn ports_are_adopted_only_from_a_running_container_on_the_same_image() {
        let ports = vec![("3900/tcp", "41000"), ("3903/tcp", "41003")];
        assert_eq!(
            adopted_ports_from_inspect(
                &inspect(true, DEFAULT_GARAGE_IMAGE, ports.clone()),
                DEFAULT_GARAGE_IMAGE
            ),
            Some(("41000".to_string(), "41003".to_string()))
        );
        assert_eq!(
            adopted_ports_from_inspect(
                &inspect(false, DEFAULT_GARAGE_IMAGE, ports.clone()),
                DEFAULT_GARAGE_IMAGE
            ),
            None,
            "a stopped container is recreated, so its ports are not adopted"
        );
        assert_eq!(
            adopted_ports_from_inspect(
                &inspect(true, "dxflrs/garage:v1.0.0", ports),
                DEFAULT_GARAGE_IMAGE
            ),
            None,
            "a different image is recreated, so its ports are not adopted"
        );
        assert_eq!(
            adopted_ports_from_inspect(
                &inspect(true, DEFAULT_GARAGE_IMAGE, vec![("3900/tcp", "41000")]),
                DEFAULT_GARAGE_IMAGE
            ),
            None,
            "both ports must be published"
        );
    }

    #[test]
    fn service_level_environment_never_mentions_the_admin_token_or_rpc_secret() {
        let parameters: HashMap<String, String> = stored_parameters()
            .as_object()
            .expect("object")
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        let env = service()
            .get_environment_variables(&parameters)
            .expect("env");
        assert_eq!(env["S3_ENDPOINT"], "http://localhost:39000");
        for value in env.values() {
            assert_ne!(value, "token-value");
            assert_ne!(value, &"r".repeat(64));
        }
        let docker_env = service()
            .get_docker_environment_variables(&parameters)
            .expect("docker env");
        assert_eq!(docker_env["S3_ENDPOINT"], "http://garage-media:3900");
    }

    #[test]
    fn parameter_schema_documents_ports_image_and_buckets_but_not_secrets() {
        let schema = GarageService::parameter_schema().expect("schema");
        let properties = schema["properties"].as_object().expect("properties");
        for field in ["port", "admin_port", "docker_image", "region", "buckets"] {
            assert!(properties.contains_key(field), "schema lacks {field}");
        }
        for field in ["admin_token", "rpc_secret"] {
            assert!(!properties.contains_key(field), "schema must hide {field}");
        }
    }

    #[test]
    fn admin_client_urls_use_the_stored_host_and_admin_port() {
        let admin = GarageAdmin::new("media", &config()).expect("client builds");
        assert_eq!(admin.base_url, "http://localhost:39003");
    }

    #[test]
    fn admin_client_rejects_an_unparseable_admin_port() {
        let mut cfg = config();
        cfg.admin_port = "nope".to_string();
        let error = GarageAdmin::new("media", &cfg)
            .err()
            .expect("bad admin port is a configuration error");
        assert!(error.to_string().contains("admin_port"), "{error}");
    }

    #[test]
    fn readiness_policy_gives_up_after_repeated_restarts() {
        let target = GarageReadinessTarget::new("media".to_string(), config(), None);
        let policy = target.policy();
        assert_eq!(policy.max_restarts, GARAGE_READINESS_MAX_RESTARTS);
        assert_eq!(policy.deadline, GARAGE_READINESS_DEADLINE);
    }
}

/// End-to-end tests against real Garage and RustFS containers. They skip
/// (rather than fail) when no Docker daemon or image source is available.
#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::externalsvc::managed_s3::{
        ManagedS3AccessKeyRequest, ManagedS3Grant, MigrationSource,
    };
    use crate::externalsvc::rustfs::RustfsService;
    use crate::externalsvc::HealthProbeStatus;
    use crate::parameter_strategies::{GarageParameterStrategy, ParameterStrategy};
    use aws_sdk_s3::primitives::ByteStream;
    use sha2::{Digest, Sha256};

    fn docker_or_skip() -> Option<Arc<Docker>> {
        let docker = Docker::connect_with_local_defaults().ok()?;
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {}
            Err(_) => return None,
        }
        Some(Arc::new(docker))
    }

    async fn docker_available() -> Option<Arc<Docker>> {
        let docker = docker_or_skip()?;
        match docker.ping().await {
            Ok(_) => Some(docker),
            Err(e) => {
                println!("Docker daemon not responding ({e}), skipping");
                None
            }
        }
    }

    fn encryption() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new("test_encryption_key_1234567890ab")
                .expect("test encryption service"),
        )
    }

    fn unique_name(prefix: &str) -> String {
        format!("{prefix}-{:08x}", rand::random::<u32>())
    }

    fn client(
        endpoint: &ManagedS3Endpoint,
        access_key: &str,
        secret_key: &str,
    ) -> aws_sdk_s3::Client {
        let credentials =
            aws_sdk_s3::config::Credentials::new(access_key, secret_key, None, None, "it");
        aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::Config::builder()
                .region(Region::new(endpoint.region.clone()))
                .endpoint_url(format!("http://{}:{}", endpoint.host, endpoint.host_port))
                .credentials_provider(credentials)
                .force_path_style(true)
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .build(),
        )
    }

    fn grant_client(endpoint: &ManagedS3Endpoint, grant: &ManagedS3Grant) -> aws_sdk_s3::Client {
        client(endpoint, &grant.access_key_id, &grant.secret_access_key)
    }

    async fn put(c: &aws_sdk_s3::Client, bucket: &str, key: &str, body: &[u8]) -> bool {
        c.put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from(body.to_vec()))
            .send()
            .await
            .is_ok()
    }

    async fn get(c: &aws_sdk_s3::Client, bucket: &str, key: &str) -> Option<Vec<u8>> {
        let output = c.get_object().bucket(bucket).key(key).send().await.ok()?;
        Some(output.body.collect().await.ok()?.into_bytes().to_vec())
    }

    fn request(project: &str, environment: &str, buckets: &[&str]) -> ManagedS3AccessKeyRequest {
        ManagedS3AccessKeyRequest {
            project_scope: project.to_string(),
            environment_scope: environment.to_string(),
            buckets: buckets.iter().map(|b| (*b).to_owned()).collect(),
        }
    }

    /// Removes the service's container and volumes whether the body passed,
    /// failed or panicked.
    async fn run_then_cleanup<F, Fut>(services: Vec<Box<dyn ExternalService>>, body: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let outcome = tokio::spawn(async move { body().await }).await;
        for service in services {
            if let Err(e) = service.remove().await {
                println!("cleanup of '{}' failed: {e}", service.get_name());
            }
        }
        if let Err(join_error) = outcome {
            std::panic::resume_unwind(join_error.into_panic());
        }
    }

    async fn start_garage(
        docker: &Arc<Docker>,
        name: &str,
        buckets: serde_json::Value,
    ) -> (GarageService, ServiceConfig) {
        let strategy = GarageParameterStrategy;
        let mut params = HashMap::new();
        params.insert("buckets".to_string(), buckets);
        strategy
            .auto_generate_missing(&mut params)
            .expect("defaults");
        let mut parameters = serde_json::to_value(&params).expect("parameters");
        let service = GarageService::new(
            name.to_string(),
            docker.clone(),
            encryption(),
            ServiceType::S3,
        );
        let config = ServiceConfig {
            name: name.to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: parameters.clone(),
        };
        let inferred = service.init(config).await.expect("Garage starts");
        for (key, value) in inferred {
            parameters[key] = serde_json::Value::String(value);
        }
        let config = ServiceConfig {
            name: name.to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters,
        };
        (service, config)
    }

    async fn start_rustfs(docker: &Arc<Docker>, name: &str) -> (RustfsService, ServiceConfig) {
        let service = RustfsService::new(name.to_string(), docker.clone(), encryption());
        let mut parameters = serde_json::json!({
            "host": "localhost",
            "region": "us-east-1",
            "docker_image": crate::externalsvc::rustfs::DEFAULT_RUSTFS_IMAGE,
        });
        let config = ServiceConfig {
            name: name.to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters: parameters.clone(),
        };
        let inferred = service.init(config).await.expect("RustFS starts");
        for (key, value) in inferred {
            parameters[key] = serde_json::Value::String(value);
        }
        let config = ServiceConfig {
            name: name.to_string(),
            service_type: ServiceType::S3,
            version: None,
            parameters,
        };
        (service, config)
    }

    #[tokio::test]
    async fn garage_scopes_credentials_to_their_environment_and_buckets() {
        let Some(docker) = docker_available().await else {
            println!("Docker not available, skipping");
            return;
        };
        let name = unique_name("temps-it-garage");
        let (service, config) =
            start_garage(&docker, &name, serde_json::json!(["uploads", "media"])).await;
        let handle =
            GarageService::new(name.clone(), docker.clone(), encryption(), ServiceType::S3);

        let cleanup: Vec<Box<dyn ExternalService>> = vec![Box::new(service)];
        run_then_cleanup(cleanup, move || async move {
            let health = ManagedS3Backend::health_check(&handle, &config)
                .await
                .expect("health probe runs");
            assert_eq!(health.status, HealthProbeStatus::Operational, "{health:?}");

            let endpoint = handle.endpoint(&config).expect("endpoint");
            let prod_request = request("shop", "production", &["uploads", "media"]);
            let staging_request = request("shop", "staging", &["uploads"]);
            let prod = handle
                .create_access_key(&config, &prod_request)
                .await
                .expect("prod grant");
            let staging = handle
                .create_access_key(&config, &staging_request)
                .await
                .expect("staging grant");
            assert_ne!(prod.access_key_id, staging.access_key_id);
            assert_eq!(prod.buckets.len(), 2);

            // Idempotent: a redeploy gets the very same credentials.
            let again = handle
                .create_access_key(&config, &prod_request)
                .await
                .expect("again");
            assert_eq!(again.access_key_id, prod.access_key_id);
            assert_eq!(again.secret_access_key, prod.secret_access_key);

            let prod_client = grant_client(&endpoint, &prod);
            let staging_client = grant_client(&endpoint, &staging);
            let prod_uploads = prod.buckets[0].physical.clone();
            let prod_media = prod.buckets[1].physical.clone();
            let staging_uploads = staging.buckets[0].physical.clone();

            // Allowed: put and get on each granted bucket with each key.
            for bucket in [&prod_uploads, &prod_media] {
                assert!(
                    put(&prod_client, bucket, "hello.txt", b"prod").await,
                    "put {bucket}"
                );
                assert_eq!(
                    get(&prod_client, bucket, "hello.txt").await.as_deref(),
                    Some(&b"prod"[..])
                );
            }
            assert!(put(&staging_client, &staging_uploads, "hello.txt", b"staging").await);
            assert_eq!(
                get(&staging_client, &staging_uploads, "hello.txt")
                    .await
                    .as_deref(),
                Some(&b"staging"[..])
            );

            // Denied: no key reaches another environment's buckets.
            assert!(
                !put(&staging_client, &prod_uploads, "x", b"x").await,
                "staging must not write prod"
            );
            assert!(get(&staging_client, &prod_media, "hello.txt")
                .await
                .is_none());
            assert!(
                !put(&prod_client, &staging_uploads, "x", b"x").await,
                "prod must not write staging"
            );
            assert!(get(&prod_client, &staging_uploads, "hello.txt")
                .await
                .is_none());

            // Revoking an environment cuts it off and leaves the others.
            handle
                .revoke_access_key(&config, "shop", "staging")
                .await
                .expect("revoke");
            assert!(
                !put(&staging_client, &staging_uploads, "y", b"y").await,
                "revoked key must not write"
            );
            assert!(get(&staging_client, &staging_uploads, "hello.txt")
                .await
                .is_none());
            assert!(put(&prod_client, &prod_uploads, "still.txt", b"ok").await);
            // The data survives the revocation.
            let admin = client(
                &endpoint,
                &handle.admin_credentials(&config).expect("admin").0,
                &handle.admin_credentials(&config).expect("admin").1,
            );
            assert_eq!(
                get(&admin, &staging_uploads, "hello.txt").await.as_deref(),
                Some(&b"staging"[..])
            );

            // A re-grant restores access (credentials are deterministic).
            let regrant = handle
                .create_access_key(&config, &staging_request)
                .await
                .expect("regrant");
            assert_eq!(regrant.access_key_id, staging.access_key_id);
            assert!(put(&staging_client, &staging_uploads, "z", b"z").await);
        })
        .await;
    }

    async fn checksums(c: &aws_sdk_s3::Client, bucket: &str) -> Vec<(String, String)> {
        let listing = c
            .list_objects_v2()
            .bucket(bucket)
            .send()
            .await
            .expect("list objects");
        let mut out = Vec::new();
        for object in listing.contents() {
            let key = object.key().unwrap_or_default().to_string();
            let body = get(c, bucket, &key).await.expect("object body");
            out.push((key, hex::encode(Sha256::digest(&body))));
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn data_migrates_between_rustfs_and_garage_in_both_directions() {
        let Some(docker) = docker_available().await else {
            println!("Docker not available, skipping");
            return;
        };
        let (rustfs, rustfs_config) = start_rustfs(&docker, &unique_name("temps-it-rfs")).await;
        let (garage, garage_config) =
            start_garage(&docker, &unique_name("temps-it-grg"), serde_json::json!([])).await;
        let rustfs_handle = RustfsService::new(rustfs.get_name(), docker.clone(), encryption());
        let garage_handle = GarageService::new(
            garage.get_name(),
            docker.clone(),
            encryption(),
            ServiceType::S3,
        );

        let cleanup: Vec<Box<dyn ExternalService>> = vec![Box::new(rustfs), Box::new(garage)];
        run_then_cleanup(cleanup, move || async move {
            let rustfs_endpoint = rustfs_handle
                .endpoint(&rustfs_config)
                .expect("rustfs endpoint");
            let garage_endpoint = garage_handle
                .endpoint(&garage_config)
                .expect("garage endpoint");
            let (rk, rs) = rustfs_handle
                .admin_credentials(&rustfs_config)
                .expect("rustfs admin");
            let (gk, gs) = garage_handle
                .admin_credentials(&garage_config)
                .expect("garage admin");
            let rustfs_admin = client(&rustfs_endpoint, &rk, &rs);
            let garage_admin = client(&garage_endpoint, &gk, &gs);

            // RustFS -> Garage
            rustfs_handle
                .create_bucket(&rustfs_config, "photos")
                .await
                .expect("bucket");
            for (key, body) in [
                ("a.txt", "alpha"),
                ("nested/b.bin", "bravo-bravo"),
                ("c.txt", ""),
                ("nested/deeper/empty", ""),
            ] {
                assert!(
                    put(&rustfs_admin, "photos", key, body.as_bytes()).await,
                    "seed {key}"
                );
            }
            let source = MigrationSource {
                service: rustfs_handle.service_name(),
                backend: ManagedS3BackendKind::Rustfs,
                endpoint: rustfs_endpoint.clone(),
                admin_access_key: rk.clone(),
                admin_secret_key: rs.clone(),
            };
            let report = garage_handle
                .migrate_from(&garage_config, &source)
                .await
                .expect("rustfs -> garage");
            assert!(report.buckets.contains(&"photos".to_string()), "{report:?}");
            let before = checksums(&rustfs_admin, "photos").await;
            assert_eq!(before.len(), 4);
            assert_eq!(checksums(&garage_admin, "photos").await, before);

            // Garage -> RustFS (carries a new object back, keeps the old)
            assert!(put(&garage_admin, "photos", "d.txt", b"delta").await);
            let source = MigrationSource {
                service: garage_handle.service_name(),
                backend: ManagedS3BackendKind::Garage,
                endpoint: garage_endpoint.clone(),
                admin_access_key: gk.clone(),
                admin_secret_key: gs.clone(),
            };
            rustfs_handle
                .migrate_from(&rustfs_config, &source)
                .await
                .expect("garage -> rustfs");
            let after = checksums(&garage_admin, "photos").await;
            assert_eq!(after.len(), 5);
            assert_eq!(checksums(&rustfs_admin, "photos").await, after);
        })
        .await;
    }

    #[tokio::test]
    async fn garage_backup_restores_into_a_fresh_garage_service() {
        let Some(docker) = docker_available().await else {
            println!("Docker not available, skipping");
            return;
        };
        let (store, store_config) = start_rustfs(&docker, &unique_name("temps-it-store")).await;
        let (source, source_config) =
            start_garage(&docker, &unique_name("temps-it-src"), serde_json::json!([])).await;
        let (target, target_config) =
            start_garage(&docker, &unique_name("temps-it-dst"), serde_json::json!([])).await;
        let store_handle = RustfsService::new(store.get_name(), docker.clone(), encryption());
        let source_handle = GarageService::new(
            source.get_name(),
            docker.clone(),
            encryption(),
            ServiceType::S3,
        );
        let target_handle = GarageService::new(
            target.get_name(),
            docker.clone(),
            encryption(),
            ServiceType::S3,
        );

        let cleanup: Vec<Box<dyn ExternalService>> =
            vec![Box::new(store), Box::new(source), Box::new(target)];
        run_then_cleanup(cleanup, move || async move {
            let store_endpoint = store_handle
                .endpoint(&store_config)
                .expect("store endpoint");
            let (sk, ss) = store_handle
                .admin_credentials(&store_config)
                .expect("store admin");
            store_handle
                .create_bucket(&store_config, "backups")
                .await
                .expect("backup bucket");
            let destination = S3Location {
                endpoint: Some(store_endpoint.internal_url()),
                access_key: sk,
                secret_key: ss,
                session_token: None,
                bucket: "backups".to_string(),
                prefix: "garage/run-1".to_string(),
            };

            let source_endpoint = source_handle
                .endpoint(&source_config)
                .expect("source endpoint");
            let (ak, asec) = source_handle
                .admin_credentials(&source_config)
                .expect("admin");
            let source_admin = client(&source_endpoint, &ak, &asec);
            source_handle
                .create_bucket(&source_config, "docs")
                .await
                .expect("bucket");
            assert!(put(&source_admin, "docs", "one.txt", b"first").await);
            assert!(put(&source_admin, "docs", "dir/two.txt", b"second").await);

            let backed_up = source_handle
                .backup(&source_config, &destination)
                .await
                .expect("backup");
            assert!(
                backed_up.buckets.contains(&"docs".to_string()),
                "{backed_up:?}"
            );

            let restored = target_handle
                .restore(&target_config, &destination, false)
                .await
                .expect("restore into a new service");
            assert!(
                restored.buckets.contains(&"docs".to_string()),
                "{restored:?}"
            );

            let target_endpoint = target_handle
                .endpoint(&target_config)
                .expect("target endpoint");
            let (tk, ts) = target_handle
                .admin_credentials(&target_config)
                .expect("admin");
            let target_admin = client(&target_endpoint, &tk, &ts);
            assert_eq!(
                checksums(&target_admin, "docs").await,
                checksums(&source_admin, "docs").await
            );
        })
        .await;
    }
}
