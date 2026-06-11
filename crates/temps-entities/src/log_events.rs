use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Hypertable for ERROR and WARN log events.
///
/// Provides fast indexed search within the 7-day hot window.
/// Retention and compression managed by TimescaleDB policies.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "log_events")]
pub struct Model {
    /// TimescaleDB time column — partition key
    #[sea_orm(primary_key, auto_increment = false)]
    pub time: DBDateTime,
    pub project_id: i32,
    pub service: String,
    pub env: String,
    /// Normalized level: ERROR or WARN
    pub level: String,
    pub message: String,
    /// Extracted structured fields from the log line (JSONB)
    pub fields: Option<Json>,
    /// Reference to the chunk containing this line
    pub chunk_id: Uuid,
    /// Line offset within the chunk for context retrieval
    pub line_offset: i32,
    pub deploy_id: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
