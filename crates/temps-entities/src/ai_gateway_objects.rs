// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership of provider-side files and batches created through the AI
//! gateway with an administrator-configured provider key.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// `kind` value for an uploaded or provider-generated file.
pub const KIND_FILE: &str = "file";
/// `kind` value for a batch job.
pub const KIND_BATCH: &str = "batch";

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_gateway_objects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `file` or `batch`
    pub kind: String,
    /// The provider's identifier, e.g. `file-abc123` or `batch_abc123`
    pub upstream_id: String,
    /// Provider that holds the object, e.g. `openai`
    pub provider: String,
    /// Administrator-configured key the object was created with
    pub provider_key_id: i32,
    /// Creating user; `None` when a deployment token created it
    pub owner_user_id: Option<i32>,
    /// Creating project, for deployment tokens
    pub owner_project_id: Option<i32>,
    /// Model every request in a batch input file targets
    pub model: Option<String>,
    /// Endpoint every request in a batch input file targets
    pub endpoint: Option<String>,
    /// When a finished batch's token usage was written to `ai_usage_logs`
    pub usage_recorded_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
