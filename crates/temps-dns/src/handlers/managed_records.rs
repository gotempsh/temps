//! HTTP handlers for ownership-guarded managed DNS records (ADR-031)
//!
//! These endpoints power the domain UI's per-record state and the
//! import-or-skip conflict flow. All writes go through
//! [`ManagedDnsRecordService`], so the never-overwrite invariant is enforced
//! in the service layer regardless of what the client sends; conflicts come
//! back as RFC 7807 responses with HTTP 409.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, Permission, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::ToSchema;

use crate::providers::{DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};
use crate::services::{OwnershipScope, RecordOwnership};

use super::DnsAppState;

// ========================================
// Request/Response Types
// ========================================

/// Query selecting one record by zone-relative name and type
#[derive(Debug, Clone, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ManagedRecordQuery {
    /// Domain (any FQDN under a managed zone)
    #[schema(example = "example.com")]
    pub domain: String,
    /// Record name relative to the zone ("@" for apex)
    #[schema(example = "app")]
    pub name: String,
    /// Record type
    pub record_type: DnsRecordType,
}

/// Request to create or update a managed DNS record
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetManagedRecordRequest {
    /// Domain (any FQDN under a managed zone)
    #[schema(example = "example.com")]
    pub domain: String,
    /// Record name relative to the zone ("@" for apex)
    #[schema(example = "app")]
    pub name: String,
    /// Record content (determines the record type)
    pub content: DnsRecordContent,
    /// TTL in seconds (None = provider default)
    pub ttl: Option<u32>,
    /// Proxy through the provider's CDN (Cloudflare orange-cloud). Also
    /// enabled by the managed domain's `proxied_by_default`.
    #[serde(default)]
    pub proxied: bool,
    /// Project this record belongs to (stamped into the ownership marker)
    pub project_id: Option<i32>,
    /// Environment this record belongs to (stamped into the ownership marker)
    pub environment_id: Option<i32>,
}

/// Request to import (adopt) an existing record into temps management
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImportManagedRecordRequest {
    /// Domain (any FQDN under a managed zone)
    #[schema(example = "example.com")]
    pub domain: String,
    /// Record name relative to the zone ("@" for apex)
    #[schema(example = "app")]
    pub name: String,
    /// Record type
    pub record_type: DnsRecordType,
    /// Project this record belongs to (stamped into the ownership marker)
    pub project_id: Option<i32>,
    /// Environment this record belongs to (stamped into the ownership marker)
    pub environment_id: Option<i32>,
}

/// Ownership state of one record, for the conflict/import UI
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordOwnershipResponse {
    /// One of: not_found | unmanaged | owned | owned_by_other
    #[schema(example = "unmanaged")]
    pub status: String,
    /// The record at the provider, when one exists
    pub record: Option<DnsRecord>,
    /// Whether this temps install may modify the record
    pub writable: bool,
    /// Owning install's instance ID when owned by a different temps install
    pub owner_instance: Option<String>,
    /// Project stamped in the ownership marker, when owned
    pub project_id: Option<i32>,
    /// Environment stamped in the ownership marker, when owned
    pub environment_id: Option<i32>,
}

impl From<RecordOwnership> for RecordOwnershipResponse {
    fn from(ownership: RecordOwnership) -> Self {
        match ownership {
            RecordOwnership::NotFound => Self {
                status: "not_found".to_string(),
                record: None,
                writable: true,
                owner_instance: None,
                project_id: None,
                environment_id: None,
            },
            RecordOwnership::Unmanaged(record) => Self {
                status: "unmanaged".to_string(),
                record: Some(record),
                writable: false,
                owner_instance: None,
                project_id: None,
                environment_id: None,
            },
            RecordOwnership::Owned(record, marker) => Self {
                status: "owned".to_string(),
                record: Some(record),
                writable: true,
                owner_instance: None,
                project_id: marker.project_id,
                environment_id: marker.environment_id,
            },
            RecordOwnership::OwnedByOther(record, marker) => Self {
                status: "owned_by_other".to_string(),
                record: Some(record),
                writable: false,
                owner_instance: Some(marker.instance),
                project_id: marker.project_id,
                environment_id: marker.environment_id,
            },
        }
    }
}

/// Result of importing a record into temps management
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportManagedRecordResponse {
    /// Record name that was imported
    pub name: String,
    /// Record type that was imported
    pub record_type: String,
    /// Project stamped in the ownership marker
    pub project_id: Option<i32>,
    /// Environment stamped in the ownership marker
    pub environment_id: Option<i32>,
}

// ========================================
// Audit events
// ========================================

