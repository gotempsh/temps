// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The architecture blueprint of an AI workspace (ADR-053). One per workspace;
//! its versions live in `ai_blueprint_versions`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_blueprints")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The workspace (`ai_applications.id`) this blueprint describes.
    pub application_id: i64,
    /// The version the user accepted last, if any.
    pub accepted_version: Option<i32>,
    /// Which project or service realises each part of the design, by component
    /// id: `{"api": {"project_id": 4}, "db": {"service_id": 9}}`.
    pub bindings: serde_json::Value,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
