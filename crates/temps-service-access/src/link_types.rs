// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateVaultLink {
    pub project_id: Option<i32>,
    pub sandbox_id: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, ToSchema, sea_orm::FromQueryResult)]
pub struct VaultLinkResponse {
    pub id: String,
    pub vault_id: String,
    pub consumer_name: Option<String>,
    pub project_id: Option<i32>,
    pub sandbox_id: Option<i32>,
    #[schema(value_type=String, format=DateTime)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type=Option<String>, format=DateTime)]
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl From<temps_entities::access_vault_links::Model> for VaultLinkResponse {
    fn from(row: temps_entities::access_vault_links::Model) -> Self {
        Self {
            consumer_name: None,
            id: row.id,
            vault_id: row.vault_id,
            project_id: row.project_id,
            sandbox_id: row.sandbox_id,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        }
    }
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VaultLinkList {
    pub items: Vec<VaultLinkResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
