//! Backup providers tried, in `priority` order, after a domain's primary
//! provider (`email_domains.provider_id`) is exhausted. Checked by
//! `ProviderService::get_send_chain` on every send.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_domain_fallback_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub domain_id: i32,
    pub provider_id: i32,
    /// Lower priority is tried first, after the domain's primary provider.
    pub priority: i32,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::email_domains::Entity",
        from = "Column::DomainId",
        to = "super::email_domains::Column::Id"
    )]
    EmailDomain,
    #[sea_orm(
        belongs_to = "super::email_providers::Entity",
        from = "Column::ProviderId",
        to = "super::email_providers::Column::Id"
    )]
    EmailProvider,
}

impl Related<super::email_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailDomain.def()
    }
}

impl Related<super::email_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailProvider.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
