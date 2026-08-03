//! `SeaORM` Entity for the `feature_flag_environments` table.
//!
//! Per-environment override of a [`super::feature_flags`] definition. One row
//! per (flag, environment); a missing row means "inherit the flag default".
//!
//! `rules` ships in Phase 1 as a column that is always `[]`. The evaluator
//! reserves a step for it between the kill switch and the environment value,
//! so populating it later cannot change what an existing flag serves.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "feature_flag_environments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub flag_id: i32,
    pub environment_id: i32,
    /// The kill switch. When false, evaluation short-circuits to the flag's
    /// default value *before* any targeting is consulted — so this keeps
    /// meaning exactly "ignore everything, serve the default" once rules exist.
    pub enabled: bool,
    /// Environment-specific value. NULL means inherit `feature_flags.default_value`.
    pub value: Option<Json>,
    /// Ordered targeting rules. Always `[]` in Phase 1.
    pub rules: Json,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::feature_flags::Entity",
        from = "Column::FlagId",
        to = "super::feature_flags::Column::Id"
    )]
    FeatureFlag,
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
}

impl Related<super::feature_flags::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::FeatureFlag.def()
    }
}

impl Related<super::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
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