#[derive(Debug, Clone, Serialize)]
struct ManagedDnsRecordSetAudit {
    context: AuditContext,
    domain: String,
    name: String,
    record_type: String,
    proxied: bool,
    project_id: Option<i32>,
    environment_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
struct ManagedDnsRecordRemovedAudit {
    context: AuditContext,
    domain: String,
    name: String,
    record_type: String,
}

#[derive(Debug, Clone, Serialize)]
struct ManagedDnsRecordImportedAudit {
    context: AuditContext,
    domain: String,
    name: String,
    record_type: String,
    project_id: Option<i32>,
    environment_id: Option<i32>,
}

macro_rules! impl_audit_operation {
    ($ty:ty, $op:literal) => {
        impl AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $op.to_string()
            }
            fn user_id(&self) -> i32 {
                self.context.user_id
            }
            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }
            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }
            fn serialize(&self) -> anyhow::Result<String> {
                serde_json::to_string(self)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
            }
        }
    };
}

impl_audit_operation!(ManagedDnsRecordSetAudit, "MANAGED_DNS_RECORD_SET");
impl_audit_operation!(ManagedDnsRecordRemovedAudit, "MANAGED_DNS_RECORD_REMOVED");
impl_audit_operation!(ManagedDnsRecordImportedAudit, "MANAGED_DNS_RECORD_IMPORTED");

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

// ========================================
// Handlers
// ========================================

/// Get the ownership state of a DNS record
#[utoipa::path(
    tag = "DNS Records",
    get,
    path = "/dns-records/ownership",
    params(ManagedRecordQuery),
    responses(
        (status = 200, description = "Ownership state", body = RecordOwnershipResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not managed by any DNS provider"),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn get_record_ownership(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Query(query): Query<ManagedRecordQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsRead);

    let ownership = state
        .managed_record_service
        .record_ownership(&query.domain, &query.name, query.record_type)
        .await?;

    Ok(Json(RecordOwnershipResponse::from(ownership)))
}

/// Create or update a managed DNS record (ownership-guarded)
#[utoipa::path(
    tag = "DNS Records",
    post,
    path = "/dns-records",
    request_body = SetManagedRecordRequest,
    responses(
        (status = 200, description = "Record set", body = DnsRecord),
        (status = 400, description = "Validation error (e.g. proxied depth limit)"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not managed by any DNS provider"),
        (status = 409, description = "Record exists and is not managed by temps"),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn set_managed_record(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SetManagedRecordRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let record = state
        .managed_record_service
        .set_managed_record(
            &request.domain,
            DnsRecordRequest {
                name: request.name.clone(),
                content: request.content.clone(),
                ttl: request.ttl,
                proxied: request.proxied,
            },
            OwnershipScope {
                project_id: request.project_id,
                environment_id: request.environment_id,
            },
        )
        .await?;

    let audit = ManagedDnsRecordSetAudit {
        context: audit_context(&auth, &metadata),
        domain: request.domain.clone(),
        name: request.name.clone(),
        record_type: record.content.record_type().to_string(),
        proxied: record.proxied,
        project_id: request.project_id,
        environment_id: request.environment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for managed DNS record set: {}",
            e
        );
    }

    Ok(Json(record))
}

/// Delete a managed DNS record (only records owned by this install)
#[utoipa::path(
    tag = "DNS Records",
    delete,
    path = "/dns-records",
    params(ManagedRecordQuery),
    responses(
        (status = 204, description = "Record removed (or already absent)"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Domain not managed by any DNS provider"),
        (status = 409, description = "Record is not managed by temps"),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn remove_managed_record(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Query(query): Query<ManagedRecordQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    state
        .managed_record_service
        .remove_managed_record(&query.domain, &query.name, query.record_type)
        .await?;

    let audit = ManagedDnsRecordRemovedAudit {
        context: audit_context(&auth, &metadata),
        domain: query.domain.clone(),
        name: query.name.clone(),
        record_type: query.record_type.to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for managed DNS record removal: {}",
            e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Import an existing DNS record into temps management (explicit adoption)
#[utoipa::path(
    tag = "DNS Records",
    post,
    path = "/dns-records/import",
    request_body = ImportManagedRecordRequest,
    responses(
        (status = 200, description = "Record imported", body = ImportManagedRecordResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Record or managed domain not found"),
        (status = 409, description = "Record is owned by another temps install"),
    ),
    security(("bearer_auth" = []))
)]
pub(super) async fn import_managed_record(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ImportManagedRecordRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::SettingsWrite);

    let marker = state
        .managed_record_service
        .import_record(
            &request.domain,
            &request.name,
            request.record_type,
            OwnershipScope {
                project_id: request.project_id,
                environment_id: request.environment_id,
            },
        )
        .await?;

    let audit = ManagedDnsRecordImportedAudit {
        context: audit_context(&auth, &metadata),
        domain: request.domain.clone(),
        name: request.name.clone(),
        record_type: request.record_type.to_string(),
        project_id: marker.project_id,
        environment_id: marker.environment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for managed DNS record import: {}",
            e
        );
    }

    Ok(Json(ImportManagedRecordResponse {
        name: request.name,
        record_type: request.record_type.to_string(),
        project_id: marker.project_id,
        environment_id: marker.environment_id,
    }))
}
