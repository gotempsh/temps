// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::AccessServiceResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveAccessVault {
    pub name: String,
    /// Existing services in this project. At most 100, without duplicates.
    pub service_ids: Vec<String>,
    pub expected_version: Option<i32>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessVaultResponse {
    pub id: String,
    pub project_id: Option<i32>,
    pub name: String,
    pub service_ids: Vec<String>,
    pub version: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessVaultOverview {
    pub vault: AccessVaultResponse,
    /// Up to four services for the overview; service_ids gives the total.
    pub services: Vec<AccessServiceResponse>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessVaultList {
    pub items: Vec<AccessVaultOverview>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessVaultDetail {
    pub vault: AccessVaultResponse,
    /// Vaults are bounded to 100 services.
    pub services: Vec<AccessServiceResponse>,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveAccessBundle {
    pub name: String,
    /// A selection of this vault's services, not an authorization grant.
    pub service_ids: Vec<String>,
    pub ttl_seconds: i32,
    pub expected_version: Option<i32>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessBundleResponse {
    pub id: String,
    pub project_id: Option<i32>,
    pub vault_id: String,
    pub name: String,
    pub service_ids: Vec<String>,
    pub ttl_seconds: i32,
    pub version: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AccessBundleList {
    pub items: Vec<AccessBundleResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
impl From<temps_entities::access_vaults::Model> for AccessVaultResponse {
    fn from(v: temps_entities::access_vaults::Model) -> Self {
        Self {
            id: v.id,
            project_id: v.project_id,
            name: v.name,
            service_ids: v.service_ids,
            version: v.version,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}
impl From<temps_entities::access_bundles::Model> for AccessBundleResponse {
    fn from(v: temps_entities::access_bundles::Model) -> Self {
        Self {
            id: v.id,
            project_id: v.project_id,
            vault_id: v.vault_id,
            name: v.name,
            service_ids: v.service_ids,
            ttl_seconds: v.ttl_seconds,
            version: v.version,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}
