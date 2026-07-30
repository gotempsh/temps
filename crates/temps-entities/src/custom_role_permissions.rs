use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One row per (custom role, permission) pair.
///
/// `permission` stores `Permission::to_string()` (e.g. `"deployments:write"`),
/// validated against `Permission::from_str` at write time in the service
/// layer, not at the DB level — the permission vocabulary lives in
/// `temps-auth`, which this data crate does not depend on.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "custom_role_permissions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub role_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub permission: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::custom_roles::Entity",
        from = "Column::RoleId",
        to = "super::custom_roles::Column::Id",
        on_delete = "Cascade"
    )]
    CustomRole,
}

impl Related<super::custom_roles::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CustomRole.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
