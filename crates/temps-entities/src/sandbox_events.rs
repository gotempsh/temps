use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Append-only timeline of lifecycle operations performed on a standalone
/// sandbox — create, stop, resume, restart, extend-timeout, resize,
/// preview-password changes, destroy. Deliberately excludes shell/exec
/// activity: this is the audit trail of *operations on the sandbox*, not of
/// commands run inside it.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sandbox_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Internal id of the sandbox this event belongs to (`sandboxes.id`).
    pub sandbox_id: i32,

    /// Machine-readable operation, e.g. `created`, `stopped`, `resumed`,
    /// `restarted`, `timeout_extended`, `resized`, `preview_password_set`,
    /// `preview_password_cleared`, `source_seeded`, `destroyed`.
    pub event_type: String,

    /// Optional structured context for the event (new expiry, from/to disk
    /// size, source type, …). Shape depends on `event_type`.
    #[sea_orm(column_type = "JsonBinary")]
    pub detail: Option<serde_json::Value>,

    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
