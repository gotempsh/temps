// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-scoped traffic delivery planning and application.
//!
//! Delivery profiles describe how traffic reaches an origin. DNS hosting is a
//! separate concern: every write still goes through `ManagedDnsRecordService`.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use sea_orm::sea_query::{extension::postgres::PgExpr, Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, EntityTrait, Order, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_core::PaginationParams;
use temps_entities::{
    custom_routes, delivery_profiles, dns_managed_domains, dns_providers, domain_delivery_bindings,
    domain_delivery_previews, environment_delivery_settings, environment_domains, environments,
    project_custom_domains, project_delivery_settings, projects, settings,
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    errors::{DeliveryIncomplete, DeliveryOperation, DeliveryStep, DnsError},
    providers::{DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType},
    services::{ManagedDnsRecordService, OwnershipScope, RecordOwnership},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryProviderKind {
    Direct,
    Cloudflare,
    Bunny,
}

impl DeliveryProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Cloudflare => "cloudflare",
            Self::Bunny => "bunny",
        }
    }
    fn parse(value: &str) -> Result<Self, DnsError> {
        match value {
            "direct" => Ok(Self::Direct),
            "cloudflare" => Ok(Self::Cloudflare),
            "bunny" => Ok(Self::Bunny),
            _ => Err(DnsError::Validation(format!(
                "Unknown delivery provider kind '{value}'"
            ))),
        }
    }
}

