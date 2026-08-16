use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub user_id: i32,
    #[serde(skip_serializing)]
    pub session_token: String,
    pub expires_at: DBDateTime,
    /// True while this row is a first-factor-only MFA challenge awaiting TOTP
    /// verification. Such rows must never authenticate a real request: only
    /// `verify_mfa_challenge` may consume them, and `verify_session` filters
    /// them out. A fully authenticated session has this set to false.
    pub mfa_pending: bool,
    /// Until this instant the session may perform sensitive actions. `NULL`
    /// means the session has not completed recent step-up verification.
    pub step_up_expires_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id"
    )]
    User,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
