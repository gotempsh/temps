// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A long-running operation a chat started: a deployment, a backup or a
//! restore. Only a pointer to the record that does the work; its state is read
//! from that record.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_chat_operations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub conversation_id: i64,
    /// `deployment`, `backup` (an S3 source backup), `service_backup` (a
    /// database or other external service backup) or `restore`.
    pub kind: String,
    /// What started it: an API operation id such as `trigger_project_pipeline`,
    /// or the chat tool `temps_deploy_sandbox_source`.
    pub operation_id: String,
    /// The deployment, backup, service backup or restore run. `None` when the
    /// operation did not say which one it started (a pipeline trigger), in
    /// which case the first deployment of the environment after `created_at`
    /// is the one it started.
    pub target_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    /// The external service backed up or restored.
    pub service_id: Option<i32>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
