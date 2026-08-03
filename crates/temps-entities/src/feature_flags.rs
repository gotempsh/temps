//! `SeaORM` Entity for the `feature_flags` table.
//!
//! A feature flag is defined once per project and its value is overridden per
//! environment (see [`super::feature_flag_environments`]). This mirrors the
//! `env_vars` / `env_var_environments` split.
//!
//! Two columns exist in Phase 1 but are deliberately unused by the evaluator:
//!
//! - `salt` is generated at create time so percentage bucketing is stable from
//!   the very first rollout. Introducing it later would reshuffle every user
//!   already assigned to a variant.
//! - `client_visible` defaults to `false` so flags are server-only unless the
//!   operator opts in. Flipping a security default after the fact would expose
//!   flags that were never meant to leave the server.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "feature_flags")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// Stable identifier used in application code. Immutable after create:
    /// keys leak into user source and (later) analytics dimensions.
    pub key: String,
    /// One of `bool`, `string`, `number`, `json`. Fixed at create — retyping
    /// would break both stored data and user code.
    pub value_type: String,
    /// Served whenever evaluation cannot do better. Never SQL NULL.
    pub default_value: Json,
    pub description: Option<String>,
    /// Per-flag bucketing salt. Rotating it reshuffles the rollout cohort.
    /// Populated at create; not read until targeting ships.
    ///
    /// Never serialized. Every response DTO omits it by hand today, but that is
    /// convention rather than enforcement — publishing the salt would let a
    /// client pre-compute `bucket(key, salt, subject)` and self-select into or
    /// out of a percentage rollout. `skip_serializing` makes that durable if
    /// this entity is ever returned directly.
    #[serde(skip_serializing)]
    pub salt: String,
    /// When false (the default) the flag is never exposed on the
    /// unauthenticated same-origin evaluation endpoint.
    pub client_visible: bool,
    /// Soft delete. Archived flags evaluate as `FLAG_NOT_FOUND` so callers
    /// fall back to their own default rather than silently changing behaviour.
    pub archived_at: Option<DBDateTime>,
    /// When an app last actually evaluated this flag, reported in batches by
    /// the SDK. `None` means never seen — which is a real answer, not missing
    /// data: a flag created and never referenced by any running code.
    ///
    /// Deliberately *not* stamped when a snapshot is fetched: the snapshot
    /// carries every flag in the environment, so stamping on fetch would make
    /// unreferenced flags look freshly used and defeat the whole point.
    pub last_evaluated_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        has_many = "super::feature_flag_environments::Entity",
        from = "Column::Id",
        to = "super::feature_flag_environments::Column::FlagId"
    )]
    FeatureFlagEnvironments,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::feature_flag_environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::FeatureFlagEnvironments.def()
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
