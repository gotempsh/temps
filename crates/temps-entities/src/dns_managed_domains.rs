//! DNS managed domains entity
//!
//! Tracks which domains are managed by which DNS provider.
//! This allows the system to know which provider to use when
//! automatically setting DNS records for a domain.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_managed_domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Reference to the DNS provider
    pub provider_id: i32,

    /// The domain name (e.g., "example.com")
    /// This is the base/apex domain, not subdomains
    pub domain: String,

    /// Provider-specific zone ID (for caching)
    pub zone_id: Option<String>,

    /// Whether automatic DNS management is enabled for this domain
    pub auto_manage: bool,

    /// Whether this domain has been verified (provider can access it)
    pub verified: bool,

    /// When verification was last checked
    pub verified_at: Option<DBDateTime>,

    /// Error message from last verification attempt
    pub verification_error: Option<String>,

    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::dns_providers::Entity",
        from = "Column::ProviderId",
        to = "super::dns_providers::Column::Id",
        on_delete = "Cascade"
    )]
    DnsProvider,
}

impl Related<super::dns_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::DnsProvider.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }

        Ok(self)
    }
}
