// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "environment_domains")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub environment_id: i32,
    pub domain: String,
    pub created_at: DBDateTime,
    /// Who this address is for (ADR 064): `public`, or the reserved
    /// `private` / `signed_in`. Defaults to `public`.
    #[serde(default = "crate::audience::default_audience")]
    pub audience: String,
    /// For a `private` address (ADR 064 D5): the network range a visitor
    /// must come from, e.g. `100.64.0.0/10`.
    #[serde(default)]
    pub private_range: Option<String>,
    /// For a `private` address: the worker whose private listener serves it.
    #[serde(default)]
    pub private_node_id: Option<i32>,
    /// For a `private` address: the listener address DNS points at.
    #[serde(default)]
    pub private_listener: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::environments::Entity",
        from = "Column::EnvironmentId",
        to = "super::environments::Column::Id"
    )]
    Environment,
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

        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }

        Ok(self)
    }
}
