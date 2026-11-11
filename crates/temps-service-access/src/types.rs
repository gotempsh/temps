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
    /// Explicit provider identity selected by the operator; never inferred from a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AccessServiceProvider>,
    /// HTTPS origin only; no path, userinfo, query, fragment or non-443 port.
    pub origin: String,
    /// Segment-boundary path prefixes, for example `/owner/private-repo`.
    pub path_prefixes: Vec<String>,
    pub methods: Vec<AccessMethod>,
    /// Use the selected GitHub connection on the host. No provider token is returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repository: Option<GithubRepositoryAccess>,
    /// How the stored credential reaches the destination. Omitted means a
    /// bearer token in `Authorization`, the shape every older policy has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<CredentialDelivery>,
}

/// How a stored credential is presented to the destination, plus any fixed
/// headers the API requires. Changing it changes the policy, so consent given
/// for the previous delivery does not carry over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialDelivery {
    #[serde(default)]
    pub format: CredentialFormat,
    /// Fixed, non-secret headers sent with every request, such as an API
    /// version. At most 16; values are printable ASCII and never templates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<RequestHeader>,
}

/// The credential is one stored secret. Formats differ only in how it is sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialFormat {
    /// `Authorization: Bearer <credential>`.
    #[default]
    Bearer,
    /// `<name>: <prefix><credential>`, for example `x-api-key`.
    Header {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    /// `Authorization: Basic base64(<username>:<credential>)` with a fixed username.
    BasicToken { username: String },
    /// `Authorization: Basic base64(<credential>)`; the stored credential is
    /// `username:password`.
    Basic,
}

/// What the catalog knows about a saved service's destination: how to show it
/// and where its credential is created, so a key can be rotated from the edit page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CatalogReference {
    pub id: String,
    pub name: String,
    pub logo: String,
    /// One label for a single credential, or two (username, password).
    pub credential_labels: Vec<String>,
    pub credential_help: CredentialHelp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CredentialHelp {
    /// HTTPS page where this service's key or token is created.
    pub url: Option<String>,
    /// Notes the service's template gives for its credential fields.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestHeader {
    pub name: String,
    pub value: String,
}

/// Immutable reference to an explicitly selected GitHub connection and synced repository.
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
    /// Add a newly created service to this vault in the same transaction.
    #[serde(default)]
    pub vault_id: Option<String>,
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
    pub project_id: Option<i32>,
    pub name: String,
    pub policy: SandboxAccessPolicy,
    pub max_ttl_seconds: i32,
    pub enabled: bool,
    pub has_credential: bool,
    /// The catalog service this origin belongs to, when it is one. Derived on
    /// read from the origin; never stored in the policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogReference>,
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
    PendingApproval,
    Approved,
    IntegrationUnavailable,
    Expired,
    Revoked,
    ServiceDisabled,
    PolicyChanged,
}

/// Observed worker enforcement, independent of the requested authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantEnforcement {
    NotRequested,
    Pending,
    Active,
    Stopping,
    Stopped,
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
    pub enforcement: GrantEnforcement,
    /// True only for a current, live acknowledgment of this exact grant.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessServiceProvider {
    Github,
    Gitlab,
    Openai,
    Anthropic,
    Stripe,
    Resend,
    Sendgrid,
    Vercel,
    Netlify,
    Mistral,
    Groq,
}
