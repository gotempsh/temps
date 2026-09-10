// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable idempotency claims for deployment-token email sends.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_idempotency_keys")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    pub payload_hash: String,
    pub email_id: Uuid,
    pub lease_expires_at: DBDateTime,
    pub created_at: DBDateTime,
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
        belongs_to = "super::emails::Entity",
        from = "Column::EmailId",
        to = "super::emails::Column::Id"
    )]
    Email,
}

impl ActiveModelBehavior for ActiveModel {}
