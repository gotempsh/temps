//! Junction table restricting a secret to specific Docker Compose services.
//!
//! Mirrors `secret_environments`: one row per (secret_id, service_name) pair.
//!
//! **A secret with no rows here is delivered to every service in the stack**,
//! which is the behaviour every secret had before this table existed. Scoping
//! is therefore opt-in and additive -- an empty set can never mean "deliver
//! nowhere", because that would turn "the user has not configured scoping"
//! into a silent outage.
//!
//! `service_name` is not a foreign key: Compose service names come from the
//! user's repository, so nothing in this schema can guarantee they exist. A
//! name that no longer matches the deployed compose file is reported in the
//! deployment log rather than blocking the write, because the repository can
//! change long after the secret was scoped.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "secret_compose_services")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub secret_id: i32,
    /// Compose service name exactly as written in the compose file's
    /// `services:` mapping.
    pub service_name: String,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::secrets::Entity",
        from = "Column::SecretId",
        to = "super::secrets::Column::Id"
    )]
    Secret,
}

impl Related<super::secrets::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Secret.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }

        Ok(self)
    }
}
