// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "delivery_profiles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub provider_kind: String,
    pub bunny_pull_zone_id: Option<i64>,
    pub bunny_hostname: Option<String>,
    #[serde(skip_serializing, skip_deserializing)]
    pub bunny_api_key_encrypted: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}

impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeliveryProfile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("provider_kind", &self.provider_kind)
            .field("bunny_pull_zone_id", &self.bunny_pull_zone_id)
            .field("bunny_hostname", &self.bunny_hostname)
            .field(
                "bunny_api_key_encrypted",
                &self.bunny_api_key_encrypted.is_some(),
            )
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Model;

    #[test]
    fn encrypted_api_key_is_not_exposed_by_debug_or_json() {
        let now = chrono::Utc::now();
        let profile = Model {
            id: 1,
            name: "test".into(),
            provider_kind: "bunny".into(),
            bunny_pull_zone_id: Some(42),
            bunny_hostname: Some("test.b-cdn.net".into()),
            bunny_api_key_encrypted: Some("private-encrypted-value".into()),
            created_at: now,
            updated_at: now,
        };

        assert!(!format!("{profile:?}").contains("private-encrypted-value"));
        let json = serde_json::to_string(&profile).expect("serialize profile");
        assert!(!json.contains("private-encrypted-value"));
        assert!(!json.contains("bunny_api_key_encrypted"));
    }
}
