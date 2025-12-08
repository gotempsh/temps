use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "vulnerabilities")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub scan_id: i32,
    pub vulnerability_id: String,
    pub package_name: String,
    pub installed_version: String,
    pub fixed_version: Option<String>,
    pub severity: String,
    pub title: String,
    pub description: Option<String>,
    pub references: Option<JsonValue>,
    pub cvss_score: Option<f32>,
    pub primary_url: Option<String>,
    pub published_date: Option<DBDateTime>,
    pub last_modified_date: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::vulnerability_scans::Entity",
        from = "Column::ScanId",
        to = "super::vulnerability_scans::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    VulnerabilityScans,
}

impl Related<super::vulnerability_scans::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::VulnerabilityScans.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
