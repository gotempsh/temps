//! Append-only audit log for the standard (non-on-demand) certificate
//! issuance/renewal flow — `DomainService::request_challenge` and
//! `DomainService::complete_challenge`. Every attempt at either stage,
//! successful or failed, writes exactly one row here.
//!
//! This table is NOT the authoritative cert state — that lives on the
//! `domains` row (`status`, `last_error`, `last_error_type`). This is the
//! history: multiple rows per domain are expected over time. Contrast with
//! `on_demand_cert_attempts`, which covers the proxy's lazy on-demand TLS
//! issuance path instead.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "renewal_attempts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain_id: i32,
    /// `"request_challenge"` | `"complete_challenge"`.
    pub stage: String,
    /// `"http-01"` | `"dns-01"`.
    pub verification_method: String,
    /// `"success"` | `"failed"`.
    pub outcome: String,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::DomainId",
        to = "super::domains::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Domains,
}

impl Related<super::domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domains.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        // Append-only: only stamp `created_at` on insert. Rows are never
        // mutated after they are written.
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
