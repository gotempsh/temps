// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A function (ADR-055 Phase 10): an uploaded artifact that runs as a
//! scale-to-zero workload on the workload plane and is reached on its own
//! hostname through the activator.
//!
//! `artifact_kind` is `wasm` (the module is stored by `wasm_digest` under
//! the data directory) or `oci` (`image_reference`). `workload_id` is set
//! once the plane accepted the function's workload.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "functions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    pub name: String,
    /// `wasm` or `oci`.
    pub artifact_kind: String,
    pub image_reference: Option<String>,
    /// `sha256:<hex>` of the stored module.
    pub wasm_digest: Option<String>,
    pub wasm_size_bytes: Option<i64>,
    /// The port the function serves HTTP on inside its instance.
    pub port: i32,
    /// Seconds without a request before the activator puts it to sleep.
    pub idle_timeout_seconds: i32,
    /// Seconds a wake may take before waiting requests are failed.
    pub wake_timeout_seconds: i32,
    pub memory_limit_mb: Option<i32>,
    /// The hostname the proxy routes to the function (lowercase, no port).
    pub hostname: String,
    pub workload_id: Option<i64>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