pub trait DeliveryAdapter: Send + Sync {
    fn validate_dns_provider(&self, provider: &dns_providers::Model) -> Result<(), DnsError>;
    fn plan(
        &self,
        hostname: &str,
        zone: &str,
        origin_target: &str,
    ) -> Result<DeliveryRequirements, DnsError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginTlsPolicy {
    ExistingCertificate,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeliveryRequirements {
    pub record: DeliveryRecordRequirement,
    pub origin_tls: OriginTlsPolicy,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeliveryRecordRequirement {
    pub name: String,
    pub record_type: DnsRecordType,
    pub content: DnsRecordContent,
    pub value: String,
    pub proxied: bool,
    pub ttl: Option<u32>,
}

struct DirectAdapter;
impl DeliveryAdapter for DirectAdapter {
    fn validate_dns_provider(&self, _provider: &dns_providers::Model) -> Result<(), DnsError> {
        Ok(())
    }
    fn plan(
        &self,
        hostname: &str,
        zone: &str,
        origin_target: &str,
    ) -> Result<DeliveryRequirements, DnsError> {
        let (name, record_type, content) =
            DomainDeliveryService::record(zone, hostname, origin_target)?;
        Ok(DeliveryRequirements {
            record: DeliveryRecordRequirement {
                name,
                record_type,
                content,
                value: origin_target.into(),
                proxied: false,
                ttl: Some(300),
            },
            origin_tls: OriginTlsPolicy::ExistingCertificate,
            warnings: vec![],
        })
    }
}
struct CloudflareAdapter;
impl DeliveryAdapter for CloudflareAdapter {
    fn validate_dns_provider(&self, provider: &dns_providers::Model) -> Result<(), DnsError> {
        if provider.provider_type != "cloudflare" {
            return Err(DnsError::Validation(format!(
                "Cloudflare delivery requires a Cloudflare DNS provider; provider {} is '{}'",
                provider.id, provider.provider_type
            )));
        }
        Ok(())
    }
    fn plan(
        &self,
        hostname: &str,
        zone: &str,
        origin_target: &str,
    ) -> Result<DeliveryRequirements, DnsError> {
        let (name, record_type, content) =
            DomainDeliveryService::record(zone, hostname, origin_target)?;
        Ok(DeliveryRequirements{record:DeliveryRecordRequirement{name,record_type,content,value:origin_target.into(),proxied:true,ttl:None},origin_tls:OriginTlsPolicy::ExistingCertificate,warnings:vec!["Temps preserves the current origin certificate and Cloudflare zone TLS mode. Full (strict) requires a certificate trusted by Cloudflare.".into()]})
    }
}

struct BunnyAdapter {
    hostname: String,
}
impl DeliveryAdapter for BunnyAdapter {
    fn validate_dns_provider(&self, _provider: &dns_providers::Model) -> Result<(), DnsError> {
        Ok(())
    }
    fn plan(
        &self,
        hostname: &str,
        zone: &str,
        _origin_target: &str,
    ) -> Result<DeliveryRequirements, DnsError> {
        if hostname == zone {
            return Err(DnsError::Validation(format!(
                "Bunny delivery for '{hostname}' needs a subdomain: a CNAME cannot be created at the zone apex"
            )));
        }
        let (name, record_type, content) =
            DomainDeliveryService::record(zone, hostname, &self.hostname)?;
        Ok(DeliveryRequirements {
            record: DeliveryRecordRequirement {
                name, record_type, content, value: self.hostname.clone(), proxied: false, ttl: Some(300),
            },
            origin_tls: OriginTlsPolicy::ExistingCertificate,
            warnings: vec!["Bunny will request a free edge certificate after the CNAME is written; DNS propagation can require a retry.".into()],
        })
    }
}

fn adapter(
    kind: DeliveryProviderKind,
    profile: &delivery_profiles::Model,
) -> Result<Box<dyn DeliveryAdapter>, DnsError> {
    match kind {
        DeliveryProviderKind::Direct => Ok(Box::new(DirectAdapter)),
        DeliveryProviderKind::Cloudflare => Ok(Box::new(CloudflareAdapter)),
        DeliveryProviderKind::Bunny => Ok(Box::new(BunnyAdapter {
            hostname: profile.bunny_hostname.clone().ok_or_else(|| {
                DnsError::Validation(format!(
                    "Bunny delivery profile {} is missing its CDN hostname",
                    profile.id
                ))
            })?,
        })),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BunnyZone {
    id: i64,
    name: String,
    origin_url: Option<String>,
    enabled: bool,
    suspended: bool,
    add_host_header: bool,
    #[serde(default)]
    hostnames: Vec<BunnyHostname>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BunnyHostname {
    value: String,
    is_system_hostname: bool,
    has_certificate: bool,
}

impl BunnyZone {
    fn validate(&self, expected_id: i64) -> Result<String, DnsError> {
        if self.id != expected_id || !self.enabled || self.suspended {
            return Err(DnsError::Validation(format!(
                "Bunny Pull Zone {expected_id} is unavailable or suspended"
            )));
        }
        if !self.add_host_header {
            return Err(DnsError::Validation(format!("Enable 'Add Host Header' on Bunny Pull Zone {expected_id} so Temps can route each project hostname")));
        }
        let origin = self
            .origin_url
            .as_deref()
            .and_then(|url| reqwest::Url::parse(url).ok());
        if !origin
            .as_ref()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        {
            return Err(DnsError::Validation(format!("Bunny Pull Zone {expected_id} needs an HTTP or HTTPS origin URL pointing at the Temps edge")));
        }
        self.hostnames
            .iter()
            .find(|hostname| hostname.is_system_hostname && hostname.value.ends_with(".b-cdn.net"))
            .map(|hostname| hostname.value.clone())
            .ok_or_else(|| {
                DnsError::Validation(format!(
                    "Bunny Pull Zone {expected_id} ({}) has no system b-cdn.net hostname",
                    self.name
                ))
            })
    }

    fn validate_origin(&self, target: &str, hostname: &str) -> Result<(), DnsError> {
        let origin = self
            .origin_url
            .as_deref()
            .and_then(|value| reqwest::Url::parse(value).ok());
        let host = origin.as_ref().and_then(|url| url.host_str());
        if !host.is_some_and(|host| host.eq_ignore_ascii_case(target.trim_end_matches('.'))) {
            return Err(DnsError::Validation(format!("Bunny Pull Zone {} origin is {:?}, but the requested Temps edge target is '{target}'; set the Pull Zone origin to that target before previewing delivery", self.id, host)));
        }
        if target.trim_end_matches('.').eq_ignore_ascii_case(hostname)
            || self.hostnames.iter().any(|entry| {
                entry
                    .value
                    .eq_ignore_ascii_case(target.trim_end_matches('.'))
            })
        {
            return Err(DnsError::Validation(format!("Bunny Pull Zone {} origin target '{target}' points back to a Bunny hostname and would create a traffic loop", self.id)));
        }
        Ok(())
    }
}

/// Upper bound for a successful Bunny API response body. A Pull Zone with
/// every optional field populated is a few hundred KB; anything larger is not
/// a response this client should buffer.
const BUNNY_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound for an error body; it is only ever logged, never returned.
const BUNNY_MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// Upstream error text kept in debug logs.
const BUNNY_LOGGED_ERROR_CHARS: usize = 512;

struct BunnyApi {
    /// `None` when the HTTP client could not be built; requests then fail
    /// with an actionable error instead of panicking at startup.
    client: Option<reqwest::Client>,
    base_url: String,
}

/// Read at most `limit` bytes of a response body. Returns `Ok(None)` when the
/// body is larger than `limit` (the remainder is never buffered).
async fn read_bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Option<Vec<u8>>, reqwest::Error> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

impl BunnyApi {
    fn new() -> Self {
        Self::with_base_url("https://api.bunny.net".into())
    }

    fn with_base_url(base_url: String) -> Self {
        // Never follow redirects: reqwest forwards custom headers such as
        // `AccessKey` to the redirect target, which could leak the key.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                tracing::error!("Failed to initialize Bunny CDN API client: {error}");
            })
            .ok();
        Self { client, base_url }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        key: &str,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response, DnsError> {
        let client = self.client.as_ref().ok_or_else(|| {
            DnsError::ApiError(
                "Bunny CDN API client failed to initialize; check the server's TLS setup".into(),
            )
        })?;
        let mut access_key = reqwest::header::HeaderValue::from_str(key).map_err(|_| {
            DnsError::InvalidCredentials("Bunny API key contains invalid header characters".into())
        })?;
        access_key.set_sensitive(true);
        let mut request = client
            .request(method, format!("{}{path}", self.base_url))
            .header("AccessKey", access_key)
            .timeout(std::time::Duration::from_secs(15));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| DnsError::ConnectionFailed(format!("Bunny API {path}: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            Self::log_upstream_error(path, status, key, response).await;
            // Upstream bodies never reach the error: they can echo request
            // data (including the key) in forms a substring redaction misses,
            // and this text is returned to API callers and persisted in
            // `last_error` columns readable by project members.
            let code = status.as_u16();
            return Err(match code {
                401 | 403 => DnsError::InvalidCredentials(format!(
                    "Bunny API key cannot access {path} (HTTP {code}); check that the key is an account API key with access to this Pull Zone"
                )),
                400 => DnsError::Validation(format!(
                    "Bunny API rejected the request to {path} (HTTP 400); check the Pull Zone configuration in the bunny.net dashboard"
                )),
                404 => DnsError::Validation(format!(
                    "Bunny API resource {path} was not found (HTTP 404); check the Pull Zone ID"
                )),
                429 => DnsError::RateLimited(format!(
                    "Bunny API rate limited the request to {path} (HTTP 429); retry shortly"
                )),
                _ => DnsError::ApiError(format!(
                    "Bunny API request to {path} failed (HTTP {code})"
                )),
            });
        }
        Ok(response)
    }

    /// Debug-log a truncated, key-redacted upstream error body for operators.
    async fn log_upstream_error(
        path: &str,
        status: reqwest::StatusCode,
        key: &str,
        response: reqwest::Response,
    ) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let body = match read_bounded_body(response, BUNNY_MAX_ERROR_BODY_BYTES).await {
            Ok(Some(body)) => String::from_utf8_lossy(&body).into_owned(),
            Ok(None) => "<body exceeded log limit>".to_string(),
            Err(error) => format!("<body unreadable: {error}>"),
        };
        let mut message: String = body.chars().take(BUNNY_LOGGED_ERROR_CHARS).collect();
        if !key.is_empty() {
            message = message.replace(key, "[REDACTED]");
        }
        tracing::debug!(
            path,
            status = status.as_u16(),
            upstream_message = %message,
            "Bunny API request failed"
        );
    }

    async fn get_zone(&self, id: i64, key: &str) -> Result<BunnyZone, DnsError> {
        let path = format!("/pullzone/{id}");
        let response = self.request(reqwest::Method::GET, &path, key, None).await?;
        let body = read_bounded_body(response, BUNNY_MAX_RESPONSE_BYTES)
            .await
            .map_err(|error| {
                DnsError::ConnectionFailed(format!(
                    "Bunny API {path}: failed to read Pull Zone {id} response: {error}"
                ))
            })?
            .ok_or_else(|| {
                DnsError::ApiError(format!(
                    "Bunny API {path}: Pull Zone {id} response exceeded {BUNNY_MAX_RESPONSE_BYTES} bytes"
                ))
            })?;
        serde_json::from_slice::<BunnyZone>(&body).map_err(|error| {
            DnsError::ConnectionFailed(format!(
                "Bunny Pull Zone {id} returned invalid data: {error}"
            ))
        })
    }

    /// Drain a success response whose body is not used, bounded so a
    /// misbehaving upstream cannot make this client buffer it.
    async fn discard_body(path: &str, response: reqwest::Response) -> Result<(), DnsError> {
        read_bounded_body(response, BUNNY_MAX_RESPONSE_BYTES)
            .await
            .map_err(|error| {
                DnsError::ConnectionFailed(format!(
                    "Bunny API {path}: failed to read response: {error}"
                ))
            })?
            .ok_or_else(|| {
                DnsError::ApiError(format!(
                    "Bunny API {path}: response exceeded {BUNNY_MAX_RESPONSE_BYTES} bytes"
                ))
            })?;
        Ok(())
    }

    async fn add_hostname(&self, id: i64, hostname: &str, key: &str) -> Result<(), DnsError> {
        let path = format!("/pullzone/{id}/addHostname");
        let response = self
            .request(
                reqwest::Method::POST,
                &path,
                key,
                Some(serde_json::json!({"Hostname": hostname})),
            )
            .await?;
        Self::discard_body(&path, response).await
    }

    async fn remove_hostname(&self, id: i64, hostname: &str, key: &str) -> Result<(), DnsError> {
        let path = format!("/pullzone/{id}/removeHostname");
        let response = self
            .request(
                reqwest::Method::DELETE,
                &path,
                key,
                Some(serde_json::json!({"Hostname": hostname})),
            )
            .await?;
        Self::discard_body(&path, response).await
    }

    async fn load_certificate(&self, hostname: &str, key: &str) -> Result<(), DnsError> {
        let path = format!("/pullzone/loadFreeCertificate?hostname={hostname}&useOnlyHttp01=true");
        let response = self.request(reqwest::Method::GET, &path, key, None).await?;
        Self::discard_body(&path, response).await
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeliveryCapabilityResponse {
    pub provider_kind: DeliveryProviderKind,
    pub name: String,
    pub supported: bool,
    pub configured: bool,
    pub requirements: Vec<String>,
    pub setup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeliveryProfileResponse {
    pub id: i32,
    pub name: String,
    pub provider_kind: DeliveryProviderKind,
    /// Bunny Pull Zone ID. Omitted (`null`) for callers without DNS provider
    /// read access, who only see the profile's name and kind.
    pub bunny_pull_zone_id: Option<i64>,
    /// Bunny system CDN hostname. Omitted (`null`) for callers without DNS
    /// provider read access.
    pub bunny_hostname: Option<String>,
    #[schema(value_type=String,format=DateTime)]
    pub created_at: chrono::DateTime<Utc>,
    #[schema(value_type=String,format=DateTime)]
    pub updated_at: chrono::DateTime<Utc>,
}
impl DeliveryProfileResponse {
    /// The view for callers without DNS provider read access: identity and
    /// delivery kind only, no provider account details.
    pub fn without_provider_details(mut self) -> Self {
        self.bunny_pull_zone_id = None;
        self.bunny_hostname = None;
        self
    }
}
impl ProjectDeliverySettingsResponse {
    /// See [`DeliveryProfileResponse::without_provider_details`].
    pub fn without_provider_details(mut self) -> Self {
        self.effective_default_profile = self
            .effective_default_profile
            .map(DeliveryProfileResponse::without_provider_details);
        self
    }
}
impl TryFrom<delivery_profiles::Model> for DeliveryProfileResponse {
    type Error = DnsError;
    fn try_from(v: delivery_profiles::Model) -> Result<Self, Self::Error> {
        Ok(Self {
            id: v.id,
            name: v.name,
            provider_kind: DeliveryProviderKind::parse(&v.provider_kind)?,
            bunny_pull_zone_id: v.bunny_pull_zone_id,
            bunny_hostname: v.bunny_hostname,
            created_at: v.created_at,
            updated_at: v.updated_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentDeliveryOverride {
    pub environment_id: i32,
    pub profile_id: Option<i32>,
}
fn last_profile_needed_for_new_projects(
    provider_kind: &str,
    future_default_kind: Option<&str>,
    has_other_profile: bool,
) -> bool {
    future_default_kind == Some(provider_kind) && !has_other_profile
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectDeliverySettingsResponse {
    pub project_id: i32,
    pub default_profile_id: Option<i32>,
    pub environment_overrides: Vec<EnvironmentDeliveryOverride>,
    pub effective_default_profile: Option<DeliveryProfileResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PreviewDomainDeliveryBindingRequest {
    pub hostname: String,
    pub environment_id: i32,
    pub dns_provider_id: i32,
    pub zone: String,
    pub origin_target: String,
    pub delivery_profile_id: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdoptDeliveryRecord {
    pub name: String,
    pub record_type: DnsRecordType,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeliveryRecordPlan {
    pub name: String,
    pub record_type: DnsRecordType,
    pub value: String,
    pub proxied: bool,
    pub ownership_status: String,
    pub requires_adoption: bool,
    /// The record live at the provider when the preview was taken. Apply
    /// refuses if the provider no longer holds exactly this record.
    pub expected_existing_record: Option<DnsRecord>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeliveryRoutingPlan {
    pub will_create_custom_domain: bool,
    pub custom_domain_id: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DomainDeliveryPreviewResponse {
    #[schema(value_type=String)]
    pub preview_id: Uuid,
    #[schema(value_type=String,format=DateTime)]
    pub expires_at: chrono::DateTime<Utc>,
    pub profile_id: i32,
    pub profile_source: String,
    pub provider_kind: DeliveryProviderKind,
    pub origin_tls: OriginTlsPolicy,
    pub record: DeliveryRecordPlan,
    pub routing: DeliveryRoutingPlan,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DomainDeliveryBindingResponse {
    pub id: i32,
    pub hostname: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub custom_domain_id: i32,
    pub delivery_profile_id: i32,
    pub delivery_profile_name: String,
    pub profile_source: String,
    pub provider_kind: DeliveryProviderKind,
    pub dns_provider_id: i32,
    pub zone: String,
    pub origin_target: String,
    pub record_type: DnsRecordType,
    pub proxied: bool,
    /// Whether Temps added this hostname to the Bunny Pull Zone. Only then
    /// does removing the binding also detach the hostname, and its edge
    /// certificate, from the Pull Zone. `false` when the hostname was already
    /// on the Pull Zone before Temps set up delivery: removal leaves it
    /// attached. Always `false` for Cloudflare and direct bindings.
    pub bunny_hostname_owned: bool,
    pub status: String,
    pub last_error: Option<String>,
    #[schema(value_type=String,format=DateTime)]
    pub applied_at: Option<chrono::DateTime<Utc>>,
    #[schema(value_type=String,format=DateTime)]
    pub created_at: chrono::DateTime<Utc>,
    #[schema(value_type=String,format=DateTime)]
    pub updated_at: chrono::DateTime<Utc>,
}

/// One page of delivery profiles (`GET /delivery-profiles`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeliveryProfilePage {
    pub items: Vec<DeliveryProfileResponse>,
    /// Number of delivery profiles across all pages.
    pub total: u64,
    /// 1-based number of this page.
    pub page: u64,
    /// Page size applied to the request, after clamping to 1..=100.
    pub page_size: u64,
}
impl DeliveryProfilePage {
    /// See [`DeliveryProfileResponse::without_provider_details`].
    pub fn without_provider_details(mut self) -> Self {
        self.items = self
            .items
            .into_iter()
            .map(DeliveryProfileResponse::without_provider_details)
            .collect();
        self
    }
}

/// One page of a project's domain delivery bindings
/// (`GET /projects/{project_id}/domain-delivery-bindings`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DomainDeliveryBindingPage {
    pub items: Vec<DomainDeliveryBindingResponse>,
    /// Number of the project's bindings across all pages.
    pub total: u64,
    /// 1-based number of this page.
    pub page: u64,
    /// Page size applied to the request, after clamping to 1..=100.
    pub page_size: u64,
}

/// Direction of a delivery list sort, parsed case-insensitively from
/// `sort_order`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortDirection {
    Asc,
    Desc,
}
impl SortDirection {
    /// A missing or blank value means `desc`, so lists default to newest first.
    fn parse(value: Option<&str>, list: &str) -> Result<Self, DnsError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::Desc),
            Some(value) if value.eq_ignore_ascii_case("desc") => Ok(Self::Desc),
            Some(value) if value.eq_ignore_ascii_case("asc") => Ok(Self::Asc),
            Some(other) => Err(DnsError::Validation(format!(
                "Invalid sort_order '{other}' for {list}; allowed values: asc, desc"
            ))),
        }
    }
    fn order(self) -> Order {
        match self {
            Self::Asc => Order::Asc,
            Self::Desc => Order::Desc,
        }
    }
}

/// `sort_by` values accepted when listing delivery profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileSortBy {
    CreatedAt,
    Name,
}
impl ProfileSortBy {
    const ALLOWED: &'static str = "created_at, name";
    /// A missing or blank value means `created_at`.
    fn parse(value: Option<&str>) -> Result<Self, DnsError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("created_at") => Ok(Self::CreatedAt),
            Some("name") => Ok(Self::Name),
            Some(other) => Err(DnsError::Validation(format!(
                "Invalid sort_by '{other}' for delivery profiles; allowed values: {}",
                Self::ALLOWED
            ))),
        }
    }
    fn column(self) -> delivery_profiles::Column {
        match self {
            Self::CreatedAt => delivery_profiles::Column::CreatedAt,
            Self::Name => delivery_profiles::Column::Name,
        }
    }
}

/// `sort_by` values accepted when listing a project's domain delivery bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingSortBy {
    CreatedAt,
    Hostname,
    UpdatedAt,
}
impl BindingSortBy {
    const ALLOWED: &'static str = "created_at, hostname, updated_at";
    /// A missing or blank value means `created_at`.
    fn parse(value: Option<&str>) -> Result<Self, DnsError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("created_at") => Ok(Self::CreatedAt),
            Some("hostname") => Ok(Self::Hostname),
            Some("updated_at") => Ok(Self::UpdatedAt),
            Some(other) => Err(DnsError::Validation(format!(
                "Invalid sort_by '{other}' for domain delivery bindings; allowed values: {}",
                Self::ALLOWED
            ))),
        }
    }
    fn column(self) -> domain_delivery_bindings::Column {
        match self {
            Self::CreatedAt => domain_delivery_bindings::Column::CreatedAt,
            Self::Hostname => domain_delivery_bindings::Column::Hostname,
            Self::UpdatedAt => domain_delivery_bindings::Column::UpdatedAt,
        }
    }
}

/// Whether 1-based `page` starts at or after the last of `total` rows. The
/// offset saturates, so an absurd `page` neither overflows nor reaches the
/// database.
fn page_is_past_end(page: u64, page_size: u64, total: u64) -> bool {
    page.saturating_sub(1).saturating_mul(page_size) >= total
}

/// Longest `search` term accepted when listing delivery profiles. A profile
/// name is at most 100 characters, so a longer term could never match.
pub const PROFILE_SEARCH_MAX_CHARS: usize = 100;

/// The `search` term of a delivery profile listing: trimmed, `None` when
/// blank (no filter), and refused when longer than any profile name can be.
fn profile_search_term(search: Option<&str>) -> Result<Option<&str>, DnsError> {
    let Some(term) = search.map(str::trim).filter(|term| !term.is_empty()) else {
        return Ok(None);
    };
    let length = term.chars().count();
    if length > PROFILE_SEARCH_MAX_CHARS {
        return Err(DnsError::Validation(format!(
            "Delivery profile search is {length} characters long; the maximum is {PROFILE_SEARCH_MAX_CHARS}"
        )));
    }
    Ok(Some(term))
}

/// `%term%` for a substring match with `\`, PostgreSQL's default LIKE escape
/// character: `\`, `%` and `_` in `term` are escaped so they match
/// themselves, not any text.
fn substring_like_pattern(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len() + 2);
    pattern.push('%');
    for character in term.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

#[async_trait]
pub trait DomainDeliveryDns: Send + Sync {
    async fn record_ownership(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError>;
    /// Ownership of a binding's record, read through the binding's own DNS
    /// provider and zone. Cleanup uses this so a binding stays removable
    /// after its zone stops being verified or auto-managed.
    async fn record_ownership_for_provider(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError>;
    async fn import_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError>;
    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        scope: OwnershipScope,
    ) -> Result<crate::providers::DnsRecord, DnsError>;
    /// Remove a binding's record through the binding's own DNS provider and
    /// zone (see [`Self::record_ownership_for_provider`]).
    async fn remove_record(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError>;

    async fn import_record_with_transaction(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        _transaction: &DatabaseTransaction,
    ) -> Result<(), DnsError> {
        self.import_record(domain, name, record_type, scope).await
    }

    async fn set_record_with_transaction(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        scope: OwnershipScope,
        _transaction: &DatabaseTransaction,
    ) -> Result<crate::providers::DnsRecord, DnsError> {
        self.set_record(domain, request, proxied, scope).await
    }

    async fn remove_record_with_transaction(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        _transaction: &DatabaseTransaction,
    ) -> Result<(), DnsError> {
        self.remove_record(provider_id, zone, name, record_type, scope)
            .await
    }

    /// Take the cross-process provider-record lock for `name` in `domain` on
    /// `transaction`. It is the same advisory lock every managed DNS writer
    /// takes, and it is re-entrant for the transaction that holds it.
    async fn lock_record_with_transaction(
        &self,
        domain: &str,
        name: &str,
        transaction: &DatabaseTransaction,
    ) -> Result<(), DnsError> {
        ManagedDnsRecordService::lock_record_on_transaction(transaction, domain, name).await
    }
}
#[async_trait]
impl DomainDeliveryDns for ManagedDnsRecordService {
    async fn record_ownership(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        self.record_ownership(domain, name, record_type).await
    }
    async fn record_ownership_for_provider(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<RecordOwnership, DnsError> {
        ManagedDnsRecordService::record_ownership_for_provider(
            self,
            provider_id,
            zone,
            name,
            record_type,
        )
        .await
    }
    async fn import_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        self.import_record(domain, name, record_type, scope)
            .await
            .map(|_| ())
    }
    async fn import_record_with_transaction(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        transaction: &DatabaseTransaction,
    ) -> Result<(), DnsError> {
        ManagedDnsRecordService::import_record_with_transaction(
            self,
            domain,
            name,
            record_type,
            scope,
            Some(transaction),
        )
        .await
        .map(|_| ())
    }
    async fn set_record(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        scope: OwnershipScope,
    ) -> Result<crate::providers::DnsRecord, DnsError> {
        self.set_managed_record(domain, request, proxied, scope)
            .await
    }
    async fn set_record_with_transaction(
        &self,
        domain: &str,
        request: DnsRecordRequest,
        proxied: Option<bool>,
        scope: OwnershipScope,
        transaction: &DatabaseTransaction,
    ) -> Result<crate::providers::DnsRecord, DnsError> {
        self.set_managed_record_with_transaction(domain, request, proxied, scope, Some(transaction))
            .await
    }
    async fn remove_record(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
    ) -> Result<(), DnsError> {
        self.remove_managed_record_for_provider(provider_id, zone, name, record_type, scope)
            .await
    }
    async fn remove_record_with_transaction(
        &self,
        provider_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
        scope: OwnershipScope,
        transaction: &DatabaseTransaction,
    ) -> Result<(), DnsError> {
        self.remove_managed_record_for_provider_with_transaction(
            provider_id,
            zone,
            name,
            record_type,
            scope,
            Some(transaction),
        )
        .await
    }
}

/// Ownership controller stamped on every record domain delivery writes.
pub(crate) const DELIVERY_CONTROLLER: &str = "domain-delivery";

/// What a preview row's `plan` column stores: the plan returned to the user
/// plus apply bookkeeping that is never part of an API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDeliveryPlan {
    #[serde(flatten)]
    plan: DomainDeliveryPreviewResponse,
    /// Set once an apply attempt wrote and read back the DNS record. Absent
    /// on rows that were never applied (and on rows from older releases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dns_write_receipt: Option<DeliveryDnsWriteReceipt>,
}

/// Proof that an apply attempt of a preview wrote `record` and the provider
/// read it back. A retry after a later step failed resumes from it instead of
/// refusing Temps' own write as a change made after preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryDnsWriteReceipt {
    record: DnsRecord,
    written_at: chrono::DateTime<Utc>,
}

/// The route and binding an apply reserved before any provider call.
struct DeliveryReservation {
    custom_domain: project_custom_domains::Model,
    binding: domain_delivery_bindings::Model,
    created_custom_domain: bool,
}

/// What an apply or cleanup has changed so far, so a failure can report (and
/// the handler can audit) exactly which steps were already done.
struct DeliveryProgress {
    operation: DeliveryOperation,
    project_id: i32,
    environment_id: i32,
    hostname: String,
    preview_id: Option<Uuid>,
    binding_id: Option<i32>,
    completed: Vec<DeliveryStep>,
}

impl DeliveryProgress {
    fn complete(&mut self, step: DeliveryStep) {
        self.completed.push(step);
    }

    /// The error to return when `failed_step` failed with `source`: `source`
    /// unchanged when nothing had changed yet, otherwise a
    /// [`DnsError::DeliveryIncomplete`] naming what already had.
    fn failure(&self, failed_step: DeliveryStep, source: DnsError) -> DnsError {
        if self.completed.is_empty() {
            return source;
        }
        DnsError::DeliveryIncomplete(Box::new(DeliveryIncomplete {
            operation: self.operation,
            project_id: self.project_id,
            environment_id: self.environment_id,
            hostname: self.hostname.clone(),
            preview_id: self.preview_id,
            binding_id: self.binding_id,
            completed_steps: self.completed.clone(),
            failed_step,
            source,
        }))
    }
}

/// Tag an error with the delivery step that produced it.
fn at_step(step: DeliveryStep) -> impl FnOnce(DnsError) -> (DeliveryStep, DnsError) {
    move |error| (step, error)
}

pub struct DomainDeliveryService {
    db: Arc<DatabaseConnection>,
    managed: Arc<dyn DomainDeliveryDns>,
    encryption: Arc<temps_core::EncryptionService>,
    bunny: BunnyApi,
}
impl DomainDeliveryService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        managed: Arc<ManagedDnsRecordService>,
        encryption: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            managed,
            encryption,
            bunny: BunnyApi::new(),
        }
    }
    pub fn with_dns(
        db: Arc<DatabaseConnection>,
        managed: Arc<dyn DomainDeliveryDns>,
        encryption: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            managed,
            encryption,
            bunny: BunnyApi::new(),
        }
    }

    /// Send Bunny CDN API calls to `base_url` instead of
    /// `https://api.bunny.net`, e.g. an API-compatible double in tests.
    pub fn with_bunny_api_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.bunny = BunnyApi::with_base_url(base_url.into());
        self
    }

    fn bunny_credentials(
        &self,
        profile: &delivery_profiles::Model,
    ) -> Result<(i64, String), DnsError> {
        let zone_id = profile.bunny_pull_zone_id.ok_or_else(|| {
            DnsError::Validation(format!("Bunny profile {} has no Pull Zone ID", profile.id))
        })?;
        let encrypted = profile.bunny_api_key_encrypted.as_deref().ok_or_else(|| {
            DnsError::Validation(format!("Bunny profile {} has no API key", profile.id))
        })?;
        let key = self.encryption.decrypt_string(encrypted).map_err(|error| {
            DnsError::Decryption(format!("Bunny profile {} API key: {error}", profile.id))
        })?;
        Ok((zone_id, key))
    }

    async fn bunny_zone_for_target(
        &self,
        profile: &delivery_profiles::Model,
        target: &str,
        hostname: &str,
    ) -> Result<BunnyZone, DnsError> {
        let (zone_id, key) = self.bunny_credentials(profile)?;
        let zone = self.bunny.get_zone(zone_id, &key).await?;
        let cdn_hostname = zone.validate(zone_id)?;
        if profile.bunny_hostname.as_deref() != Some(cdn_hostname.as_str()) {
            return Err(DnsError::Validation(format!(
                "Bunny Pull Zone {zone_id} system hostname changed; recreate delivery profile {}",
                profile.id
            )));
        }
        zone.validate_origin(target, hostname)?;
        Ok(zone)
    }

    async fn acquire_delivery_lock(&self, hostname: &str) -> Result<DatabaseTransaction, DnsError> {
        let transaction = self.db.begin().await?;
        let key = format!(
            "domain-delivery:{}",
            hostname.trim().trim_end_matches('.').to_ascii_lowercase()
        );
        let row = transaction
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_try_advisory_xact_lock(hashtext($1)) AS acquired",
                [key.into()],
            ))
            .await?
            .ok_or_else(|| {
                DnsError::Database(sea_orm::DbErr::Custom(
                    "PostgreSQL advisory lock query returned no row".into(),
                ))
            })?;
        let acquired: bool = row.try_get("", "acquired")?;
        if !acquired {
            return Err(DnsError::RecordConflict {
                domain: hostname.into(), name: hostname.into(), record_type: "BINDING".into(),
                reason: "another domain delivery operation is already running for this hostname; retry when it completes".into(),
            });
        }
        Ok(transaction)
    }

