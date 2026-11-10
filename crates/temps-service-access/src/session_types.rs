// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::ServiceGrantResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Extensible consumer identity. Only authenticated sandbox bindings are supported today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessSessionConsumer {
    Sandbox { sandbox_id: String },
}
impl AccessSessionConsumer {
    pub fn sandbox_id(&self) -> &str {
        match self {
            Self::Sandbox { sandbox_id } => sandbox_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionServiceVersion {
    pub service_id: String,
    pub version: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAccessSession {
    /// Stable for retries of this approval. A new session needs a new UUID.
    #[schema(value_type = String, format = "uuid")]
    pub request_id: Uuid,
    pub bundle_id: String,
    pub bundle_version: i32,
    pub consumer: AccessSessionConsumer,
    pub ttl_seconds: i32,
    /// Exact policies the operator reviewed, one per bundle service (maximum 16).
    pub services: Vec<SessionServiceVersion>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessSessionStatus {
    Issued,
    Expired,
    Revoked,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessSessionResponse {
    pub id: String,
    pub project_id: i32,
    pub vault_id: String,
    pub bundle_id: String,
    pub bundle_name: String,
    pub bundle_version: i32,
    pub consumer: AccessSessionConsumer,
    pub created_by: Option<i32>,
    /// Authorization state only. Inspect grants for acknowledged worker enforcement.
    pub status: AccessSessionStatus,
    pub service_count: usize,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub revoked_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessSessionDetail {
    pub session: AccessSessionResponse,
    pub grants: Vec<ServiceGrantResponse>,
}
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct AccessSessionQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub vault_id: Option<String>,
    /// Filter by the sandbox's public ID; ownership still applies.
    pub sandbox_id: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessSessionList {
    pub items: Vec<AccessSessionResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
