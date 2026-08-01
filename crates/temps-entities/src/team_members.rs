use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One row per (team, user) pair.
///
/// The member's permissions inside a project come from `role` intersected
/// with the role their team holds on that project. A plugin can override
/// the `role` half per membership via
/// `temps_core::MembershipPermissionResolver`, keyed on this row's `id` —
/// which is why there is no role-override column here: what a plugin
/// stores is the plugin's business, and core keeps no dangling reference
/// to a table it doesn't own.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "team_members")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub team_id: i32,
    pub user_id: i32,
    /// `owner | admin | deployer | viewer` — see [`super::TeamRole`].
    pub role: String,
    pub added_by: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::teams::Entity",
        from = "Column::TeamId",
        to = "super::teams::Column::Id",
        on_delete = "Cascade"
    )]
    Team,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::teams::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Team.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
