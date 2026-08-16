use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Raw application source file stored for native (non-source-map) error
/// symbolication. Keyed by `(project_id, release, file_path)` — the same shape
/// as [`super::source_maps`] — so the symbolication pipeline can attach source
/// context to Go/Rust/etc. frames that already carry original filename+lineno.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "source_files")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub project_id: i32,

    /// Release this source belongs to. Must match the SDK's reported release
    /// (e.g. the deployed commit SHA / tag) for frames to resolve.
    pub release: String,

    /// Normalized path of the source file as it appears in stack frames
    /// (e.g. "~/internal/gateway/middleware.go").
    pub file_path: String,

    /// Raw source file bytes (UTF-8 text in practice).
    #[serde(skip_serializing)]
    pub content: Vec<u8>,

    /// Size of the source content in bytes.
    pub size_bytes: i64,

    /// SHA256 checksum of the content.
    pub checksum: Option<String>,

    pub created_at: DBDateTime,
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
    Projects,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Projects.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
