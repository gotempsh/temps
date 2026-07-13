//! Suppressed recipients entity — addresses that must not receive further
//! email due to a hard bounce, a spam complaint, or a manual admin action.
//! Checked by `EmailService::send` before every send.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "suppressed_recipients")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Always lowercased/trimmed before storage — see SuppressionService::normalize.
    pub email: String,
    /// "bounced" | "complained" | "manual"
    pub reason: String,
    /// Which domain's send triggered this, if known. Diagnostic only —
    /// suppression is enforced globally, not scoped to one sending domain.
    pub domain_id: Option<i32>,
    pub detail: Option<String>,
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
}

impl Related<super::email_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EmailDomain.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