    pub async fn capabilities(&self) -> Result<Vec<DeliveryCapabilityResponse>, DnsError> {
        let cloudflare_configured = dns_providers::Entity::find()
            .filter(dns_providers::Column::ProviderType.eq("cloudflare"))
            .filter(dns_providers::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .is_some();
        Ok(vec![
            DeliveryCapabilityResponse {
                provider_kind: DeliveryProviderKind::Direct,
                name: "Direct origin".into(),
                supported: true,
                configured: true,
                requirements: vec!["A public IPv4/IPv6 address or origin hostname".into()],
                setup_path: None,
            },
            DeliveryCapabilityResponse {
                provider_kind: DeliveryProviderKind::Cloudflare,
                name: "Cloudflare proxy".into(),
                supported: true,
                configured: cloudflare_configured,
                requirements: vec![
                    "An active Cloudflare DNS provider connection".into(),
                    "A verified, auto-managed Cloudflare zone".into(),
                    "A valid origin certificate when Full (strict) is enabled".into(),
                ],
                setup_path: Some("/dns-providers".into()),
            },
            DeliveryCapabilityResponse {
                provider_kind: DeliveryProviderKind::Bunny,
                name: "bunny.net CDN".into(),
                supported: true,
                configured: delivery_profiles::Entity::find()
                    .filter(delivery_profiles::Column::ProviderKind.eq("bunny"))
                    .one(self.db.as_ref())
                    .await?
                    .is_some(),
                requirements: vec![
                    "A Bunny API key and an active Pull Zone with Add Host Header enabled".into(),
                    "A Pull Zone origin pointing to the Temps edge target".into(),
                    "A verified, auto-managed DNS zone for each domain".into(),
                ],
                setup_path: Some("/delivery-profiles".into()),
            },
        ])
    }

    pub async fn create_profile(
        &self,
        name: String,
        kind: DeliveryProviderKind,
    ) -> Result<DeliveryProfileResponse, DnsError> {
        self.create_profile_with_bunny(name, kind, None, None).await
    }

    pub async fn create_profile_with_bunny(
        &self,
        name: String,
        kind: DeliveryProviderKind,
        bunny_pull_zone_id: Option<i64>,
        bunny_api_key: Option<String>,
    ) -> Result<DeliveryProfileResponse, DnsError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(DnsError::Validation(
                "Delivery profile name must contain 1 to 100 characters".into(),
            ));
        }
        let (zone_id, hostname, encrypted_key) = if kind == DeliveryProviderKind::Bunny {
            let zone_id = bunny_pull_zone_id.filter(|id| *id > 0).ok_or_else(|| {
                DnsError::Validation("Bunny delivery requires a positive Pull Zone ID".into())
            })?;
            let key = bunny_api_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| DnsError::Validation("Bunny delivery requires an API key".into()))?;
            let zone = self.bunny.get_zone(zone_id, key).await?;
            let hostname = zone.validate(zone_id)?;
            let encrypted = self
                .encryption
                .encrypt_string(key)
                .map_err(|e| DnsError::Encryption(format!("Bunny profile '{name}': {e}")))?;
            (Some(zone_id), Some(hostname), Some(encrypted))
        } else {
            if bunny_pull_zone_id.is_some() || bunny_api_key.is_some() {
                return Err(DnsError::Validation(
                    "Bunny credentials are only valid for a Bunny delivery profile".into(),
                ));
            }
            (None, None, None)
        };
        delivery_profiles::ActiveModel {
            name: Set(name.into()),
            provider_kind: Set(kind.as_str().into()),
            bunny_pull_zone_id: Set(zone_id),
            bunny_hostname: Set(hostname),
            bunny_api_key_encrypted: Set(encrypted_key),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?
        .try_into()
    }
    /// One page of delivery profiles, newest first unless `sort_by`
    /// (`created_at`, `name`) or `sort_order` (`asc`, `desc`) say otherwise.
    /// Profile ID breaks ties in the same direction, so rows sharing a sort
    /// value are never repeated or skipped across pages.
    ///
    /// `search` keeps only profiles whose name contains it, ignoring case.
    /// It is trimmed first; a blank term matches every profile, and `%`, `_`
    /// and `\` match themselves. Terms over [`PROFILE_SEARCH_MAX_CHARS`]
    /// characters are refused before any query runs. `total` counts the
    /// matching profiles only.
    pub async fn list_profiles(
        &self,
        params: PaginationParams,
        search: Option<&str>,
    ) -> Result<DeliveryProfilePage, DnsError> {
        let sort_by = ProfileSortBy::parse(params.sort_by.as_deref())?;
        let direction = SortDirection::parse(params.sort_order.as_deref(), "delivery profiles")?;
        let search = profile_search_term(search)?;
        let (page, page_size) = params.normalize();
        let mut query = delivery_profiles::Entity::find();
        if let Some(term) = search {
            // No ESCAPE clause: backslash is already PostgreSQL's LIKE escape
            // character, and sea-query's rendering of one after the PostgreSQL
            // ILIKE operator is a syntax error.
            query = query.filter(
                Expr::col((delivery_profiles::Entity, delivery_profiles::Column::Name))
                    .ilike(substring_like_pattern(term)),
            );
        }
        let paginator = query
            .order_by(sort_by.column(), direction.order())
            .order_by(delivery_profiles::Column::Id, direction.order())
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = if page_is_past_end(page, page_size, total) {
            Vec::new()
        } else {
            paginator.fetch_page(page - 1).await?
        };
        let items = rows
            .into_iter()
            .map(DeliveryProfileResponse::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DeliveryProfilePage {
            items,
            total,
            page,
            page_size,
        })
    }
    /// One delivery profile by ID.
    pub async fn get_profile(&self, id: i32) -> Result<DeliveryProfileResponse, DnsError> {
        self.profile(id).await?.try_into()
    }
    /// Delete an unreferenced profile, returning what was deleted.
    pub async fn delete_profile(&self, id: i32) -> Result<DeliveryProfileResponse, DnsError> {
        let exists = delivery_profiles::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(DnsError::DeliveryProfileNotFound { profile_id: id })?;
        let referenced = project_delivery_settings::Entity::find()
            .filter(project_delivery_settings::Column::DefaultProfileId.eq(id))
            .one(self.db.as_ref())
            .await?
            .is_some()
            || environment_delivery_settings::Entity::find()
                .filter(environment_delivery_settings::Column::ProfileId.eq(id))
                .one(self.db.as_ref())
                .await?
                .is_some()
            || domain_delivery_bindings::Entity::find()
                .filter(domain_delivery_bindings::Column::ProfileId.eq(id))
                .one(self.db.as_ref())
                .await?
                .is_some();
        if referenced {
            return Err(DnsError::ResourceInUse {
                resource: "delivery profile",
                id: exists.id,
                name: exists.name,
                reason: "it is still referenced by a project, environment, or domain binding"
                    .into(),
            });
        }
        if exists.provider_kind == "cloudflare" || exists.provider_kind == "bunny" {
            let defaults = settings::Entity::find_by_id(1)
                .one(self.db.as_ref())
                .await?
                .map(|row| temps_core::AppSettings::from_json(row.data))
                .unwrap_or_default();
            let default_kind = if defaults.cloudflare_new_projects {
                Some("cloudflare")
            } else if defaults.bunny_new_projects {
                Some("bunny")
            } else {
                None
            };
            if default_kind == Some(exists.provider_kind.as_str()) {
                let other_profile = delivery_profiles::Entity::find()
                    .filter(delivery_profiles::Column::ProviderKind.eq(&exists.provider_kind))
                    .filter(delivery_profiles::Column::Id.ne(id))
                    .one(self.db.as_ref())
                    .await?;
                if last_profile_needed_for_new_projects(
                    &exists.provider_kind,
                    default_kind,
                    other_profile.is_some(),
                ) {
                    return Err(DnsError::ResourceInUse {
                        resource: "delivery profile",
                        id: exists.id,
                        name: exists.name,
                        reason: format!("it is the last {} profile used by the new-project default; turn that default off first", exists.provider_kind),
                    });
                }
            }
        }
        delivery_profiles::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        exists.try_into()
    }

    async fn require_project(&self, project_id: i32) -> Result<projects::Model, DnsError> {
        projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .filter(|p| !p.is_deleted)
            .ok_or_else(|| DnsError::DomainNotFound(format!("project {project_id}")))
    }
    async fn require_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<environments::Model, DnsError> {
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?;
        Self::environment_in_project(environment.as_ref(), project_id, environment_id).cloned()
    }
    /// `environment` (the row read for `environment_id`, if any) when it is
    /// live and belongs to `project_id`.
    fn environment_in_project(
        environment: Option<&environments::Model>,
        project_id: i32,
        environment_id: i32,
    ) -> Result<&environments::Model, DnsError> {
        let env = environment
            .filter(|e| e.deleted_at.is_none())
            .ok_or_else(|| DnsError::DomainNotFound(format!("environment {environment_id}")))?;
        if env.project_id != project_id {
            return Err(DnsError::Validation(format!(
                "Environment {environment_id} belongs to project {}, not project {project_id}",
                env.project_id
            )));
        }
        Ok(env)
    }
    async fn profile(&self, id: i32) -> Result<delivery_profiles::Model, DnsError> {
        delivery_profiles::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(DnsError::DeliveryProfileNotFound { profile_id: id })
    }

    pub async fn settings(
        &self,
        project_id: i32,
    ) -> Result<ProjectDeliverySettingsResponse, DnsError> {
        self.require_project(project_id).await?;
        let row = project_delivery_settings::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?;
        let environment_ids: Vec<i32> = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|e| e.id)
            .collect();
        let overrides = environment_delivery_settings::Entity::find()
            .filter(environment_delivery_settings::Column::EnvironmentId.is_in(environment_ids))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|s| EnvironmentDeliveryOverride {
                environment_id: s.environment_id,
                profile_id: s.profile_id,
            })
            .collect();
        let default_profile_id = row.and_then(|r| r.default_profile_id);
        let effective_default_profile = match default_profile_id {
            Some(id) => Some(self.profile(id).await?.try_into()?),
            None => None,
        };
        Ok(ProjectDeliverySettingsResponse {
            project_id,
            default_profile_id,
            environment_overrides: overrides,
            effective_default_profile,
        })
    }
    /// Replace a project's default delivery profile and upsert its
    /// environment overrides, all or nothing.
    ///
    /// Every referenced environment and profile is loaded in one query per
    /// table, so validation costs the same number of queries for one override
    /// as for a hundred, and the overrides are written in one statement.
    /// References are checked in request order (default profile first, then
    /// each override's environment and profile), so the first invalid one is
    /// the one reported. When an environment appears more than once, its last
    /// override wins.
    pub async fn update_settings(
        &self,
        project_id: i32,
        default_profile_id: Option<i32>,
        overrides: Vec<EnvironmentDeliveryOverride>,
    ) -> Result<ProjectDeliverySettingsResponse, DnsError> {
        self.require_project(project_id).await?;
        self.validate_settings_references(project_id, default_profile_id, &overrides)
            .await?;
        let latest_overrides: BTreeMap<i32, Option<i32>> = overrides
            .into_iter()
            .map(|item| (item.environment_id, item.profile_id))
            .collect();
        let transaction = self.db.begin().await?;
        if let Some(existing) = project_delivery_settings::Entity::find_by_id(project_id)
            .one(&transaction)
            .await?
        {
            let mut active: project_delivery_settings::ActiveModel = existing.into();
            active.default_profile_id = Set(default_profile_id);
            active.updated_at = Set(Utc::now());
            active.update(&transaction).await?;
        } else {
            project_delivery_settings::ActiveModel {
                project_id: Set(project_id),
                default_profile_id: Set(default_profile_id),
                updated_at: Set(Utc::now()),
            }
            .insert(&transaction)
            .await?;
        }
        if !latest_overrides.is_empty() {
            let now = Utc::now();
            environment_delivery_settings::Entity::insert_many(latest_overrides.into_iter().map(
                |(environment_id, profile_id)| environment_delivery_settings::ActiveModel {
                    environment_id: Set(environment_id),
                    profile_id: Set(profile_id),
                    updated_at: Set(now),
                },
            ))
            .on_conflict(
                OnConflict::column(environment_delivery_settings::Column::EnvironmentId)
                    .update_columns([
                        environment_delivery_settings::Column::ProfileId,
                        environment_delivery_settings::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await?;
        }
        transaction.commit().await?;
        self.settings(project_id).await
    }

    /// Check every profile and environment an `update_settings` request
    /// references: each profile must exist, and each environment must be
    /// live and belong to `project_id`. Loads all of them with one query per
    /// table, then checks in request order (default profile first, then each
    /// override's environment and profile), so the first invalid reference is
    /// the one reported, exactly as when each was read on its own.
    async fn validate_settings_references(
        &self,
        project_id: i32,
        default_profile_id: Option<i32>,
        overrides: &[EnvironmentDeliveryOverride],
    ) -> Result<(), DnsError> {
        let environment_ids: BTreeSet<i32> =
            overrides.iter().map(|item| item.environment_id).collect();
        let referenced_environments: HashMap<i32, environments::Model> =
            if environment_ids.is_empty() {
                HashMap::new()
            } else {
                environments::Entity::find()
                    .filter(environments::Column::Id.is_in(environment_ids))
                    .all(self.db.as_ref())
                    .await?
                    .into_iter()
                    .map(|environment| (environment.id, environment))
                    .collect()
            };
        let profile_ids: BTreeSet<i32> = default_profile_id
            .into_iter()
            .chain(overrides.iter().filter_map(|item| item.profile_id))
            .collect();
        let existing_profile_ids: HashSet<i32> = if profile_ids.is_empty() {
            HashSet::new()
        } else {
            delivery_profiles::Entity::find()
                .select_only()
                .column(delivery_profiles::Column::Id)
                .filter(delivery_profiles::Column::Id.is_in(profile_ids))
                .into_tuple::<i32>()
                .all(self.db.as_ref())
                .await?
                .into_iter()
                .collect()
        };
        let require_profile = |profile_id: i32| {
            if existing_profile_ids.contains(&profile_id) {
                Ok(())
            } else {
                Err(DnsError::DeliveryProfileNotFound { profile_id })
            }
        };
        if let Some(id) = default_profile_id {
            require_profile(id)?;
        }
        for item in overrides {
            Self::environment_in_project(
                referenced_environments.get(&item.environment_id),
                project_id,
                item.environment_id,
            )?;
            if let Some(id) = item.profile_id {
                require_profile(id)?;
            }
        }
        Ok(())
    }

    async fn effective_profile(
        &self,
        project_id: i32,
        environment_id: i32,
        explicit: Option<i32>,
    ) -> Result<(delivery_profiles::Model, String), DnsError> {
        if let Some(id) = explicit {
            return Ok((self.profile(id).await?, "binding".into()));
        }
        if let Some(row) = environment_delivery_settings::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
        {
            if let Some(id) = row.profile_id {
                return Ok((self.profile(id).await?, "environment".into()));
            }
        }
        if let Some(row) = project_delivery_settings::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
        {
            if let Some(id) = row.default_profile_id {
                return Ok((self.profile(id).await?, "project".into()));
            }
        }
        Err(DnsError::Validation(format!("No delivery profile is selected for project {project_id}, environment {environment_id}, or this binding")))
    }
    fn normalized_hostname(host: &str) -> Result<String, DnsError> {
        let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let valid_labels = h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
        if h.is_empty()
            || h.len() > 253
            || !h.contains('.')
            || !valid_labels
            || h.starts_with("_temps-owned")
        {
            return Err(DnsError::Validation(format!(
                "Invalid delivery hostname '{host}'"
            )));
        }
        Ok(h)
    }
    fn record(
        zone: &str,
        hostname: &str,
        target: &str,
    ) -> Result<(String, DnsRecordType, DnsRecordContent), DnsError> {
        let zone = zone.trim().trim_end_matches('.').to_ascii_lowercase();
        if hostname != zone && !hostname.ends_with(&format!(".{zone}")) {
            return Err(DnsError::Validation(format!(
                "Hostname '{hostname}' is outside DNS zone '{zone}'"
            )));
        }
        let name = if hostname == zone {
            "@".into()
        } else {
            hostname[..hostname.len() - zone.len() - 1].into()
        };
        let (ty, content) = match target.parse::<IpAddr>() {
            Ok(IpAddr::V4(_)) => (
                DnsRecordType::A,
                DnsRecordContent::A {
                    address: target.into(),
                },
            ),
            Ok(IpAddr::V6(_)) => (
                DnsRecordType::AAAA,
                DnsRecordContent::AAAA {
                    address: target.into(),
                },
            ),
            Err(_) => {
                let t = Self::normalized_hostname(target)?;
                (DnsRecordType::CNAME, DnsRecordContent::CNAME { target: t })
            }
        };
        Ok((name, ty, content))
    }
    fn ownership_status(v: &RecordOwnership) -> (&'static str, bool) {
        match v {
            RecordOwnership::NotFound => ("not_found", false),
            RecordOwnership::Unmanaged(_) => ("unmanaged", true),
            RecordOwnership::Owned(..) => ("owned", false),
            RecordOwnership::OwnedByOther(..) => ("owned_by_other", false),
            RecordOwnership::Orphaned(_) => ("orphaned", false),
            RecordOwnership::BlockedByOther(_) => ("blocked_by_other", false),
            RecordOwnership::RegistryConflict => ("registry_conflict", false),
        }
    }
    fn existing_record(v: &RecordOwnership) -> Option<&DnsRecord> {
        match v {
            RecordOwnership::Unmanaged(r)
            | RecordOwnership::Owned(r, _)
            | RecordOwnership::OwnedByOther(r, _) => Some(r),
            _ => None,
        }
    }
    /// Whether two optional provider records hold the same DNS data (both
    /// absent counts as the same). Delegates to
    /// [`crate::providers::traits::records_equivalent`]: same record type and
    /// proxied flag, names compared case-insensitively without the root dot,
    /// and canonical content; provider IDs, TTL and metadata are ignored.
    fn same_record(left: Option<&DnsRecord>, right: Option<&DnsRecord>) -> Result<bool, DnsError> {
        // Semantic, not field-for-field: a provider's write response and its
        // later listing spell the same record differently (Route 53 and
        // Google Cloud DNS echo `Origin.Example.NET.`, then list
        // `origin.example.net`), and IDs/TTL/metadata are not DNS data.
        match (left, right) {
            (None, None) => Ok(true),
            (Some(left), Some(right)) => {
                Ok(crate::providers::traits::records_equivalent(left, right))
            }
            _ => Ok(false),
        }
    }
    /// Refuse when a routing record of another type exists at `name`;
    /// changing record type is never done implicitly.
    async fn ensure_no_other_routing_types(
        &self,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        for other in [DnsRecordType::A, DnsRecordType::AAAA, DnsRecordType::CNAME] {
            if other == record_type {
                continue;
            }
            let state = self.managed.record_ownership(zone, name, other).await?;
            if !matches!(
                state,
                RecordOwnership::NotFound | RecordOwnership::Orphaned(_)
            ) {
                return Err(DnsError::RecordConflict {
                    domain: zone.into(),
                    name: name.into(),
                    record_type: other.to_string(),
                    reason: format!(
                        "a conflicting {other} record must be removed explicitly before a {record_type} record can be written"
                    ),
                });
            }
        }
        Ok(())
    }
    /// Ownership scope a binding's DNS record was written under, so cleanup
    /// can only remove the record this exact project/environment owns.
    fn binding_scope(binding: &domain_delivery_bindings::Model) -> OwnershipScope {
        OwnershipScope {
            project_id: Some(binding.project_id),
            environment_id: Some(binding.environment_id),
            controller: Some("domain-delivery"),
        }
    }
    fn validate_owned_scope(
        ownership: &RecordOwnership,
        project_id: i32,
        environment_id: i32,
        zone: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        let marker = match ownership {
            RecordOwnership::Owned(_, marker) | RecordOwnership::Orphaned(marker) => Some(marker),
            _ => None,
        };
        if let Some(marker) = marker {
            if marker.project_id != Some(project_id)
                || marker.environment_id != Some(environment_id)
                || marker.controller.as_deref() != Some("domain-delivery")
            {
                return Err(DnsError::RecordConflict {
                    domain: zone.into(),
                    name: name.into(),
                    record_type: record_type.to_string(),
                    reason: format!(
                        "record is managed for project {:?}, environment {:?}, controller {:?}; domain delivery cannot take it over",
                        marker.project_id, marker.environment_id, marker.controller
                    ),
                });
            }
        }
        Ok(())
    }
    fn fingerprint(
        request: &PreviewDomainDeliveryBindingRequest,
        profile: &delivery_profiles::Model,
        provider: &dns_providers::Model,
        managed: &dns_managed_domains::Model,
    ) -> Result<String, DnsError> {
        let bytes = serde_json::to_vec(&(
            request,
            profile.id,
            &profile.updated_at,
            provider.id,
            provider.is_active,
            &provider.provider_type,
            &provider.updated_at,
            managed.id,
            &managed.updated_at,
        ))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    pub async fn preview(
        &self,
        project_id: i32,
        actor_user_id: i32,
        mut request: PreviewDomainDeliveryBindingRequest,
    ) -> Result<DomainDeliveryPreviewResponse, DnsError> {
        domain_delivery_previews::Entity::delete_many()
            .filter(domain_delivery_previews::Column::ProjectId.eq(project_id))
            .filter(domain_delivery_previews::Column::ExpiresAt.lt(Utc::now()))
            .exec(self.db.as_ref())
            .await?;
        self.require_project(project_id).await?;
        self.require_environment(project_id, request.environment_id)
            .await?;
        request.hostname = Self::normalized_hostname(&request.hostname)?;
        request.zone = Self::normalized_hostname(&request.zone)?;
        let (profile, profile_source) = self
            .effective_profile(
                project_id,
                request.environment_id,
                request.delivery_profile_id,
            )
            .await?;
        let kind = DeliveryProviderKind::parse(&profile.provider_kind)?;
        let adapter = adapter(kind, &profile)?;
        if kind == DeliveryProviderKind::Bunny {
            self.bunny_zone_for_target(&profile, &request.origin_target, &request.hostname)
                .await?;
        }
        let provider = dns_providers::Entity::find_by_id(request.dns_provider_id)
            .one(self.db.as_ref())
            .await?
            .filter(|p| p.is_active)
            .ok_or(DnsError::ProviderNotFound(request.dns_provider_id))?;
        adapter.validate_dns_provider(&provider)?;
        let managed = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(provider.id))
            .filter(dns_managed_domains::Column::Domain.eq(&request.zone))
            .filter(dns_managed_domains::Column::Verified.eq(true))
            .filter(dns_managed_domains::Column::AutoManage.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(request.zone.clone()))?;
        let requirements =
            adapter.plan(&request.hostname, &request.zone, &request.origin_target)?;
        let name = requirements.record.name.clone();
        let record_type = requirements.record.record_type;
        self.ensure_no_other_routing_types(&request.zone, &name, record_type)
            .await?;
        let ownership = self
            .managed
            .record_ownership(&request.zone, &name, record_type)
            .await?;
        Self::validate_owned_scope(
            &ownership,
            project_id,
            request.environment_id,
            &request.zone,
            &name,
            record_type,
        )?;
        let (status, requires_adoption) = Self::ownership_status(&ownership);
        if matches!(
            ownership,
            RecordOwnership::OwnedByOther(..)
                | RecordOwnership::BlockedByOther(..)
                | RecordOwnership::RegistryConflict
        ) {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: name.clone(),
                record_type: record_type.to_string(),
                reason: format!("ownership state is {status}"),
            });
        }
        let existing = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?;
        if custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: request.hostname.clone(),
                record_type: "ROUTE".into(),
                reason: "hostname is already used by a custom proxy route".into(),
            });
        }
        if let Some(environment_domain) = environment_domains::Entity::find()
            .filter(environment_domains::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?
        {
            let environment = environments::Entity::find_by_id(environment_domain.environment_id)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "ROUTE".into(),
                    reason: format!(
                        "hostname is used by missing environment {}",
                        environment_domain.environment_id
                    ),
                })?;
            if environment.project_id != project_id || environment.id != request.environment_id {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "ROUTE".into(),
                    reason: format!(
                        "hostname is already used by environment {} in project {}",
                        environment.id, environment.project_id
                    ),
                });
            }
        }
        if let Some(ref d) = existing {
            if d.project_id != project_id || d.environment_id != request.environment_id {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "ROUTE".into(),
                    reason: format!(
                        "hostname already routes to project {}, environment {}",
                        d.project_id, d.environment_id
                    ),
                });
            }
        }
        if let Some(binding) = domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::Hostname.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?
        {
            if binding.project_id != project_id {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "BINDING".into(),
                    reason: format!("hostname already belongs to project {}", binding.project_id),
                });
            }
            if binding.dns_provider_id != request.dns_provider_id
                || binding.zone != request.zone
                || binding.record_type != record_type.to_string()
            {
                return Err(DnsError::RecordConflict{domain:request.zone.clone(),name:request.hostname.clone(),record_type:"BINDING".into(),reason:"changing a binding's DNS provider, zone, or record type requires removing the existing binding first".into()});
            }
        }
        let fingerprint = Self::fingerprint(&request, &profile, &provider, &managed)?;
        let id = Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(15);
        let response = DomainDeliveryPreviewResponse {
            preview_id: id,
            expires_at,
            profile_id: profile.id,
            profile_source,
            provider_kind: kind,
            origin_tls: requirements.origin_tls,
            record: DeliveryRecordPlan {
                name,
                record_type,
                value: requirements.record.value.clone(),
                proxied: requirements.record.proxied,
                ownership_status: status.into(),
                requires_adoption,
                expected_existing_record: Self::existing_record(&ownership).cloned(),
            },
            routing: DeliveryRoutingPlan {
                will_create_custom_domain: existing.is_none(),
                custom_domain_id: existing.as_ref().map(|d| d.id),
            },
            warnings: requirements.warnings,
        };
        domain_delivery_previews::ActiveModel {
            id: Set(id),
            project_id: Set(project_id),
            actor_user_id: Set(actor_user_id),
            request: Set(serde_json::to_value(&request)?),
            plan: Set(serde_json::to_value(&response)?),
            config_fingerprint: Set(fingerprint),
            status: Set("previewed".into()),
            last_error: Set(None),
            expires_at: Set(expires_at),
            created_at: Set(Utc::now()),
            applied_at: Set(None),
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(response)
    }

    pub async fn apply(
        &self,
        project_id: i32,
        actor_user_id: i32,
        preview_id: Uuid,
        adopt_records: Vec<AdoptDeliveryRecord>,
    ) -> Result<DomainDeliveryBindingResponse, DnsError> {
        let preview = domain_delivery_previews::Entity::find_by_id(preview_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(format!("delivery preview {preview_id}")))?;
        if preview.project_id != project_id {
            return Err(DnsError::DomainNotFound(format!(
                "delivery preview {preview_id} for project {project_id}"
            )));
        }
        if preview.actor_user_id != actor_user_id {
            return Err(DnsError::PermissionDenied(format!(
                "Delivery preview {preview_id} was created by a different user"
            )));
        }
        if preview.expires_at <= Utc::now() {
            return Err(DnsError::Validation(format!(
                "Delivery preview {preview_id} expired; create a new preview"
            )));
        }
        if preview.status == "applied" {
            let binding = domain_delivery_bindings::Entity::find()
                .filter(domain_delivery_bindings::Column::ProjectId.eq(project_id))
                .filter(
                    domain_delivery_bindings::Column::Hostname.eq(serde_json::from_value::<
                        PreviewDomainDeliveryBindingRequest,
                    >(
                        preview.request.clone()
                    )?
                    .hostname),
                )
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| {
                    DnsError::DomainNotFound(format!("binding applied from preview {preview_id}"))
                })?;
            return self.binding_response(binding).await;
        }
        let request: PreviewDomainDeliveryBindingRequest =
            serde_json::from_value(preview.request.clone())?;
        let delivery_lock = self.acquire_delivery_lock(&request.hostname).await?;
        self.require_project(project_id).await?;
        self.require_environment(project_id, request.environment_id)
            .await?;
        let StoredDeliveryPlan {
            plan,
            dns_write_receipt,
        } = serde_json::from_value(preview.plan.clone())?;
        let profile = self.profile(plan.profile_id).await?;
        let bunny_zone = if plan.provider_kind == DeliveryProviderKind::Bunny {
            Some(
                self.bunny_zone_for_target(&profile, &request.origin_target, &request.hostname)
                    .await?,
            )
        } else {
            None
        };
        let provider = dns_providers::Entity::find_by_id(request.dns_provider_id)
            .one(self.db.as_ref())
            .await?
            .filter(|provider| provider.is_active)
            .ok_or(DnsError::ProviderNotFound(request.dns_provider_id))?;
        adapter(
            DeliveryProviderKind::parse(&profile.provider_kind)?,
            &profile,
        )?
        .validate_dns_provider(&provider)?;
        let managed = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(request.dns_provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(&request.zone))
            .filter(dns_managed_domains::Column::Verified.eq(true))
            .filter(dns_managed_domains::Column::AutoManage.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotManaged(request.zone.clone()))?;
        if Self::fingerprint(&request, &profile, &provider, &managed)? != preview.config_fingerprint
        {
            return Err(DnsError::Validation(format!("Delivery preview {preview_id} is stale because its profile or managed zone changed")));
        }
        let current_route = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?;
        if let Some(ref route) = current_route {
            if route.project_id != project_id || route.environment_id != request.environment_id {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "ROUTE".into(),
                    reason: "routing changed after preview; create a new preview".into(),
                });
            }
        }
        if custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: request.hostname.clone(),
                record_type: "ROUTE".into(),
                reason: "a custom proxy route claimed this hostname after preview".into(),
            });
        }
        if let Some(environment_domain) = environment_domains::Entity::find()
            .filter(environment_domains::Column::Domain.eq(&request.hostname))
            .one(self.db.as_ref())
            .await?
        {
            let environment = environments::Entity::find_by_id(environment_domain.environment_id)
                .one(self.db.as_ref())
                .await?;
            if environment
                .as_ref()
                .is_none_or(|env| env.project_id != project_id || env.id != request.environment_id)
            {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "ROUTE".into(),
                    reason: "an environment route claimed this hostname after preview".into(),
                });
            }
        }
        let requirements = adapter(plan.provider_kind, &profile)?.plan(
            &request.hostname,
            &request.zone,
            &request.origin_target,
        )?;
        let record_type = requirements.record.record_type;
        let content = requirements.record.content;
        let requested_adoption = adopt_records
            .iter()
            .any(|r| r.name == plan.record.name && r.record_type == record_type);
        if adopt_records.len() > 1 || (adopt_records.len() == 1 && !requested_adoption) {
            return Err(DnsError::Validation(
                "Only the exact record marked for adoption by this preview may be adopted".into(),
            ));
        }
        if plan.record.requires_adoption && !requested_adoption {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: plan.record.name.clone(),
                record_type: record_type.to_string(),
                reason: "preview requires explicit adoption of this exact record".into(),
            });
        }
        if !plan.record.requires_adoption && !adopt_records.is_empty() {
            return Err(DnsError::Validation(
                "Adoption was requested for a record that the preview did not mark for adoption"
                    .into(),
            ));
        }
        // Take the provider-record lock before re-reading live state. Every
        // Temps DNS writer (managed records API, imports, generated-hostname
        // sync) takes this lock, and the import/write below re-enter it on
        // the same transaction, so nothing can change the record between
        // this check and the adoption that trusts it.
        self.managed
            .lock_record_with_transaction(&request.zone, &plan.record.name, &delivery_lock)
            .await?;
        // Re-check what preview checked: a record of another routing type
        // created since preview would otherwise coexist with ours.
        self.ensure_no_other_routing_types(&request.zone, &plan.record.name, record_type)
            .await?;
        let live_ownership = self
            .managed
            .record_ownership(&request.zone, &plan.record.name, record_type)
            .await?;
        Self::validate_owned_scope(
            &live_ownership,
            project_id,
            request.environment_id,
            &request.zone,
            &plan.record.name,
            record_type,
        )?;
        // The live record must still be the one the user reviewed, unless a
        // failed attempt of this same preview already wrote Temps' own record
        // there: then the retry resumes from that write.
        let resuming = if Self::same_record(
            Self::existing_record(&live_ownership),
            plan.record.expected_existing_record.as_ref(),
        )? {
            false
        } else if preview.status == "failed"
            && Self::receipt_covers_live_record(
                dns_write_receipt.as_ref(),
                &live_ownership,
                project_id,
                request.environment_id,
            )?
        {
            tracing::info!(
                %preview_id,
                project_id,
                environment_id = request.environment_id,
                hostname = %request.hostname,
                "Resuming domain delivery apply from the DNS record a failed attempt already wrote"
            );
            true
        } else {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: plan.record.name.clone(),
                record_type: record_type.to_string(),
                reason: format!(
                    "the provider record changed after delivery preview {preview_id} was taken; create a new preview to review the current record"
                ),
            });
        };

        let claimed = domain_delivery_previews::Entity::update_many()
            .col_expr(
                domain_delivery_previews::Column::Status,
                Expr::value("applying"),
            )
            .filter(domain_delivery_previews::Column::Id.eq(preview_id))
            .filter(domain_delivery_previews::Column::Status.is_in(["previewed", "failed"]))
            .exec(self.db.as_ref())
            .await?;
        if claimed.rows_affected != 1 {
            return Err(DnsError::RecordConflict {
                domain: request.zone.clone(),
                name: request.hostname.clone(),
                record_type: "PREVIEW".into(),
                reason: format!("preview {preview_id} is already being applied"),
            });
        }

        let reservation = match self
            .reserve_route_and_binding(
                project_id,
                &request,
                &plan,
                &profile,
                record_type,
                current_route.as_ref(),
            )
            .await
        {
            Ok(reservation) => reservation,
            Err(error) => {
                // The reservation rolled back as a whole, so nothing changed
                // beyond this preview, which becomes retryable again.
                self.mark_preview_failed(&preview, &error.to_string()).await;
                return Err(error);
            }
        };
        let DeliveryReservation {
            custom_domain,
            binding,
            created_custom_domain,
        } = reservation;
        let mut progress = DeliveryProgress {
            operation: DeliveryOperation::Apply,
            project_id,
            environment_id: request.environment_id,
            hostname: request.hostname.clone(),
            preview_id: Some(preview_id),
            binding_id: Some(binding.id),
            completed: Vec::new(),
        };
        if created_custom_domain {
            progress.complete(DeliveryStep::CustomDomainCreated);
        }
        progress.complete(DeliveryStep::BindingReserved);

        let scope = OwnershipScope {
            project_id: Some(project_id),
            environment_id: Some(request.environment_id),
            controller: Some(DELIVERY_CONTROLLER),
        };
        // Whether Temps added the hostname to the Bunny Pull Zone: what an
        // earlier attempt for this binding recorded, or `true` once this
        // attempt adds it. A hostname already on the Pull Zone is reused and
        // never becomes owned here, so cleanup leaves it in place.
        let mut bunny_hostname_owned = binding.bunny_hostname_owned;
        let provider_steps: Result<(), (DeliveryStep, DnsError)> = async {
            if let Some(zone) = &bunny_zone {
                if !zone
                    .hostnames
                    .iter()
                    .any(|name| name.value.eq_ignore_ascii_case(&request.hostname))
                {
                    let (_, key) = self
                        .bunny_credentials(&profile)
                        .map_err(at_step(DeliveryStep::BunnyHostnameAttached))?;
                    self.bunny
                        .add_hostname(zone.id, &request.hostname, &key)
                        .await
                        .map_err(at_step(DeliveryStep::BunnyHostnameAttached))?;
                    progress.complete(DeliveryStep::BunnyHostnameAttached);
                    // Saved before any later step can fail: a retry finds
                    // the hostname on the Pull Zone and must still know that
                    // Temps put it there.
                    bunny_hostname_owned = true;
                    self.save_bunny_hostname_owned(&binding).await;
                }
            }
            // A resumed retry adopted the record on its first attempt; it is
            // already owned by this binding's scope.
            if requested_adoption && !resuming {
                self.managed
                    .import_record_with_transaction(
                        &request.zone,
                        &plan.record.name,
                        record_type,
                        scope,
                        &delivery_lock,
                    )
                    .await
                    .map_err(at_step(DeliveryStep::DnsRecordAdopted))?;
                progress.complete(DeliveryStep::DnsRecordAdopted);
            }
            let written = self
                .managed
                .set_record_with_transaction(
                    &request.zone,
                    DnsRecordRequest {
                        name: plan.record.name.clone(),
                        content,
                        ttl: requirements.record.ttl,
                        proxied: false,
                    },
                    Some(plan.record.proxied),
                    scope,
                    &delivery_lock,
                )
                .await
                .map_err(at_step(DeliveryStep::DnsRecordWritten))?;
            progress.complete(DeliveryStep::DnsRecordWritten);
            let readback = self
                .managed
                .record_ownership(&request.zone, &plan.record.name, record_type)
                .await
                .map_err(at_step(DeliveryStep::DnsRecordVerified))?;
            if !Self::same_record(Self::existing_record(&readback), Some(&written))
                .map_err(at_step(DeliveryStep::DnsRecordVerified))?
            {
                return Err((
                    DeliveryStep::DnsRecordVerified,
                    DnsError::ConnectionFailed(format!(
                        "Provider readback for {} {} did not match the record written",
                        record_type, request.hostname
                    )),
                ));
            }
            progress.complete(DeliveryStep::DnsRecordVerified);
            self.save_dns_write_receipt(preview_id, &plan, &written)
                .await;
            if let Some(zone) = &bunny_zone {
                let (_, key) = self
                    .bunny_credentials(&profile)
                    .map_err(at_step(DeliveryStep::CertificateRequested))?;
                let current = self
                    .bunny
                    .get_zone(zone.id, &key)
                    .await
                    .map_err(at_step(DeliveryStep::CertificateRequested))?;
                if !current.hostnames.iter().any(|name| {
                    name.value.eq_ignore_ascii_case(&request.hostname) && name.has_certificate
                }) {
                    self.bunny
                        .load_certificate(&request.hostname, &key)
                        .await
                        .map_err(at_step(DeliveryStep::CertificateRequested))?;
                    progress.complete(DeliveryStep::CertificateRequested);
                }
            }
            Ok(())
        }
        .await;
        if let Err((failed_step, error)) = provider_steps {
            let failure = progress.failure(failed_step, error);
            self.mark_apply_failed(
                &binding,
                &preview,
                &failure.to_string(),
                bunny_hostname_owned,
            )
            .await;
            return Err(failure);
        }

        let binding = match self
            .activate_binding(
                &binding,
                &preview,
                custom_domain.status == "active",
                bunny_hostname_owned,
            )
            .await
        {
            Ok(binding) => binding,
            Err(error) => {
                let failure = progress.failure(DeliveryStep::BindingActivated, error);
                self.mark_apply_failed(
                    &binding,
                    &preview,
                    &failure.to_string(),
                    bunny_hostname_owned,
                )
                .await;
                return Err(failure);
            }
        };
        Self::binding_response_with(binding, &profile)
    }

    /// Reserve the hostname's route and binding in one transaction that
    /// commits before any provider call.
    ///
    /// The project and environment rows are share-locked and rechecked first
    /// (see [`Self::lock_delivery_scope`]), so project and environment
    /// deletion cannot fence them while a binding is being reserved in them.
    /// A route read earlier is then re-read `FOR UPDATE` and must still be
    /// this hostname in this project and environment. Custom domain rename,
    /// reassignment and deletion take the same row lock and refuse while a
    /// binding exists, so the route cannot move between this check and the
    /// binding that pins it. The DNS provider and managed zone are pinned
    /// last (see [`Self::lock_delivery_zone`]).
    ///
    /// Lock order: project, environment, route (custom domain), DNS
    /// provider, managed zone. The guards that refuse while bindings exist
    /// each lock only one of these rows, and writers that lock several (an
    /// environment change locks its project, then the environment) take them
    /// in this same order, so none of them can deadlock with a reservation.
    async fn reserve_route_and_binding(
        &self,
        project_id: i32,
        request: &PreviewDomainDeliveryBindingRequest,
        plan: &DomainDeliveryPreviewResponse,
        profile: &delivery_profiles::Model,
        record_type: DnsRecordType,
        current_route: Option<&project_custom_domains::Model>,
    ) -> Result<DeliveryReservation, DnsError> {
        let transaction = self.db.begin().await?;
        Self::lock_delivery_scope(&transaction, project_id, request).await?;
        let (custom_domain, created_custom_domain) = match current_route {
            Some(route) => {
                let locked = project_custom_domains::Entity::find_by_id(route.id)
                    .lock_exclusive()
                    .one(&transaction)
                    .await?;
                match locked {
                    Some(row) if Self::route_unchanged(&row, project_id, request) => (row, false),
                    changed => {
                        return Err(Self::route_moved_error(request, route.id, changed.as_ref()))
                    }
                }
            }
            None => {
                // A route created for this hostname after the read above was
                // never checked by this apply.
                if let Some(row) = project_custom_domains::Entity::find()
                    .filter(project_custom_domains::Column::Domain.eq(&request.hostname))
                    .lock_exclusive()
                    .one(&transaction)
                    .await?
                {
                    return Err(DnsError::RecordConflict {
                        domain: request.zone.clone(),
                        name: request.hostname.clone(),
                        record_type: "ROUTE".into(),
                        reason: format!(
                            "custom domain {} (project {}, environment {}) claimed this hostname after preview; create a new preview",
                            row.id, row.project_id, row.environment_id
                        ),
                    });
                }
                let row = project_custom_domains::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(request.environment_id),
                    domain: Set(request.hostname.clone()),
                    status: Set("pending".into()),
                    message: Set(Some(
                        "DNS delivery configured; awaiting certificate provisioning".into(),
                    )),
                    ..Default::default()
                }
                .insert(&transaction)
                .await?;
                (row, true)
            }
        };
        Self::lock_delivery_zone(&transaction, request).await?;
        let existing = domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::Hostname.eq(&request.hostname))
            .one(&transaction)
            .await?;
        if let Some(ref binding) = existing {
            if binding.project_id != project_id {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(),
                    name: request.hostname.clone(),
                    record_type: "BINDING".into(),
                    reason: format!("hostname is reserved by project {}", binding.project_id),
                });
            }
            if binding.dns_provider_id != request.dns_provider_id
                || binding.zone != request.zone
                || binding.record_type != record_type.to_string()
            {
                return Err(DnsError::RecordConflict {
                    domain: request.zone.clone(), name: request.hostname.clone(), record_type: "BINDING".into(),
                    reason: "changing a binding's DNS provider, zone, or record type requires removing the existing binding first".into(),
                });
            }
        }
        let now = Utc::now();
        let binding = if let Some(existing) = existing {
            // Ownership of the Bunny hostname is kept across retries with the
            // same profile, so a resumed apply that finds the hostname Temps
            // added still owns it. Another profile may use another Pull Zone,
            // where Temps has added nothing yet.
            let profile_changed = existing.profile_id != profile.id;
            let mut active: domain_delivery_bindings::ActiveModel = existing.into();
            if profile_changed {
                active.bunny_hostname_owned = Set(false);
            }
            active.environment_id = Set(request.environment_id);
            active.custom_domain_id = Set(custom_domain.id);
            active.profile_id = Set(profile.id);
            active.profile_source = Set(plan.profile_source.clone());
            active.dns_provider_id = Set(request.dns_provider_id);
            active.zone = Set(request.zone.clone());
            active.origin_target = Set(request.origin_target.clone());
            active.record_type = Set(record_type.to_string());
            active.proxied = Set(plan.record.proxied);
            active.status = Set("applying".into());
            active.last_error = Set(None);
            active.updated_at = Set(now);
            active.update(&transaction).await?
        } else {
            domain_delivery_bindings::ActiveModel {
                hostname: Set(request.hostname.clone()),
                project_id: Set(project_id),
                environment_id: Set(request.environment_id),
                custom_domain_id: Set(custom_domain.id),
                profile_id: Set(profile.id),
                profile_source: Set(plan.profile_source.clone()),
                dns_provider_id: Set(request.dns_provider_id),
                zone: Set(request.zone.clone()),
                origin_target: Set(request.origin_target.clone()),
                record_type: Set(record_type.to_string()),
                proxied: Set(plan.record.proxied),
                status: Set("applying".into()),
                last_error: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                applied_at: Set(None),
                bunny_hostname_owned: Set(false),
                ..Default::default()
            }
            .insert(&transaction)
            .await?
        };
        transaction.commit().await?;
        Ok(DeliveryReservation {
            custom_domain,
            binding,
            created_custom_domain,
        })
    }

    /// Share-lock the project row, then the environment row, that the binding
    /// being reserved belongs to, and refuse unless the project is not being
    /// deleted and the environment is not deleted and still belongs to it.
    ///
    /// A binding's foreign keys only take `FOR KEY SHARE` on these rows, which
    /// does not conflict with the plain `UPDATE` a soft delete writes. Project
    /// deletion (`ProjectService::begin_project_deletion`) and environment
    /// deletion (`EnvironmentService::delete_environment`) therefore lock the
    /// row `FOR UPDATE`, count bindings and write their deletion fence in one
    /// transaction: either they wait for this reservation and then see its
    /// binding, or this waits for them and sees the fence. Without that, a
    /// binding saved behind the fence would block the final project delete
    /// while its cleanup refuses the deleted project.
    async fn lock_delivery_scope(
        transaction: &DatabaseTransaction,
        project_id: i32,
        request: &PreviewDomainDeliveryBindingRequest,
    ) -> Result<(), DnsError> {
        let environment_id = request.environment_id;
        let project = projects::Entity::find_by_id(project_id)
            .lock_shared()
            .one(transaction)
            .await?
            .ok_or_else(|| DnsError::DeliveryProjectNotFound {
                project_id,
                hostname: request.hostname.clone(),
            })?;
        if project.is_deleted {
            return Err(DnsError::DeliveryProjectBeingDeleted {
                project_id,
                hostname: request.hostname.clone(),
            });
        }
        let environment = environments::Entity::find_by_id(environment_id)
            .lock_shared()
            .one(transaction)
            .await?
            .filter(|environment| environment.project_id == project_id)
            .ok_or_else(|| DnsError::DeliveryEnvironmentNotFound {
                project_id,
                environment_id,
                hostname: request.hostname.clone(),
            })?;
        if environment.deleted_at.is_some() {
            return Err(DnsError::DeliveryEnvironmentDeleted {
                project_id,
                environment_id,
                hostname: request.hostname.clone(),
            });
        }
        Ok(())
    }

    /// Share-lock the DNS provider row, then the managed zone row, that the
    /// binding being reserved will be written through, and refuse unless the
    /// provider is still active and the zone still verified and auto-managed.
    ///
    /// Bindings have no foreign key to the zone. Removing a zone, turning off
    /// its auto-management, and deactivating or deleting a provider lock the
    /// same rows `FOR UPDATE` before counting bindings, so either they wait
    /// for this reservation and then see its binding, or this waits for them
    /// and sees their change.
    async fn lock_delivery_zone(
        transaction: &DatabaseTransaction,
        request: &PreviewDomainDeliveryBindingRequest,
    ) -> Result<(), DnsError> {
        let unavailable = |reason: String| DnsError::DeliveryZoneUnavailable {
            hostname: request.hostname.clone(),
            zone: request.zone.clone(),
            provider_id: request.dns_provider_id,
            reason,
        };
        let provider = dns_providers::Entity::find_by_id(request.dns_provider_id)
            .lock_shared()
            .one(transaction)
            .await?
            .ok_or_else(|| unavailable("the DNS provider was deleted after preview".into()))?;
        if !provider.is_active {
            return Err(unavailable(format!(
                "DNS provider '{}' was deactivated after preview",
                provider.name
            )));
        }
        // `request.zone` was normalized (trimmed, lowercase, no trailing dot)
        // by preview, and is the exact form apply matched the zone by.
        let zone = dns_managed_domains::Entity::find()
            .filter(dns_managed_domains::Column::ProviderId.eq(request.dns_provider_id))
            .filter(dns_managed_domains::Column::Domain.eq(&request.zone))
            .lock_shared()
            .one(transaction)
            .await?
            .ok_or_else(|| {
                unavailable(
                    "the zone was removed from the provider's managed domains after preview".into(),
                )
            })?;
        if !zone.verified {
            return Err(unavailable(format!(
                "managed domain {} is no longer verified",
                zone.id
            )));
        }
        if !zone.auto_manage {
            return Err(unavailable(format!(
                "managed domain {} is no longer auto-managed",
                zone.id
            )));
        }
        Ok(())
    }

    /// Whether a locked custom domain row still routes `request.hostname` to
    /// this project and environment.
    fn route_unchanged(
        route: &project_custom_domains::Model,
        project_id: i32,
        request: &PreviewDomainDeliveryBindingRequest,
    ) -> bool {
        route
            .domain
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase()
            == request.hostname
            && route.project_id == project_id
            && route.environment_id == request.environment_id
    }

    fn route_moved_error(
        request: &PreviewDomainDeliveryBindingRequest,
        custom_domain_id: i32,
        current: Option<&project_custom_domains::Model>,
    ) -> DnsError {
        let reason = match current {
            Some(route) => format!(
                "custom domain {custom_domain_id} changed after preview and now routes '{}' to project {}, environment {}; create a new preview",
                route.domain, route.project_id, route.environment_id
            ),
            None => format!(
                "custom domain {custom_domain_id} was deleted after preview; create a new preview"
            ),
        };
        DnsError::RecordConflict {
            domain: request.zone.clone(),
            name: request.hostname.clone(),
            record_type: "ROUTE".into(),
            reason,
        }
    }

    /// Whether the live record is the one a failed attempt of this preview
    /// wrote: a receipt exists, the live record is equivalent to it, and this
    /// install owns the live record for domain delivery in exactly this
    /// project and environment.
    fn receipt_covers_live_record(
        receipt: Option<&DeliveryDnsWriteReceipt>,
        live: &RecordOwnership,
        project_id: i32,
        environment_id: i32,
    ) -> Result<bool, DnsError> {
        let (Some(receipt), RecordOwnership::Owned(record, marker)) = (receipt, live) else {
            return Ok(false);
        };
        if marker.controller.as_deref() != Some(DELIVERY_CONTROLLER)
            || marker.project_id != Some(project_id)
            || marker.environment_id != Some(environment_id)
        {
            return Ok(false);
        }
        Self::same_record(Some(record), Some(&receipt.record))
    }

    /// Remember on the preview the record this attempt wrote and read back,
    /// so a retry after a later step fails can resume from it. Losing the
    /// receipt only costs that resumption (the retry is refused and needs a
    /// new preview), so a failure to save it is logged rather than returned.
    async fn save_dns_write_receipt(
        &self,
        preview_id: Uuid,
        plan: &DomainDeliveryPreviewResponse,
        written: &DnsRecord,
    ) {
        let stored = StoredDeliveryPlan {
            plan: plan.clone(),
            dns_write_receipt: Some(DeliveryDnsWriteReceipt {
                record: written.clone(),
                written_at: Utc::now(),
            }),
        };
        let saved = match serde_json::to_value(&stored) {
            Ok(value) => domain_delivery_previews::Entity::update_many()
                .col_expr(domain_delivery_previews::Column::Plan, Expr::value(value))
                .filter(domain_delivery_previews::Column::Id.eq(preview_id))
                .exec(self.db.as_ref())
                .await
                .map(|_| ())
                .map_err(DnsError::from),
            Err(error) => Err(DnsError::from(error)),
        };
        if let Err(error) = saved {
            tracing::error!(
                %preview_id,
                %error,
                "Failed to save the DNS write receipt of delivery preview {preview_id}; a retry after a later failure will need a new preview"
            );
        }
    }

    /// Remember on `binding` that Temps added its hostname to the Bunny Pull
    /// Zone, right after the add succeeded. Best effort: the apply's final
    /// binding write records it again whether the apply succeeds or fails, so
    /// a failure here is logged. If every write is lost, cleanup later treats
    /// the hostname as preexisting and leaves it attached, never the reverse.
    async fn save_bunny_hostname_owned(&self, binding: &domain_delivery_bindings::Model) {
        let saved = domain_delivery_bindings::Entity::update_many()
            .col_expr(
                domain_delivery_bindings::Column::BunnyHostnameOwned,
                Expr::value(true),
            )
            .filter(domain_delivery_bindings::Column::Id.eq(binding.id))
            .exec(self.db.as_ref())
            .await;
        if let Err(error) = saved {
            tracing::error!(
                binding_id = binding.id,
                project_id = binding.project_id,
                hostname = %binding.hostname,
                %error,
                "Failed to record that Temps attached Bunny hostname {} for delivery binding {}; the apply's final write retries it",
                binding.hostname,
                binding.id
            );
        }
    }

    /// Mark a binding applied (`active` once its route is active, otherwise
    /// `dns_configured`) together with the preview that applied it, and save
    /// whether Temps owns its Bunny hostname.
    async fn activate_binding(
        &self,
        binding: &domain_delivery_bindings::Model,
        preview: &domain_delivery_previews::Model,
        route_active: bool,
        bunny_hostname_owned: bool,
    ) -> Result<domain_delivery_bindings::Model, DnsError> {
        let now = Utc::now();
        let transaction = self.db.begin().await?;
        let mut active_binding: domain_delivery_bindings::ActiveModel = binding.clone().into();
        active_binding.status = Set(if route_active {
            "active".into()
        } else {
            "dns_configured".into()
        });
        active_binding.last_error = Set(None);
        active_binding.updated_at = Set(now);
        active_binding.applied_at = Set(Some(now));
        active_binding.bunny_hostname_owned = Set(bunny_hostname_owned);
        let binding = active_binding.update(&transaction).await?;
        let mut applied: domain_delivery_previews::ActiveModel = preview.clone().into();
        applied.status = Set("applied".into());
        applied.last_error = Set(None);
        applied.applied_at = Set(Some(now));
        applied.update(&transaction).await?;
        transaction.commit().await?;
        Ok(binding)
    }

    /// Record a failed apply on its binding and preview so the same preview
    /// can be applied again, keeping whether Temps owns the binding's Bunny
    /// hostname so the retry and any cleanup know it. Best effort: the caller
    /// returns the original failure, so a database error here is logged
    /// instead of replacing it.
    async fn mark_apply_failed(
        &self,
        binding: &domain_delivery_bindings::Model,
        preview: &domain_delivery_previews::Model,
        message: &str,
        bunny_hostname_owned: bool,
    ) {
        let mut failed: domain_delivery_bindings::ActiveModel = binding.clone().into();
        failed.status = Set("failed".into());
        failed.last_error = Set(Some(message.to_string()));
        failed.updated_at = Set(Utc::now());
        failed.bunny_hostname_owned = Set(bunny_hostname_owned);
        if let Err(error) = failed.update(self.db.as_ref()).await {
            tracing::error!(
                binding_id = binding.id,
                hostname = %binding.hostname,
                %error,
                "Failed to mark domain delivery binding {} as failed after an apply error",
                binding.id
            );
        }
        self.mark_preview_failed(preview, message).await;
    }

    async fn mark_preview_failed(&self, preview: &domain_delivery_previews::Model, message: &str) {
        let mut failed: domain_delivery_previews::ActiveModel = preview.clone().into();
        failed.status = Set("failed".into());
        failed.last_error = Set(Some(message.to_string()));
        if let Err(error) = failed.update(self.db.as_ref()).await {
            tracing::error!(
                preview_id = %preview.id,
                project_id = preview.project_id,
                %error,
                "Failed to mark domain delivery preview {} as failed",
                preview.id
            );
        }
    }
    async fn binding_response(
        &self,
        v: domain_delivery_bindings::Model,
    ) -> Result<DomainDeliveryBindingResponse, DnsError> {
        let p = self.profile(v.profile_id).await?;
        Self::binding_response_with(v, &p)
    }
    fn binding_response_with(
        v: domain_delivery_bindings::Model,
        p: &delivery_profiles::Model,
    ) -> Result<DomainDeliveryBindingResponse, DnsError> {
        let provider_kind = DeliveryProviderKind::parse(&p.provider_kind)?;
        Ok(DomainDeliveryBindingResponse {
            id: v.id,
            hostname: v.hostname,
            project_id: v.project_id,
            environment_id: v.environment_id,
            custom_domain_id: v.custom_domain_id,
            delivery_profile_id: v.profile_id,
            delivery_profile_name: p.name.clone(),
            profile_source: v.profile_source,
            provider_kind,
            // Only a Bunny binding's cleanup touches a Pull Zone.
            bunny_hostname_owned: provider_kind == DeliveryProviderKind::Bunny
                && v.bunny_hostname_owned,
            dns_provider_id: v.dns_provider_id,
            zone: v.zone,
            origin_target: v.origin_target,
            record_type: match v.record_type.as_str() {
                "A" => DnsRecordType::A,
                "AAAA" => DnsRecordType::AAAA,
                "CNAME" => DnsRecordType::CNAME,
                _ => {
                    return Err(DnsError::Validation(format!(
                        "Binding {} has invalid record type '{}'",
                        v.id, v.record_type
                    )))
                }
            },
            proxied: v.proxied,
            status: v.status,
            last_error: v.last_error,
            applied_at: v.applied_at,
            created_at: v.created_at,
            updated_at: v.updated_at,
        })
    }
    /// One page of a project's domain delivery bindings, newest first unless
    /// `sort_by` (`created_at`, `hostname`, `updated_at`) or `sort_order`
    /// (`asc`, `desc`) say otherwise. Binding ID breaks ties in the same
    /// direction, so rows sharing a sort value are never repeated or skipped
    /// across pages.
    pub async fn list_bindings(
        &self,
        project_id: i32,
        params: PaginationParams,
    ) -> Result<DomainDeliveryBindingPage, DnsError> {
        let sort_by = BindingSortBy::parse(params.sort_by.as_deref())?;
        let direction =
            SortDirection::parse(params.sort_order.as_deref(), "domain delivery bindings")?;
        let (page, page_size) = params.normalize();
        self.require_project(project_id).await?;
        let paginator = domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::ProjectId.eq(project_id))
            .order_by(sort_by.column(), direction.order())
            .order_by(domain_delivery_bindings::Column::Id, direction.order())
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let bindings = if page_is_past_end(page, page_size, total) {
            Vec::new()
        } else {
            paginator.fetch_page(page - 1).await?
        };
        // Every profile this page references, in one query rather than one
        // per binding.
        let profile_ids: Vec<i32> = bindings
            .iter()
            .map(|binding| binding.profile_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let profiles: std::collections::HashMap<i32, delivery_profiles::Model> =
            if profile_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                delivery_profiles::Entity::find()
                    .filter(delivery_profiles::Column::Id.is_in(profile_ids))
                    .all(self.db.as_ref())
                    .await?
                    .into_iter()
                    .map(|profile| (profile.id, profile))
                    .collect()
            };
        let items = bindings
            .into_iter()
            .map(|binding| {
                let profile = profiles.get(&binding.profile_id).ok_or_else(|| {
                    DnsError::DomainNotFound(format!(
                        "delivery profile {} for binding {}",
                        binding.profile_id, binding.id
                    ))
                })?;
                Self::binding_response_with(binding, profile)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DomainDeliveryBindingPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// Detach a delivered hostname from a Bunny profile's Pull Zone. No-op
    /// for hostnames already absent from the zone, so a retried cleanup
    /// converges.
    async fn remove_bunny_hostname(
        &self,
        profile: &delivery_profiles::Model,
        hostname: &str,
    ) -> Result<(), DnsError> {
        let (zone_id, key) = self.bunny_credentials(profile)?;
        let zone = self.bunny.get_zone(zone_id, &key).await?;
        let attached = zone
            .hostnames
            .iter()
            .any(|entry| !entry.is_system_hostname && entry.value.eq_ignore_ascii_case(hostname));
        if attached {
            self.bunny.remove_hostname(zone_id, hostname, &key).await?;
        }
        Ok(())
    }

    /// Remove a binding's DNS record, detach its hostname from Bunny when
    /// Temps added it there, and delete the binding. Returns the binding as
    /// it was before deletion; its `bunny_hostname_owned` says whether the
    /// Bunny hostname was detached (`true`) or, having been on the Pull Zone
    /// before Temps set up delivery, left in place with its edge certificate.
    ///
    /// The provider and zone come from the binding itself, so cleanup keeps
    /// working after the zone stops being verified or auto-managed.
    pub async fn delete_binding(
        &self,
        project_id: i32,
        binding_id: i32,
    ) -> Result<DomainDeliveryBindingResponse, DnsError> {
        self.require_project(project_id).await?;
        let binding = domain_delivery_bindings::Entity::find_by_id(binding_id)
            .one(self.db.as_ref())
            .await?
            .filter(|binding| binding.project_id == project_id)
            .ok_or_else(|| {
                DnsError::DomainNotFound(format!(
                    "delivery binding {binding_id} for project {project_id}"
                ))
            })?;
        let delivery_lock = self.acquire_delivery_lock(&binding.hostname).await?;
        let binding = domain_delivery_bindings::Entity::find_by_id(binding_id)
            .one(self.db.as_ref())
            .await?
            .filter(|current| current.project_id == project_id)
            .ok_or_else(|| {
                DnsError::DomainNotFound(format!(
                    "delivery binding {binding_id} changed while cleanup was waiting for the hostname lock"
                ))
            })?;
        let record_type = match binding.record_type.as_str() {
            "A" => DnsRecordType::A,
            "AAAA" => DnsRecordType::AAAA,
            "CNAME" => DnsRecordType::CNAME,
            _ => {
                return Err(DnsError::Validation(format!(
                    "Binding {binding_id} has invalid record type '{}'",
                    binding.record_type
                )))
            }
        };
        let profile = self.profile(binding.profile_id).await?;
        let deleted = Self::binding_response_with(binding.clone(), &profile)?;
        let (record_name, _, _) =
            Self::record(&binding.zone, &binding.hostname, &binding.origin_target)?;
        self.managed
            .lock_record_with_transaction(&binding.zone, &record_name, &delivery_lock)
            .await?;
        let ownership = self
            .managed
            .record_ownership_for_provider(
                binding.dns_provider_id,
                &binding.zone,
                &record_name,
                record_type,
            )
            .await?;
        Self::validate_owned_scope(
            &ownership,
            project_id,
            binding.environment_id,
            &binding.zone,
            &record_name,
            record_type,
        )?;
        let mut progress = DeliveryProgress {
            operation: DeliveryOperation::Cleanup,
            project_id,
            environment_id: binding.environment_id,
            hostname: binding.hostname.clone(),
            preview_id: None,
            binding_id: Some(binding_id),
            completed: Vec::new(),
        };
        let cleanup: Result<(), (DeliveryStep, DnsError)> = async {
            self.managed
                .remove_record_with_transaction(
                    binding.dns_provider_id,
                    &binding.zone,
                    &record_name,
                    record_type,
                    Self::binding_scope(&binding),
                    &delivery_lock,
                )
                .await
                .map_err(at_step(DeliveryStep::DnsRecordRemoved))?;
            progress.complete(DeliveryStep::DnsRecordRemoved);
            // DNS goes first so traffic stops reaching the Pull Zone before
            // the hostname (and its edge certificate) is detached from it.
            // Only a hostname Temps added is detached: one that was already
            // on the Pull Zone belongs to whoever attached it.
            if profile.provider_kind == DeliveryProviderKind::Bunny.as_str() {
                if binding.bunny_hostname_owned {
                    self.remove_bunny_hostname(&profile, &binding.hostname)
                        .await
                        .map_err(at_step(DeliveryStep::BunnyHostnameRemoved))?;
                    progress.complete(DeliveryStep::BunnyHostnameRemoved);
                } else {
                    tracing::info!(
                        binding_id,
                        project_id,
                        profile_id = profile.id,
                        hostname = %binding.hostname,
                        "Leaving Bunny hostname {} on the Pull Zone of delivery profile {}: it was attached before Temps set up delivery",
                        binding.hostname,
                        profile.id
                    );
                }
            }
            domain_delivery_bindings::Entity::delete_by_id(binding_id)
                .exec(self.db.as_ref())
                .await
                .map_err(|error| (DeliveryStep::BindingDeleted, DnsError::from(error)))?;
            Ok(())
        }
        .await;
        if let Err((failed_step, error)) = cleanup {
            let failure = progress.failure(failed_step, error);
            let mut failed: domain_delivery_bindings::ActiveModel = binding.into();
            failed.status = Set("cleanup_failed".into());
            failed.last_error = Set(Some(failure.to_string()));
            failed.updated_at = Set(Utc::now());
            // Best effort: the cleanup failure is what the caller needs.
            if let Err(error) = failed.update(self.db.as_ref()).await {
                tracing::error!(
                    binding_id,
                    project_id,
                    %error,
                    "Failed to mark domain delivery binding {binding_id} as cleanup_failed"
                );
            }
            return Err(failure);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod apply_resume_and_progress_tests {
    use super::*;
    use crate::ownership::OwnershipMarker;

    fn record(address: &str) -> DnsRecord {
        DnsRecord {
            id: Some("rec-1".into()),
            zone: "example.com".into(),
            name: "app".into(),
            fqdn: "app.example.com".into(),
            content: DnsRecordContent::A {
                address: address.into(),
            },
            ttl: 300,
            proxied: false,
            metadata: Default::default(),
        }
    }

    fn marker(project_id: i32, environment_id: i32, controller: Option<&str>) -> OwnershipMarker {
        OwnershipMarker::new_signed(
            &[7; 32],
            "instance",
            "example.com",
            "app",
            DnsRecordType::A,
            "fingerprint",
            Some(project_id),
            Some(environment_id),
            controller,
        )
        .expect("marker")
    }

    fn receipt(address: &str) -> DeliveryDnsWriteReceipt {
        DeliveryDnsWriteReceipt {
            record: record(address),
            written_at: Utc::now(),
        }
    }

    fn request() -> PreviewDomainDeliveryBindingRequest {
        PreviewDomainDeliveryBindingRequest {
            hostname: "app.example.com".into(),
            environment_id: 10,
            dns_provider_id: 3,
            zone: "example.com".into(),
            origin_target: "192.0.2.1".into(),
            delivery_profile_id: None,
        }
    }

    fn plan() -> DomainDeliveryPreviewResponse {
        DomainDeliveryPreviewResponse {
            preview_id: Uuid::new_v4(),
            expires_at: Utc::now(),
            profile_id: 2,
            profile_source: "project".into(),
            provider_kind: DeliveryProviderKind::Direct,
            origin_tls: OriginTlsPolicy::ExistingCertificate,
            record: DeliveryRecordPlan {
                name: "app".into(),
                record_type: DnsRecordType::A,
                value: "192.0.2.1".into(),
                proxied: false,
                ownership_status: "unmanaged".into(),
                requires_adoption: true,
                expected_existing_record: Some(record("198.51.100.7")),
            },
            routing: DeliveryRoutingPlan {
                will_create_custom_domain: true,
                custom_domain_id: None,
            },
            warnings: vec![],
        }
    }

    #[test]
    fn receipt_resumes_only_temps_own_record_in_the_same_scope() {
        let written = receipt("203.0.113.10");
        let own = RecordOwnership::Owned(
            record("203.0.113.10"),
            marker(1, 10, Some(DELIVERY_CONTROLLER)),
        );
        assert!(
            DomainDeliveryService::receipt_covers_live_record(Some(&written), &own, 1, 10)
                .expect("comparable")
        );
        // No receipt: no attempt of this preview wrote anything.
        assert!(
            !DomainDeliveryService::receipt_covers_live_record(None, &own, 1, 10)
                .expect("comparable")
        );
        // The record changed after Temps wrote it.
        let changed = RecordOwnership::Owned(
            record("203.0.113.99"),
            marker(1, 10, Some(DELIVERY_CONTROLLER)),
        );
        assert!(!DomainDeliveryService::receipt_covers_live_record(
            Some(&written),
            &changed,
            1,
            10
        )
        .expect("comparable"));
        // Same value, but no longer under this install's ownership marker.
        let unmanaged = RecordOwnership::Unmanaged(record("203.0.113.10"));
        assert!(!DomainDeliveryService::receipt_covers_live_record(
            Some(&written),
            &unmanaged,
            1,
            10
        )
        .expect("comparable"));
        // Owned by another environment, project, or workflow.
        for other in [
            marker(1, 11, Some(DELIVERY_CONTROLLER)),
            marker(2, 10, Some(DELIVERY_CONTROLLER)),
            marker(1, 10, None),
        ] {
            let ownership = RecordOwnership::Owned(record("203.0.113.10"), other);
            assert!(!DomainDeliveryService::receipt_covers_live_record(
                Some(&written),
                &ownership,
                1,
                10
            )
            .expect("comparable"));
        }
    }

    #[test]
    fn stored_plan_reads_rows_without_a_receipt_and_round_trips_one() {
        let plan = plan();
        // What `preview` stores: the API response, with no receipt.
        let stored: StoredDeliveryPlan =
            serde_json::from_value(serde_json::to_value(&plan).expect("plan json"))
                .expect("a plan row without a receipt");
        assert!(stored.dns_write_receipt.is_none());
        assert_eq!(stored.plan.preview_id, plan.preview_id);

        let with_receipt = StoredDeliveryPlan {
            plan: plan.clone(),
            dns_write_receipt: Some(receipt("203.0.113.10")),
        };
        let row = serde_json::to_value(&with_receipt).expect("stored plan json");
        // The receipt never changes how the row reads as the API plan.
        let api_view: DomainDeliveryPreviewResponse =
            serde_json::from_value(row.clone()).expect("plan view of a row with a receipt");
        assert_eq!(api_view.preview_id, plan.preview_id);
        assert_eq!(api_view.record.name, "app");
        let round_trip: StoredDeliveryPlan =
            serde_json::from_value(row).expect("stored plan with a receipt");
        let saved = round_trip.dns_write_receipt.expect("receipt kept");
        assert!(DomainDeliveryService::same_record(
            Some(&saved.record),
            Some(&record("203.0.113.10"))
        )
        .expect("comparable"));
        assert!(DomainDeliveryService::same_record(
            round_trip.plan.record.expected_existing_record.as_ref(),
            Some(&record("198.51.100.7"))
        )
        .expect("comparable"));
    }

    #[test]
    fn progress_reports_incomplete_only_after_something_changed() {
        let mut progress = DeliveryProgress {
            operation: DeliveryOperation::Apply,
            project_id: 1,
            environment_id: 10,
            hostname: "app.example.com".into(),
            preview_id: Some(Uuid::nil()),
            binding_id: Some(5),
            completed: Vec::new(),
        };
        let untouched = progress.failure(
            DeliveryStep::BindingReserved,
            DnsError::Validation("invalid".into()),
        );
        assert!(matches!(untouched, DnsError::Validation(_)), "{untouched}");

        progress.complete(DeliveryStep::BindingReserved);
        progress.complete(DeliveryStep::DnsRecordWritten);
        let error = progress.failure(
            DeliveryStep::CertificateRequested,
            DnsError::ApiError(
                "Bunny API request to /pullzone/loadFreeCertificate failed (HTTP 500)".into(),
            ),
        );
        let DnsError::DeliveryIncomplete(incomplete) = &error else {
            panic!("expected DeliveryIncomplete, got {error}");
        };
        assert_eq!(
            incomplete.completed_steps,
            vec![
                DeliveryStep::BindingReserved,
                DeliveryStep::DnsRecordWritten
            ]
        );
        assert_eq!(incomplete.failed_step, DeliveryStep::CertificateRequested);
        assert!(matches!(incomplete.source, DnsError::ApiError(_)));
        let message = error.to_string();
        assert!(message.contains("'app.example.com'"), "{message}");
        assert!(message.contains("binding 5"), "{message}");
        assert!(message.contains("'certificate_requested'"), "{message}");
        assert!(
            message.contains("[binding_reserved, dns_record_written]"),
            "{message}"
        );
        assert!(
            message.contains("apply the same preview again"),
            "{message}"
        );
        assert!(message.contains("HTTP 500"), "{message}");
    }

    #[test]
    fn reserved_route_must_still_be_the_previewed_hostname_project_and_environment() {
        let now = Utc::now();
        let route = project_custom_domains::Model {
            id: 4,
            project_id: 1,
            environment_id: 10,
            domain: " App.Example.com. ".into(),
            redirect_to: None,
            status_code: None,
            branch: None,
            status: "pending".into(),
            message: None,
            created_at: now,
            updated_at: now,
            certificate_id: None,
            service_name: None,
        };
        let request = request();
        assert!(DomainDeliveryService::route_unchanged(&route, 1, &request));

        let mut renamed = route.clone();
        renamed.domain = "other.example.com".into();
        let mut moved_environment = route.clone();
        moved_environment.environment_id = 11;
        let mut moved_project = route.clone();
        moved_project.project_id = 2;
        for changed in [&renamed, &moved_environment, &moved_project] {
            assert!(!DomainDeliveryService::route_unchanged(
                changed, 1, &request
            ));
        }

        let error = DomainDeliveryService::route_moved_error(&request, 4, Some(&renamed));
        assert!(
            matches!(&error, DnsError::RecordConflict { record_type, reason, .. }
                if record_type == "ROUTE"
                    && reason.contains("custom domain 4")
                    && reason.contains("other.example.com")),
            "{error}"
        );
        let deleted = DomainDeliveryService::route_moved_error(&request, 4, None);
        assert!(
            matches!(&deleted, DnsError::RecordConflict { reason, .. }
                if reason.contains("custom domain 4 was deleted")),
            "{deleted}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::OwnershipMarker;
    use wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    fn test_bunny_zone() -> BunnyZone {
        BunnyZone {
            id: 42,
            name: "temps-edge".into(),
            origin_url: Some("https://edge.example.net".into()),
            enabled: true,
            suspended: false,
            add_host_header: true,
            hostnames: vec![BunnyHostname {
                value: "temps-edge.b-cdn.net".into(),
                is_system_hostname: true,
                has_certificate: false,
            }],
        }
    }

    #[test]
    fn bunny_requires_an_active_zone_with_host_forwarding_and_matching_origin() {
        let mut zone = test_bunny_zone();
        assert_eq!(
            zone.validate(42).expect("valid zone"),
            "temps-edge.b-cdn.net"
        );
        assert!(zone
            .validate_origin("edge.example.net", "app.example.com")
            .is_ok());
        assert!(zone
            .validate_origin("other.example.net", "app.example.com")
            .is_err());
        assert!(zone
            .validate_origin("edge.example.net", "edge.example.net")
            .is_err());
        zone.add_host_header = false;
        assert!(zone.validate(42).is_err());
        zone.add_host_header = true;
        zone.suspended = true;
        assert!(zone.validate(42).is_err());
    }

    #[test]
    fn bunny_delivery_uses_a_cname_and_rejects_zone_apex() {
        let adapter = BunnyAdapter {
            hostname: "temps-edge.b-cdn.net".into(),
        };
        let plan = adapter
            .plan("app.example.com", "example.com", "edge.example.net")
            .expect("valid subdomain");
        assert_eq!(plan.record.record_type, DnsRecordType::CNAME);
        assert_eq!(plan.record.value, "temps-edge.b-cdn.net");
        assert!(!plan.record.proxied);
        assert!(adapter
            .plan("example.com", "example.com", "edge.example.net")
            .is_err());
    }

    #[test]
    fn bunny_profile_response_never_contains_encrypted_api_key() {
        let now = Utc::now();
        let profile = delivery_profiles::Model {
            id: 42,
            name: "Bunny".into(),
            provider_kind: "bunny".into(),
            bunny_pull_zone_id: Some(42),
            bunny_hostname: Some("temps-edge.b-cdn.net".into()),
            bunny_api_key_encrypted: Some("secret-ciphertext".into()),
            created_at: now,
            updated_at: now,
        };
        let response: DeliveryProfileResponse = profile.try_into().expect("valid profile");
        let json = serde_json::to_string(&response).expect("serializable response");
        assert!(json.contains("temps-edge.b-cdn.net"));
        assert!(!json.contains("secret-ciphertext"));
        assert!(!json.contains("bunny_api_key_encrypted"));
    }

    #[tokio::test]
    async fn bunny_api_authenticates_and_registers_hostname_and_certificate() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/pullzone/42")).and(header("AccessKey", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id":42,"Name":"temps-edge","OriginUrl":"https://edge.example.net","Enabled":true,"Suspended":false,"AddHostHeader":true,
                "Hostnames":[{"Value":"temps-edge.b-cdn.net","IsSystemHostname":true,"HasCertificate":false}]
            }))).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/pullzone/42/addHostname"))
            .and(header("AccessKey", "test-key"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/pullzone/loadFreeCertificate"))
            .and(query_param("hostname", "app.example.com"))
            .and(header("AccessKey", "test-key"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/pullzone/43"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"Message":"API key TEST-KEY (test-key) is unauthorized; upstream-detail-marker"}),
            ))
            .mount(&server)
            .await;
        let api = BunnyApi::with_base_url(server.uri());
        let zone = api.get_zone(42, "test-key").await.expect("zone response");
        assert_eq!(
            zone.validate(42).expect("valid zone"),
            "temps-edge.b-cdn.net"
        );
        api.add_hostname(42, "app.example.com", "test-key")
            .await
            .expect("hostname added");
        api.load_certificate("app.example.com", "test-key")
            .await
            .expect("certificate requested");
        assert!(api.get_zone(42, "wrong-key").await.is_err());
        assert!(matches!(
            api.get_zone(43, "wrong-key").await,
            Err(DnsError::InvalidCredentials(_))
        ));
        let error = api
            .get_zone(43, "test-key")
            .await
            .expect_err("Bunny rejects the key");
        let message = error.to_string();
        // Neither the key (in any casing the upstream chose to echo) nor any
        // part of the upstream body may reach the error text.
        assert!(
            !message.to_ascii_lowercase().contains("test-key"),
            "{message}"
        );
        assert!(!message.contains("upstream-detail-marker"), "{message}");
        assert!(message.contains("/pullzone/43"), "{message}");
        assert!(message.contains("HTTP 401"), "{message}");
    }

    #[tokio::test]
    async fn bunny_api_errors_never_include_upstream_bodies() {
        let server = MockServer::start().await;
        for (status, zone) in [(400, 50), (404, 51), (429, 52), (500, 53)] {
            Mock::given(method("GET"))
                .and(path(format!("/pullzone/{zone}")))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"Message":"upstream-detail-marker"})),
                )
                .mount(&server)
                .await;
        }
        let api = BunnyApi::with_base_url(server.uri());
        for (status, zone) in [(400, 50), (404, 51), (429, 52), (500, 53)] {
            let error = api
                .get_zone(zone, "test-key")
                .await
                .expect_err("non-success status must fail");
            let message = error.to_string();
            assert!(!message.contains("upstream-detail-marker"), "{message}");
            assert!(message.contains(&format!("HTTP {status}")), "{message}");
            match status {
                400 | 404 => assert!(matches!(error, DnsError::Validation(_))),
                429 => assert!(matches!(error, DnsError::RateLimited(_))),
                _ => assert!(matches!(error, DnsError::ApiError(_))),
            }
        }
    }

    #[tokio::test]
    async fn bunny_api_refuses_oversized_response_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pullzone/42"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("x".repeat(BUNNY_MAX_RESPONSE_BYTES + 1)),
            )
            .mount(&server)
            .await;
        let api = BunnyApi::with_base_url(server.uri());
        let error = api
            .get_zone(42, "test-key")
            .await
            .expect_err("oversized body must be refused");
        assert!(matches!(error, DnsError::ApiError(_)), "{error}");
        assert!(error.to_string().contains("exceeded"), "{error}");
    }

    #[test]
    fn profile_view_without_provider_details_keeps_only_identity_and_kind() {
        let now = Utc::now();
        let full = DeliveryProfileResponse {
            id: 7,
            name: "Edge".into(),
            provider_kind: DeliveryProviderKind::Bunny,
            bunny_pull_zone_id: Some(42),
            bunny_hostname: Some("temps-edge.b-cdn.net".into()),
            created_at: now,
            updated_at: now,
        };
        let settings = ProjectDeliverySettingsResponse {
            project_id: 1,
            default_profile_id: Some(7),
            environment_overrides: vec![],
            effective_default_profile: Some(full.clone()),
        }
        .without_provider_details();
        let reduced = full.without_provider_details();
        assert_eq!((reduced.id, reduced.name.as_str()), (7, "Edge"));
        assert_eq!(reduced.provider_kind, DeliveryProviderKind::Bunny);
        assert!(reduced.bunny_pull_zone_id.is_none());
        assert!(reduced.bunny_hostname.is_none());
        let json = serde_json::to_string(&settings).expect("serializable settings");
        assert!(!json.contains("b-cdn.net"), "{json}");
        assert!(json.contains("\"bunny_pull_zone_id\":null"), "{json}");
    }

    #[test]
    fn record_comparison_is_exact() {
        let record = DnsRecord {
            id: Some("rec-1".into()),
            zone: "example.com".into(),
            name: "app".into(),
            fqdn: "app.example.com".into(),
            content: DnsRecordContent::A {
                address: "192.0.2.1".into(),
            },
            ttl: 300,
            proxied: false,
            metadata: Default::default(),
        };
        let mut changed = record.clone();
        changed.content = DnsRecordContent::A {
            address: "203.0.113.9".into(),
        };
        assert!(DomainDeliveryService::same_record(None, None).expect("comparable"));
        assert!(
            DomainDeliveryService::same_record(Some(&record), Some(&record.clone()))
                .expect("comparable")
        );
        assert!(
            !DomainDeliveryService::same_record(Some(&record), Some(&changed)).expect("comparable")
        );
        assert!(!DomainDeliveryService::same_record(Some(&record), None).expect("comparable"));
    }
    #[tokio::test]
    async fn bunny_api_removes_hostname_with_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/pullzone/42/removeHostname"))
            .and(header("AccessKey", "test-key"))
            .and(wiremock::matchers::body_json(
                serde_json::json!({"Hostname": "app.example.com"}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let api = BunnyApi::with_base_url(server.uri());
        api.remove_hostname(42, "app.example.com", "test-key")
            .await
            .expect("hostname removed");
    }
    #[tokio::test]
    async fn bunny_api_does_not_follow_redirects_with_the_access_key() {
        let server = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pullzone/42"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/steal", elsewhere.uri()).as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/steal"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&elsewhere)
            .await;
        let api = BunnyApi::with_base_url(server.uri());
        assert!(api.get_zone(42, "test-key").await.is_err());
    }
    #[test]
    fn future_project_default_keeps_its_last_profile() {
        assert!(last_profile_needed_for_new_projects(
            "cloudflare",
            Some("cloudflare"),
            false
        ));
        assert!(last_profile_needed_for_new_projects(
            "bunny",
            Some("bunny"),
            false
        ));
        assert!(!last_profile_needed_for_new_projects(
            "cloudflare",
            None,
            false
        ));
        assert!(!last_profile_needed_for_new_projects(
            "cloudflare",
            Some("cloudflare"),
            true
        ));
        assert!(!last_profile_needed_for_new_projects(
            "direct",
            Some("bunny"),
            false
        ));
    }
    #[test]
    fn target_shapes_choose_safe_record_types() {
        assert_eq!(
            DomainDeliveryService::record("example.com", "app.example.com", "192.0.2.1")
                .expect("valid")
                .1,
            DnsRecordType::A
        );
        assert_eq!(
            DomainDeliveryService::record("example.com", "app.example.com", "edge.example.net")
                .expect("valid")
                .1,
            DnsRecordType::CNAME
        );
    }
    #[test]
    fn hostname_must_be_in_zone() {
        assert!(
            DomainDeliveryService::record("example.com", "app.attacker.test", "192.0.2.1").is_err()
        );
    }

    #[test]
    fn hostname_validation_rejects_wildcards_reserved_names_and_bad_labels() {
        for hostname in [
            "*.example.com",
            "_temps-owned-a.app.example.com",
            "-app.example.com",
            "app_.example.com",
        ] {
            assert!(
                DomainDeliveryService::normalized_hostname(hostname).is_err(),
                "{hostname} must be rejected"
            );
        }
        assert_eq!(
            DomainDeliveryService::normalized_hostname("App.Example.COM.").expect("valid hostname"),
            "app.example.com"
        );
    }

    #[test]
    fn adapters_produce_provider_neutral_requirements() {
        let direct = DirectAdapter
            .plan("app.example.com", "example.com", "192.0.2.1")
            .expect("direct plan");
        assert!(!direct.record.proxied);
        assert_eq!(direct.record.ttl, Some(300));
        let cloudflare = CloudflareAdapter
            .plan("app.example.com", "example.com", "origin.example.net")
            .expect("cloudflare plan");
        assert!(cloudflare.record.proxied);
        assert_eq!(cloudflare.record.ttl, None);
        assert!(matches!(
            cloudflare.origin_tls,
            OriginTlsPolicy::ExistingCertificate
        ));
    }

    #[test]
    fn same_install_marker_cannot_cross_project_scope() {
        let marker = OwnershipMarker::new_signed(
            &[7; 32],
            "instance",
            "example.com",
            "app",
            DnsRecordType::A,
            "fingerprint",
            Some(2),
            Some(20),
            Some("domain-delivery"),
        )
        .expect("marker");
        let ownership = RecordOwnership::Orphaned(marker);
        assert!(matches!(
            DomainDeliveryService::validate_owned_scope(
                &ownership,
                1,
                10,
                "example.com",
                "app",
                DnsRecordType::A
            ),
            Err(DnsError::RecordConflict { .. })
        ));
    }

    fn mock_delivery_service(db: sea_orm::DatabaseConnection) -> DomainDeliveryService {
        let db = Arc::new(db);
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let providers = Arc::new(crate::services::DnsProviderService::new(
            db.clone(),
            encryption.clone(),
        ));
        let managed = Arc::new(ManagedDnsRecordService::new(
            db.clone(),
            providers,
            encryption.clone(),
        ));
        DomainDeliveryService::new(db, managed, encryption)
    }

    fn direct_profile_model(id: i32) -> delivery_profiles::Model {
        let now = Utc::now();
        delivery_profiles::Model {
            id,
            name: format!("direct-{id}"),
            provider_kind: "direct".into(),
            bunny_pull_zone_id: None,
            bunny_hostname: None,
            bunny_api_key_encrypted: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn delivery_list_sorting_defaults_to_created_at_descending() {
        assert_eq!(
            SortDirection::parse(None, "delivery profiles").expect("missing"),
            SortDirection::Desc
        );
        assert_eq!(
            SortDirection::parse(Some("  "), "delivery profiles").expect("blank"),
            SortDirection::Desc
        );
        assert_eq!(
            SortDirection::parse(Some("ASC"), "delivery profiles").expect("upper case"),
            SortDirection::Asc
        );
        assert_eq!(
            SortDirection::parse(Some("Desc"), "delivery profiles").expect("mixed case"),
            SortDirection::Desc
        );
        assert_eq!(
            ProfileSortBy::parse(None).expect("missing"),
            ProfileSortBy::CreatedAt
        );
        assert_eq!(
            ProfileSortBy::parse(Some("name")).expect("name"),
            ProfileSortBy::Name
        );
        assert_eq!(
            BindingSortBy::parse(Some("")).expect("blank"),
            BindingSortBy::CreatedAt
        );
        assert_eq!(
            BindingSortBy::parse(Some("hostname")).expect("hostname"),
            BindingSortBy::Hostname
        );
        assert_eq!(
            BindingSortBy::parse(Some("updated_at")).expect("updated_at"),
            BindingSortBy::UpdatedAt
        );
    }

    #[test]
    fn delivery_list_sorting_rejects_unknown_values_and_lists_allowed_ones() {
        let error = ProfileSortBy::parse(Some("hostname")).expect_err("profiles have no hostname");
        assert!(
            matches!(&error, DnsError::Validation(message)
                if message.contains("'hostname'") && message.contains("created_at, name")),
            "{error}"
        );
        let error = BindingSortBy::parse(Some("name")).expect_err("bindings have no name");
        assert!(
            matches!(&error, DnsError::Validation(message)
                if message.contains("'name'")
                    && message.contains("created_at, hostname, updated_at")),
            "{error}"
        );
        let error = SortDirection::parse(Some("up"), "domain delivery bindings")
            .expect_err("unknown direction");
        assert!(
            matches!(&error, DnsError::Validation(message)
                if message.contains("domain delivery bindings") && message.contains("asc, desc")),
            "{error}"
        );
        // Sort keys are column names and are matched exactly.
        assert!(ProfileSortBy::parse(Some("NAME")).is_err());
    }

    #[test]
    fn pages_past_the_last_row_are_detected_without_overflow() {
        assert!(page_is_past_end(1, 20, 0));
        assert!(!page_is_past_end(1, 20, 1));
        assert!(!page_is_past_end(2, 20, 21));
        assert!(page_is_past_end(2, 20, 20));
        assert!(page_is_past_end(u64::MAX, 100, u64::MAX - 1));
    }

    #[test]
    fn profile_page_without_provider_details_reduces_every_item() {
        let mut bunny = direct_profile_model(2);
        bunny.provider_kind = "bunny".into();
        bunny.bunny_pull_zone_id = Some(42);
        bunny.bunny_hostname = Some("edge.b-cdn.net".into());
        let page = DeliveryProfilePage {
            items: vec![
                DeliveryProfileResponse::try_from(bunny).expect("bunny profile"),
                DeliveryProfileResponse::try_from(direct_profile_model(1)).expect("direct"),
            ],
            total: 2,
            page: 1,
            page_size: 20,
        }
        .without_provider_details();
        assert_eq!(page.total, 2);
        assert!(page
            .items
            .iter()
            .all(|item| item.bunny_pull_zone_id.is_none() && item.bunny_hostname.is_none()));
        assert_eq!(page.items[0].provider_kind, DeliveryProviderKind::Bunny);
    }

    #[tokio::test]
    async fn listing_rejects_an_invalid_sort_before_querying() {
        // No query results are registered: touching the database would fail
        // with a database error instead of the validation error.
        let service = mock_delivery_service(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        );
        let result = service
            .list_profiles(
                PaginationParams {
                    sort_by: Some("bunny_api_key_encrypted".into()),
                    ..PaginationParams::default()
                },
                None,
            )
            .await;
        assert!(matches!(result, Err(DnsError::Validation(_))), "{result:?}");
        let result = service
            .list_bindings(
                7,
                PaginationParams {
                    sort_order: Some("sideways".into()),
                    ..PaginationParams::default()
                },
            )
            .await;
        assert!(matches!(result, Err(DnsError::Validation(_))), "{result:?}");
    }

    #[tokio::test]
    async fn listing_profiles_returns_the_requested_page_with_the_full_count() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[std::collections::BTreeMap::from([(
                "num_items",
                sea_orm::Value::BigInt(Some(21)),
            )])]])
            .append_query_results([vec![direct_profile_model(1)]])
            .into_connection();
        let page = mock_delivery_service(db)
            .list_profiles(
                PaginationParams {
                    page: Some(2),
                    page_size: None,
                    sort_by: None,
                    sort_order: None,
                },
                None,
            )
            .await
            .expect("second page");
        assert_eq!((page.total, page.page, page.page_size), (21, 2, 20));
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, 1);
    }

    #[tokio::test]
    async fn getting_a_missing_profile_is_not_found_naming_its_id() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<delivery_profiles::Model>::new()])
            .into_connection();
        let error = mock_delivery_service(db)
            .get_profile(41)
            .await
            .expect_err("missing profile");
        assert!(
            matches!(&error, DnsError::DeliveryProfileNotFound { profile_id: 41 }),
            "{error}"
        );
        assert_eq!(error.to_string(), "Delivery profile 41 not found");
    }

    #[test]
    fn profile_search_terms_are_trimmed_and_blank_means_no_filter() {
        assert_eq!(profile_search_term(None).expect("no term"), None);
        assert_eq!(profile_search_term(Some("   ")).expect("blank"), None);
        assert_eq!(
            profile_search_term(Some("  Edge ")).expect("trimmed"),
            Some("Edge")
        );
        // The limit counts characters, not bytes.
        let longest = "é".repeat(PROFILE_SEARCH_MAX_CHARS);
        assert_eq!(
            profile_search_term(Some(&longest)).expect("at the limit"),
            Some(longest.as_str())
        );
        let too_long = "a".repeat(PROFILE_SEARCH_MAX_CHARS + 1);
        let error = profile_search_term(Some(&too_long)).expect_err("over the limit");
        assert!(
            matches!(&error, DnsError::Validation(message) if message.contains("101 characters")),
            "{error}"
        );
    }

    #[test]
    fn profile_search_pattern_matches_wildcards_literally() {
        assert_eq!(substring_like_pattern("edge"), "%edge%");
        assert_eq!(substring_like_pattern(r"100%_\x"), r"%100\%\_\\x%");
    }
}
