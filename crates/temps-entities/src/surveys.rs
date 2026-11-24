// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `SeaORM` Entity for the `surveys` table (ADR 110 D1).
//!
//! A survey is defined once per project. Answers are ordinary analytics
//! events (`survey_shown`, `survey_dismissed`, `survey_sent`) carrying the
//! survey id, so this table holds only the definition.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "surveys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub description: Option<String>,
    /// One of `draft`, `active`, `paused`, `completed`. Only `active` surveys
    /// reach browsers.
    pub status: String,
    /// Ordered questions, validated by the surveys service on every write.
    pub questions: Json,
    /// Display conditions (page path, triggering event, sampling, once per
    /// visitor, a feature flag). `{}` shows the survey on every page.
    pub targeting: Json,
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
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
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
