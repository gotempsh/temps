//! DNS ownership instance identity (ADR-031)
//!
//! Single-row table holding the random, install-scoped ID this temps
//! instance stamps into DNS ownership markers (`_temps-owned.*` TXT
//! records). Two temps installs managing the same zone use this to refuse
//! to touch each other's records.
//!
//! The ID is generated once on first managed-DNS write and never changes:
//! rotating it would orphan every record this install previously created.
//! It is intentionally NOT the telemetry `anonymous_id` — that one is
//! telemetry-scoped and must stay unlinkable to public DNS data.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_instance_identity")]
pub struct Model {
    /// Always 1 — the table is constrained to a single row.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,

    /// Random UUID identifying this temps install in ownership markers.
    pub instance_id: String,

    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
