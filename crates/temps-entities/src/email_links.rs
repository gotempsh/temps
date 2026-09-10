// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email links entity for mapping tracked link indices to original URLs

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_links")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub email_id: Uuid,
    pub link_index: i32,
    pub original_url: String,
    pub click_count: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::emails::Entity",
        from = "Column::EmailId",
        to = "super::emails::Column::Id"
    )]
    Email,
}

impl Related<super::emails::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Email.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
