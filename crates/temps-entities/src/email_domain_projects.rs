// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Explicit authorization for a project to send through a verified email domain.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "email_domain_projects")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub domain_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::email_domains::Entity",
        from = "Column::DomainId",
        to = "super::email_domains::Column::Id"
    )]
    EmailDomain,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}

impl ActiveModelBehavior for ActiveModel {}
