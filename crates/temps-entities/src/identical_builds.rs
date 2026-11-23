// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One coordinated image build (ADR 113).
//!
//! A row exists for every build of a project whose
//! `identical_build_mode` is not `parallel`. Builds of one group
//! (`queue_key`) rendezvous here: the partial unique index on `queue_key`
//! where `state = 'building'` lets exactly one of them build, the others
//! wait, and a `share` build whose `fingerprint` and `access_boundary` match
//! a recent success deploys that row's `image` instead of building. The
//! table, not process memory, is the source of truth, so coordination
//! survives a restart and spans every control-plane process.
//!
//! No value here is secret: `fingerprint` and `queue_key` are keyed hashes
//! (HMAC with a subkey of the install's encryption key) of the build's
//! inputs, never the inputs themselves.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::identical_build_mode::{IdenticalBuildMode, IdenticalBuildState};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "identical_builds")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub deployment_id: i32,
    pub project_id: i32,
    /// The build job of the deployment (`deployment_jobs.job_id`). One row
    /// per (deployment, job): a retried job takes its own row over.
    pub job_id: String,
    /// The project's mode when the build started.
    pub mode: IdenticalBuildMode,
    /// The group this build queues in: the fingerprint, or the source key
    /// when the project's scope is `source`. Prefixed with the scope, so the
    /// two never collide.
    pub queue_key: String,
    /// Keyed hash of everything that determines the image.
    pub fingerprint: String,
    /// Which teams may reach the project (`unrestricted`, or `teams:<ids>`).
    /// An image is only reused between rows with the same boundary, so
    /// whoever sees the reusing deployment could already see the build.
    pub access_boundary: String,
    pub state: IdenticalBuildState,
    /// For a `reused` row, the build whose image it deployed.
    pub source_build_id: Option<i32>,
    /// For a `succeeded` share build, the image it produced (the build job's
    /// outputs that locate it: tag, id, the node that holds it).
    pub image: Option<Json>,
    /// `owner/name` of the repository, for log lines.
    pub repository: String,
    pub commit_sha: String,
    /// Why the row ended as it did (failure, cancellation, take-over).
    pub detail: Option<String>,
    /// Refreshed while the row is `waiting` or `building`; a building row
    /// whose heartbeat stops is taken over.
    pub heartbeat_at: DBDateTime,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Deployment,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
    }
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
