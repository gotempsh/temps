//! DNS providers entity
//!
//! Stores DNS provider configurations with encrypted credentials.
//! Each provider can manage multiple domains and supports different
//! authentication methods (API token, API key + user, etc.).

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "dns_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// User-friendly name for this provider configuration
    pub name: String,

    /// Provider type: cloudflare, namecheap, route53, digitalocean, manual
    pub provider_type: String,

    /// Encrypted JSON with provider credentials
    /// Structure varies by provider type (see temps-dns credentials module)
    pub credentials: String,

    /// Whether this provider is currently active
    pub is_active: bool,

    /// Optional description or notes
    pub description: Option<String>,

    /// When the provider was last successfully used
    pub last_used_at: Option<DBDateTime>,

    /// Last error message if any operation failed
    pub last_error: Option<String>,

    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::dns_managed_domains::Entity")]
    DnsManagedDomains,
}

impl Related<super::dns_managed_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::DnsManagedDomains.def()
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
