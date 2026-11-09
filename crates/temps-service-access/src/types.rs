// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Exact destination and request scope. The broker must validate this again
/// against its supported protocols before installing a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxAccessPolicy {
    /// HTTPS origin only; no path, userinfo, query, fragment or non-443 port.
    pub origin: String,
    /// Segment-boundary path prefixes, for example `/owner/private-repo`.
    pub path_prefixes: Vec<String>,
    pub methods: Vec<AccessMethod>,
    /// Use the project-bound GitHub connection on the host. No provider token is returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repository: Option<GithubRepositoryAccess>,
}

/// Immutable reference to the project's existing GitHub repository connection.
/// Changing the connection or repository requires a new policy and fresh consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubRepositoryAccess {
    pub connection_id: i32,
    pub owner: String,
    pub repository: String,
    pub protocol: GithubAccessProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GithubAccessProtocol {
    /// Read repository REST endpoints using GET and HEAD.
    Api,
    /// Clone/fetch over Git smart HTTPS; push and Git LFS are denied.
    Git,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum AccessMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveAccessService {
    pub name: String,
    pub policy: SandboxAccessPolicy,
    /// Maximum lifetime of a request, between 60 and 86400 seconds.
    pub max_ttl_seconds: i32,
    /// Allows requests to be saved; does not enable a network connection.
    pub enabled: bool,
    /// Write-only bearer credential; omitted on update keeps the existing one.
    /// Empty strings are rejected. Use clear_credential to remove it.
    #[schema(write_only)]
    pub credential: Option<temps_core::secret_string::SecretString>,
    #[serde(default)]
    pub clear_credential: bool,
    /// Required for updates; omit for creation.
    pub expected_version: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessServiceResponse {
    pub id: String,
    pub project_id: i32,
    pub name: String,
    pub policy: SandboxAccessPolicy,
    pub max_ttl_seconds: i32,
    pub enabled: bool,
    pub has_credential: bool,
    pub version: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestServiceGrant {
    pub service_id: String,
    pub service_version: i32,
    /// Absolute expiry is calculated at creation, even if the broker is unavailable.
    pub ttl_seconds: i32,
}

/// Explicit operator approval, separate from a saved request. Reuse request_id
/// only when retrying this exact approval; it never extends an existing grant.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveServiceGrant {
    #[schema(value_type = String, format = "uuid")]
    pub request_id: uuid::Uuid,
    pub service_id: String,
    pub service_version: i32,
    pub ttl_seconds: i32,
}

/// Durable approval receipt. HTTP 202 does not mean the worker is connected.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceGrantApproval {
    pub grant_id: String,
    pub sandbox_id: String,
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeServiceGrant {
    pub expected_version: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceGrantStatus {
    IntegrationUnavailable,
    Expired,
    Revoked,
    ServiceDisabled,
    PolicyChanged,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceGrantResponse {
    pub id: String,
    pub service_id: String,
    pub service_name: String,
    pub sandbox_id: String,
    pub service_version: i32,
    pub policy: SandboxAccessPolicy,
    pub status: ServiceGrantStatus,
    pub reason: String,
    /// Always false until an enforcement adapter is implemented.
    pub connected: bool,
    pub version: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct AccessPageQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}
impl AccessPageQuery {
    pub fn normalized(&self) -> (u64, u64) {
        (
            self.page.unwrap_or(1).clamp(1, 1_000_000),
            self.page_size.unwrap_or(20).clamp(1, 100),
        )
    }
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessServiceList {
    pub items: Vec<AccessServiceResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceGrantList {
    pub items: Vec<ServiceGrantResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
}
