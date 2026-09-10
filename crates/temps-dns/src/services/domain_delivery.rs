//! Project-scoped traffic delivery planning and application.
//!
//! Delivery profiles describe how traffic reaches an origin. DNS hosting is a
//! separate concern: every write still goes through `ManagedDnsRecordService`.

use std::{net::IpAddr, sync::Arc};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, EntityTrait, QueryFilter, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_entities::{
    custom_routes, delivery_profiles, dns_managed_domains, dns_providers, domain_delivery_bindings,
    domain_delivery_previews, environment_delivery_settings, environment_domains, environments,
    project_custom_domains, project_delivery_settings, projects,
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    errors::DnsError,
    providers::{DnsRecordContent, DnsRecordRequest, DnsRecordType},
    services::{ManagedDnsRecordService, OwnershipScope, RecordOwnership},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryProviderKind {
    Direct,
    Cloudflare,
}

impl DeliveryProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Cloudflare => "cloudflare",
        }
    }
    fn parse(value: &str) -> Result<Self, DnsError> {
        match value {
            "direct" => Ok(Self::Direct),
            "cloudflare" => Ok(Self::Cloudflare),
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

fn adapter(kind: DeliveryProviderKind) -> Box<dyn DeliveryAdapter> {
    match kind {
        DeliveryProviderKind::Direct => Box::new(DirectAdapter),
        DeliveryProviderKind::Cloudflare => Box::new(CloudflareAdapter),
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
    #[schema(value_type=String,format=DateTime)]
    pub created_at: chrono::DateTime<Utc>,
    #[schema(value_type=String,format=DateTime)]
    pub updated_at: chrono::DateTime<Utc>,
}
impl TryFrom<delivery_profiles::Model> for DeliveryProfileResponse {
    type Error = DnsError;
    fn try_from(v: delivery_profiles::Model) -> Result<Self, Self::Error> {
        Ok(Self {
            id: v.id,
            name: v.name,
            provider_kind: DeliveryProviderKind::parse(&v.provider_kind)?,
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
    pub expected_existing_record: Option<serde_json::Value>,
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
    pub status: String,
    pub last_error: Option<String>,
    #[schema(value_type=String,format=DateTime)]
    pub applied_at: Option<chrono::DateTime<Utc>>,
}

#[async_trait]
pub trait DomainDeliveryDns: Send + Sync {
    async fn record_ownership(
        &self,
        domain: &str,
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
    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError>;
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
    async fn remove_record(
        &self,
        domain: &str,
        name: &str,
        record_type: DnsRecordType,
    ) -> Result<(), DnsError> {
        self.remove_managed_record(domain, name, record_type).await
    }
}

pub struct DomainDeliveryService {
    db: Arc<DatabaseConnection>,
    managed: Arc<dyn DomainDeliveryDns>,
}
impl DomainDeliveryService {
    pub fn new(db: Arc<DatabaseConnection>, managed: Arc<ManagedDnsRecordService>) -> Self {
        Self { db, managed }
    }
    pub fn with_dns(db: Arc<DatabaseConnection>, managed: Arc<dyn DomainDeliveryDns>) -> Self {
        Self { db, managed }
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
        ])
    }

    pub async fn create_profile(
        &self,
        name: String,
        kind: DeliveryProviderKind,
    ) -> Result<DeliveryProfileResponse, DnsError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(DnsError::Validation(
                "Delivery profile name must contain 1 to 100 characters".into(),
            ));
        }
        delivery_profiles::ActiveModel {
            name: Set(name.into()),
            provider_kind: Set(kind.as_str().into()),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?
        .try_into()
    }
    pub async fn list_profiles(&self) -> Result<Vec<DeliveryProfileResponse>, DnsError> {
        delivery_profiles::Entity::find()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }
    pub async fn delete_profile(&self, id: i32) -> Result<(), DnsError> {
        let exists = delivery_profiles::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DnsError::DomainNotFound(format!("delivery profile {id}")))?;
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
            return Err(DnsError::RecordConflict {
                domain: "delivery profiles".into(),
                name: exists.name,
                record_type: "PROFILE".into(),
                reason: "profile is still referenced by a project, environment, or domain binding"
                    .into(),
            });
        }
        delivery_profiles::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
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
        let env = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await?
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
            .ok_or_else(|| DnsError::DomainNotFound(format!("delivery profile {id}")))
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
    pub async fn update_settings(
        &self,
        project_id: i32,
        default_profile_id: Option<i32>,
        overrides: Vec<EnvironmentDeliveryOverride>,
    ) -> Result<ProjectDeliverySettingsResponse, DnsError> {
        self.require_project(project_id).await?;
        if let Some(id) = default_profile_id {
            self.profile(id).await?;
        }
        for item in &overrides {
            self.require_environment(project_id, item.environment_id)
                .await?;
            if let Some(id) = item.profile_id {
                self.profile(id).await?;
            }
        }
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
        for item in overrides {
            if let Some(existing) =
                environment_delivery_settings::Entity::find_by_id(item.environment_id)
                    .one(&transaction)
                    .await?
            {
                let mut active: environment_delivery_settings::ActiveModel = existing.into();
                active.profile_id = Set(item.profile_id);
                active.updated_at = Set(Utc::now());
                active.update(&transaction).await?;
            } else {
                environment_delivery_settings::ActiveModel {
                    environment_id: Set(item.environment_id),
                    profile_id: Set(item.profile_id),
                    updated_at: Set(Utc::now()),
                }
                .insert(&transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        self.settings(project_id).await
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
    fn existing_record(v: &RecordOwnership) -> Result<Option<serde_json::Value>, DnsError> {
        match v {
            RecordOwnership::Unmanaged(r)
            | RecordOwnership::Owned(r, _)
            | RecordOwnership::OwnedByOther(r, _) => Ok(Some(serde_json::to_value(r)?)),
            _ => Ok(None),
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
        let adapter = adapter(kind);
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
        for other in [DnsRecordType::A, DnsRecordType::AAAA, DnsRecordType::CNAME] {
            if other != record_type {
                let state = self
                    .managed
                    .record_ownership(&request.zone, &name, other)
                    .await?;
                if !matches!(
                    state,
                    RecordOwnership::NotFound | RecordOwnership::Orphaned(_)
                ) {
                    return Err(DnsError::RecordConflict{domain:request.zone.clone(),name:name.clone(),record_type:other.to_string(),reason:format!("a conflicting {other} record must be removed explicitly before changing record type")});
                }
            }
        }
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
                value: request.origin_target.clone(),
                proxied: requirements.record.proxied,
                ownership_status: status.into(),
                requires_adoption,
                expected_existing_record: Self::existing_record(&ownership)?,
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
        let _delivery_lock = self.acquire_delivery_lock(&request.hostname).await?;
        self.require_project(project_id).await?;
        self.require_environment(project_id, request.environment_id)
            .await?;
        let plan: DomainDeliveryPreviewResponse = serde_json::from_value(preview.plan.clone())?;
        let profile = self.profile(plan.profile_id).await?;
        let provider = dns_providers::Entity::find_by_id(request.dns_provider_id)
            .one(self.db.as_ref())
            .await?
            .filter(|provider| provider.is_active)
            .ok_or(DnsError::ProviderNotFound(request.dns_provider_id))?;
        adapter(DeliveryProviderKind::parse(&profile.provider_kind)?)
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
        let requirements = adapter(plan.provider_kind).plan(
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
        if Self::existing_record(&live_ownership)? != plan.record.expected_existing_record {
            return Err(DnsError::Validation(format!(
                "Delivery preview {preview_id} is stale because the provider record changed"
            )));
        }

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

        let reservation: Result<
            (
                project_custom_domains::Model,
                domain_delivery_bindings::Model,
            ),
            DnsError,
        > = async {
            let custom = if let Some(route) = current_route {
                route
            } else {
                project_custom_domains::ActiveModel {
                    project_id: Set(project_id),
                    environment_id: Set(request.environment_id),
                    domain: Set(request.hostname.clone()),
                    status: Set("pending".into()),
                    message: Set(Some(
                        "DNS delivery configured; awaiting certificate provisioning".into(),
                    )),
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?
            };
            let existing = domain_delivery_bindings::Entity::find()
                .filter(domain_delivery_bindings::Column::Hostname.eq(&request.hostname))
                .one(self.db.as_ref())
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
                let mut active: domain_delivery_bindings::ActiveModel = existing.into();
                active.environment_id = Set(request.environment_id);
                active.custom_domain_id = Set(custom.id);
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
                active.update(self.db.as_ref()).await?
            } else {
                domain_delivery_bindings::ActiveModel {
                    hostname: Set(request.hostname.clone()),
                    project_id: Set(project_id),
                    environment_id: Set(request.environment_id),
                    custom_domain_id: Set(custom.id),
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
                    ..Default::default()
                }
                .insert(self.db.as_ref())
                .await?
            };
            Ok((custom, binding))
        }
        .await;
        let (custom, binding) = match reservation {
            Ok(value) => value,
            Err(error) => {
                let mut failed: domain_delivery_previews::ActiveModel = preview.clone().into();
                failed.status = Set("failed".into());
                failed.last_error = Set(Some(error.to_string()));
                failed.update(self.db.as_ref()).await?;
                return Err(error);
            }
        };

        let dns_result: Result<(), DnsError> = async {
            if requested_adoption {
                self.managed
                    .import_record(
                        &request.zone,
                        &plan.record.name,
                        record_type,
                        OwnershipScope {
                            project_id: Some(project_id),
                            environment_id: Some(request.environment_id),
                            controller: Some("domain-delivery"),
                        },
                    )
                    .await?;
            }
            let written = self
                .managed
                .set_record(
                    &request.zone,
                    DnsRecordRequest {
                        name: plan.record.name.clone(),
                        content,
                        ttl: requirements.record.ttl,
                        proxied: false,
                    },
                    Some(plan.record.proxied),
                    OwnershipScope {
                        project_id: Some(project_id),
                        environment_id: Some(request.environment_id),
                        controller: Some("domain-delivery"),
                    },
                )
                .await?;
            let readback = self
                .managed
                .record_ownership(&request.zone, &plan.record.name, record_type)
                .await?;
            let readback_record = Self::existing_record(&readback)?;
            if readback_record != Some(serde_json::to_value(&written)?) {
                return Err(DnsError::ConnectionFailed(format!(
                    "Provider readback for {} {} did not match the record written",
                    record_type, request.hostname
                )));
            }
            Ok(())
        }
        .await;
        if let Err(error) = dns_result {
            let message = error.to_string();
            let mut failed: domain_delivery_bindings::ActiveModel = binding.into();
            failed.status = Set("failed".into());
            failed.last_error = Set(Some(message.clone()));
            failed.updated_at = Set(Utc::now());
            failed.update(self.db.as_ref()).await?;
            let mut failed_preview: domain_delivery_previews::ActiveModel = preview.into();
            failed_preview.status = Set("failed".into());
            failed_preview.last_error = Set(Some(message));
            failed_preview.update(self.db.as_ref()).await?;
            return Err(error);
        }
        let mut active_binding: domain_delivery_bindings::ActiveModel = binding.into();
        active_binding.status = Set(if custom.status == "active" {
            "active".into()
        } else {
            "dns_configured".into()
        });
        active_binding.last_error = Set(None);
        active_binding.updated_at = Set(Utc::now());
        active_binding.applied_at = Set(Some(Utc::now()));
        let binding = active_binding.update(self.db.as_ref()).await?;
        let mut active: domain_delivery_previews::ActiveModel = preview.into();
        active.status = Set("applied".into());
        active.last_error = Set(None);
        active.applied_at = Set(Some(Utc::now()));
        active.update(self.db.as_ref()).await?;
        self.binding_response(binding).await
    }
    async fn binding_response(
        &self,
        v: domain_delivery_bindings::Model,
    ) -> Result<DomainDeliveryBindingResponse, DnsError> {
        let p = self.profile(v.profile_id).await?;
        Ok(DomainDeliveryBindingResponse {
            id: v.id,
            hostname: v.hostname,
            project_id: v.project_id,
            environment_id: v.environment_id,
            custom_domain_id: v.custom_domain_id,
            delivery_profile_id: v.profile_id,
            delivery_profile_name: p.name.clone(),
            profile_source: v.profile_source,
            provider_kind: DeliveryProviderKind::parse(&p.provider_kind)?,
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
        })
    }
    pub async fn list_bindings(
        &self,
        project_id: i32,
    ) -> Result<Vec<DomainDeliveryBindingResponse>, DnsError> {
        self.require_project(project_id).await?;
        let mut out = Vec::new();
        for v in domain_delivery_bindings::Entity::find()
            .filter(domain_delivery_bindings::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?
        {
            out.push(self.binding_response(v).await?);
        }
        Ok(out)
    }

    pub async fn delete_binding(&self, project_id: i32, binding_id: i32) -> Result<(), DnsError> {
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
        let _delivery_lock = self.acquire_delivery_lock(&binding.hostname).await?;
        let binding=domain_delivery_bindings::Entity::find_by_id(binding_id).one(self.db.as_ref()).await?.filter(|current|current.project_id==project_id).ok_or_else(||DnsError::DomainNotFound(format!("delivery binding {binding_id} changed while cleanup was waiting for the hostname lock")))?;
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
        let (record_name, _, _) =
            Self::record(&binding.zone, &binding.hostname, &binding.origin_target)?;
        let ownership = self
            .managed
            .record_ownership(&binding.zone, &record_name, record_type)
            .await?;
        Self::validate_owned_scope(
            &ownership,
            project_id,
            binding.environment_id,
            &binding.zone,
            &record_name,
            record_type,
        )?;
        if let Err(error) = self
            .managed
            .remove_record(&binding.zone, &record_name, record_type)
            .await
        {
            let mut failed: domain_delivery_bindings::ActiveModel = binding.into();
            failed.status = Set("cleanup_failed".into());
            failed.last_error = Set(Some(error.to_string()));
            failed.updated_at = Set(Utc::now());
            failed.update(self.db.as_ref()).await?;
            return Err(error);
        }
        domain_delivery_bindings::Entity::delete_by_id(binding_id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::OwnershipMarker;
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
}
