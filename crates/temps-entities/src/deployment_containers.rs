// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr, FromJsonQueryResult};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

// Eq dropped because cpu_limit_cores is f64 (NaN-safe equality isn't a thing).
// PartialEq is enough for the few diff checks the monitoring loop does.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "deployment_containers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub deployment_id: i32,
    pub container_id: String,
    pub container_name: String,
    pub container_port: i32,
    pub host_port: Option<i32>,
    pub image_name: Option<String>,
    pub status: Option<String>,
    /// Compose service name (e.g., "web", "redis"). NULL for single-container deployments.
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub service_name: Option<String>,
    pub created_at: DBDateTime,
    pub deployed_at: DBDateTime,
    pub ready_at: Option<DBDateTime>,
    pub deleted_at: Option<DBDateTime>,
    /// Node this container runs on. NULL = local node (single-node mode).
    pub node_id: Option<i32>,
    /// Process exit code from Docker (NULL if still running or never inspected post-exit).
    pub exit_code: Option<i32>,
    /// Human-readable reason the container exited, e.g. "OOMKilled",
    /// "Signal SIGKILL (9)", "Exit code 137". NULL if still running.
    #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
    pub exit_reason: Option<String>,
    /// True when Docker reported the container was killed by the OOM killer.
    pub oom_killed: Option<bool>,
    /// Free-form error string captured from Docker's container state on exit.
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,
    /// When the container exited (FinishedAt from Docker inspect).
    pub finished_at: Option<DBDateTime>,
    /// When the container's main process most recently started (Docker's
    /// StartedAt). Distinct from `created_at` because the container may
    /// restart in place after a crash.
    pub started_at: Option<DBDateTime>,
    /// CPU limit applied to the container, in whole cores (e.g. 1.0 = 1 vCPU).
    /// NULL if no limit is configured.
    pub cpu_limit_cores: Option<f64>,
    /// Every TCP port Docker published for this container. `container_port`
    /// and `host_port` keep the primary mapping; this lets a Compose service
    /// expose several public ports, each routed through its own live host
    /// mapping. NULL on rows written before the column existed.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub port_bindings: Option<ContainerPortBindings>,
}

/// A published TCP port: the container-side target and the host port Docker
/// mapped it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPortBinding {
    pub container_port: i32,
    pub host_port: i32,
}

/// JSON list stored in `deployment_containers.port_bindings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
#[serde(transparent)]
pub struct ContainerPortBindings(pub Vec<ContainerPortBinding>);

impl Model {
    /// Host port Docker published for `container_port`, if any.
    ///
    /// Falls back to the legacy single mapping for rows written before
    /// `port_bindings` existed.
    pub fn host_port_for(&self, container_port: i32) -> Option<i32> {
        if let Some(bindings) = &self.port_bindings {
            return bindings
                .0
                .iter()
                .find(|binding| binding.container_port == container_port)
                .map(|binding| binding.host_port);
        }
        (self.container_port == container_port)
            .then_some(self.host_port)
            .flatten()
    }

    /// Whether the container exposes `container_port`, either as its primary
    /// target or as one of its published bindings.
    pub fn exposes_port(&self, container_port: i32) -> bool {
        self.container_port == container_port
            || self.port_bindings.as_ref().is_some_and(|bindings| {
                bindings
                    .0
                    .iter()
                    .any(|binding| binding.container_port == container_port)
            })
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id"
    )]
    Deployment,
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id"
    )]
    Node,
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployment.def()
    }
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();

        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }

        Ok(self)
    }
}
