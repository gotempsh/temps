// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A template being created, or apps being imported from a project's
/// repository (ADR-051), with the result of every step taken so far.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "template_instantiations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub user_id: i32,
    /// Chosen by the client; repeating a request with the same key returns
    /// this instantiation instead of starting another.
    pub idempotency_key: String,
    /// `template` or `import`.
    pub kind: String,
    pub template_slug: Option<String>,
    /// What is being created, for display.
    pub name: String,
    /// The project the services go into, once known.
    pub group_id: Option<i32>,
    /// `pending`, `running`, `succeeded` or `failed`.
    pub status: String,
    /// Inputs and per-step results; never contains secret values.
    #[sea_orm(column_type = "JsonBinary")]
    pub plan: Json,
    /// Encrypted variable values the remaining steps need; cleared on
    /// success.
    #[serde(skip_serializing)]
    pub secrets: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// When a runner atomically claimed this instantiation. Set only while
    /// `status = 'running'`; used to detect abandoned runs on restart.
    pub claimed_at: Option<DateTime<Utc>>,
    /// Last heartbeat from the running engine; updated every 30 s. When
    /// `status = 'running'` and this is more than 2 minutes old the
    /// instantiation is reported as `interrupted`.
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Random value written by every claim of this instantiation, takeovers
    /// included. A runner's writes are conditional on the token it was
    /// given, so one that lost its claim cannot change the row (see
    /// `temps-project-groups`). Never exposed.
    #[serde(skip_serializing)]
    pub claim_token: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
