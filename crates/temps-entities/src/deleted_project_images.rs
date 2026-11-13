// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Projects whose rows were deleted and whose built images may still be on
//! a runtime.
//!
//! A row is written by a trigger on `projects` in the same transaction as
//! the delete, and removed by the nightly sweep of images of deleted
//! projects once no runtime holds an image labelled with the project. The
//! sweep asks each runtime for one such project's images at a time, so a
//! Docker daemon's answer, which cannot be limited, stays one project's
//! leftovers.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "deleted_project_images")]
pub struct Model {
    /// The deleted project's ID, as its images' `sh.temps.image.project_id`
    /// label carries it. No foreign key: the project row is gone.
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_id: i32,

    /// When the project row was deleted.
    pub deleted_at: DBDateTime,

    /// When the sweep last looked for its images; `None` until the first
    /// look. The least recently checked rows are looked at first.
    pub checked_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
