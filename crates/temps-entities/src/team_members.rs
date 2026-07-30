use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One row per (team, user) pair.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "team_members")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub team_id: i32,
    pub user_id: i32,
    /// `owner | admin | deployer | viewer` — see [`super::TeamRole`].
    /// Used when `custom_role_id` is `None`.
    pub role: String,
    /// When `Some`, this member's effective permissions for projects the
    /// team can access come from `custom_role_permissions` for this role
    /// instead of the fixed `role` column above. `ON DELETE SET NULL`: a
    /// deleted custom role falls the member back to `role`.
    pub custom_role_id: Option<i32>,
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
        belongs_to = "super::custom_roles::Entity",
        from = "Column::CustomRoleId",
        to = "super::custom_roles::Column::Id",
        on_delete = "SetNull"
    )]
    CustomRole,
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

impl Related<super::custom_roles::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CustomRole.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
