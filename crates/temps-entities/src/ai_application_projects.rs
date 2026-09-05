// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_application_projects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub application_id: i64,
    pub project_id: i32,
    /// Exactly one link per application is primary. The service layer keeps
    /// this invariant transactional; a partial unique index prevents races.
    pub is_primary: bool,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
