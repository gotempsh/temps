// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-host sandbox image preparation: whether the operator asked Temps to
//! keep the host's default sandbox image pulled, and what the last
//! preparation did. One row per host; `node_id` is `NULL` for the control
//! plane. See `m20261119_000002_sandbox_image_preparations`.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sandbox_image_preparations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// The worker node, or `None` for the control plane.
    pub node_id: Option<i32>,
    /// Keep this host's default sandbox image pulled, re-pulling it when the
    /// default changes. Off until the operator prepares the host.
    pub keep_prepared: bool,
    /// The exact image reference last prepared on this host.
    pub prepared_image: Option<String>,
    /// A previously prepared image still to remove (a container used it).
    pub stale_image: Option<String>,
    /// Why the last preparation failed.
    pub last_error: Option<String>,
    pub last_error_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Node,
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
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
