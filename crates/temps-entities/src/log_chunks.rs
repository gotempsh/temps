use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "log_chunks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: i32,
    pub env: String,
    pub service: String,
    pub container_id: String,
    pub deploy_id: Option<i32>,
    pub started_at: DBDateTime,
    pub ended_at: DBDateTime,
    pub storage_key: String,
    pub line_count: i32,
    pub compressed_size_bytes: i32,
    pub has_errors: bool,
    /// Byte offset of every 100th line (uncompressed) for partial retrieval
    pub line_offsets: Vec<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
