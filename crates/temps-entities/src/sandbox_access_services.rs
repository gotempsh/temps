// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;

// Internal storage only: never serialize this model into a response or audit.
#[derive(Clone, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_access_services")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub project_id: i32,
    pub name: String,
    pub policy: Json,
    pub max_ttl_seconds: i32,
    pub enabled: bool,
    pub credential_encrypted: Option<String>,
    pub version: i32,
    pub created_at: temps_core::DBDateTime,
    pub updated_at: temps_core::DBDateTime,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
}
impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}

impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxAccessService")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .field("has_credential", &self.credential_encrypted.is_some())
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Model;

    #[test]
    fn debug_reports_credential_presence_without_exposing_ciphertext() {
        let now = chrono::Utc::now();
        let mut service = Model {
            id: "svc_test".into(),
            project_id: 7,
            name: "API".into(),
            policy: serde_json::json!({}),
            max_ttl_seconds: 900,
            enabled: true,
            credential_encrypted: Some("test-encrypted-credential".into()),
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let debug = format!("{service:?}");
        assert!(!debug.contains("test-encrypted-credential"));
        assert!(debug.contains("has_credential: true"));
        assert!(debug.contains("svc_test"));
        service.credential_encrypted = None;
        assert!(format!("{service:?}").contains("has_credential: false"));
    }
}
