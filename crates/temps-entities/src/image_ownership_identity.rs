// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Image ownership instance identity.
//!
//! Single-row table holding the random, install-scoped ID this Temps install
//! stamps on the images it builds (`sh.temps.instance`), so housekeeping can
//! tell its own orphaned images from another install's on a shared Docker
//! host. Created on first use and never rotated: rotating it would make every
//! image built before unrecognisable, which leaves them on disk rather than
//! removing anything wrongly. Separate from `dns_instance_identity`, which is
//! published in DNS ownership markers.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "image_ownership_identity")]
pub struct Model {
    /// Always 1: the table is constrained to a single row.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,

    /// Random UUID identifying this install in image labels.
    pub instance_id: String,

    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
