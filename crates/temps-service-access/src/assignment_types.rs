// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::types::{GrantEnforcement, SandboxAccessPolicy};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestSandboxService {
    pub service_id: String,
    pub service_version: i32,
}
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveSandboxService {
    pub expected_version: i32,
    pub service_version: i32,
    /// Omit for the sandbox lifetime. A fixed expiry never renews beyond this deadline.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeSandboxService {
    pub expected_version: i32,
}
#[derive(Debug, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxServiceStatus {
    PendingApproval,
    Approved,
    Suspended,
    Revoked,
    Expired,
    PolicyChanged,
    ServiceDisabled,
}
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SandboxServiceResponse {
    pub id: String,
    pub service_id: String,
    pub service_name: String,
    pub service_version: i32,
    pub policy: SandboxAccessPolicy,
    pub version: i32,
    pub status: SandboxServiceStatus,
    pub enforcement: GrantEnforcement,
    pub connected: bool,
    pub reason: String,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SandboxServiceList {
    pub items: Vec<SandboxServiceResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
}
