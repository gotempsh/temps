// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Desired state for the one persistent workspace owned by an AI application.
///
/// The sandbox/container is deliberately only a realization of this row. If
/// compute disappears, reconciliation recreates it with the same volume path
/// and limits.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_application_workspaces")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub application_id: i64,
    pub sandbox_public_id: Option<String>,
    pub desired_state: String,
    pub runtime: String,
    pub image: Option<String>,
    pub cpu_limit: f64,
    pub memory_limit_mb: i64,
    pub pids_limit: i64,
    pub disk_limit_mb: i64,
    pub idle_timeout_secs: i64,
    pub last_error: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
