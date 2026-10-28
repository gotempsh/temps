// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Static placement for Docker Compose stacks.
//!
//! A Compose stack keeps its data where it runs: named volumes live in that
//! host's Docker daemon and relative bind mounts in that host's data
//! directory. Scheduling every deployment afresh, the way single-container
//! replicas are, would therefore start a stack with empty volumes whenever
//! the scheduler picked a different node. Instead:
//!
//! 1. The first deployment of an environment chooses a home from its Target
//!    Nodes and Label Selectors (the same scheduler single containers use).
//!    Unconstrained, it stays on the control plane, as Compose always has.
//! 2. When that deployment reaches `docker compose up` — the first moment
//!    data can exist — the home is pinned in `compose_placements`.
//! 3. Every later deployment goes back to the pinned home, whatever Target
//!    Nodes now say. A mismatch is reported, never acted on.
//! 4. Moving the stack is an explicit request ([`ComposePlacementService::request_move`]).
//!    The next deployment stops the stack on the old host (`docker compose
//!    down`, which keeps volumes) and starts it on the new one, empty. The old
//!    host's data stays where it is.
//! 5. A home node that is offline is never replaced automatically.

use std::sync::Arc;

use sea_orm::{
    sea_query::OnConflict, ActiveValue::Set, ColumnTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter,
};
use temps_entities::{compose_placements, environments, nodes, projects};

use super::node_scheduler::{NodeAssignment, NodeScheduler, ReplicaPlacementRequest};
use super::CONTROL_PLANE_NODE_ID;

#[derive(Debug, thiserror::Error)]
pub enum ComposePlacementError {
    #[error("Database error while {operation} for Compose environment {environment_id}: {source}")]
    Database {
        environment_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error(
        "The Compose stack of environment '{environment_name}' (id {environment_id}) lives on \
         worker node '{node_name}' (id {node_id}), which is {state}. Its volumes are on that \
         node, so Temps will not start the stack anywhere else on its own: bring '{node_name}' \
         back online, or move the stack to another node in the environment's Node scheduling \
         settings (its data stays behind on '{node_name}')"
    )]
    HomeNodeUnavailable {
        environment_id: i32,
        environment_name: String,
        node_id: i32,
        node_name: String,
        state: String,
    },

    #[error(
        "No node can take the first deployment of Compose environment '{environment_name}' \
         (id {environment_id}): {reason}"
    )]
    NoPlacement {
        environment_id: i32,
        environment_name: String,
        reason: String,
    },

    #[error(
        "Compose environment {environment_id} was pinned to {pinned} by another deployment while \
         this one was starting on {expected}; redeploy so it goes to {pinned}"
    )]
    PinnedConcurrently {
        environment_id: i32,
        pinned: String,
        expected: String,
    },

    #[error(
        "The Compose stack of environment {environment_id} was moved while deployment \
         {deployment_id} was running: it planned to run on {expected} but the stack now \
         belongs on {current}. Nothing was started; redeploy to run it on {current}"
    )]
    MovedConcurrently {
        environment_id: i32,
        deployment_id: i32,
        expected: String,
        current: String,
    },

    #[error("Cannot move the Compose stack of environment {environment_id}: {reason}")]
    InvalidMove { environment_id: i32, reason: String },

    #[error("Node {node_id} not found while moving Compose environment {environment_id}")]
    NodeNotFound { environment_id: i32, node_id: i32 },

    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error(
        "Project {project_id} is not a Docker Compose project; only Compose stacks are pinned \
         to a node"
    )]
    NotComposeProject { project_id: i32 },

    #[error(
        "Moving the Compose stack of environment {environment_id} off {home} starts it on the \
         new node with empty volumes; the data on {home} stays there and is not copied. Set \
         acknowledge_data_loss to true to move it"
    )]
    DataLossNotAcknowledged { environment_id: i32, home: String },
}

/// Where a Compose environment's stack lives, for the console.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct ComposePlacementResponse {
    pub project_id: i32,
    pub environment_id: i32,
    /// Whether the stack has a home yet. `false` until its first deployment
    /// reaches `docker compose up` (or a move chose one).
    pub pinned: bool,
    /// The home node: `0` is the control plane. `None` when not pinned.
    pub home_node_id: Option<i32>,
    pub home_node_name: Option<String>,
    /// The home's state (`active`, `offline`, `draining`, ...). Deployments
    /// fail rather than move while the home is not active.
    pub home_node_status: Option<String>,
    /// When the stack was pinned to its home, ISO 8601.
    pub pinned_at: Option<chrono::DateTime<chrono::Utc>>,
    /// A move is pending: the node the next deployment stops the stack on
    /// (`0` is the control plane).
    pub moving_from_node_id: Option<i32>,
    pub moving_from_node_name: Option<String>,
    /// Why the environment's Target Nodes / Label Selectors no longer
    /// describe the home, when they do not.
    pub drift: Option<String>,
}

/// Where the next deployment of a Compose environment goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposePlacementPlan {
    /// The host to deploy to: `None` is the control plane.
    pub home: Option<i32>,
    /// The home's name, for the deployment log.
    pub home_name: String,
    /// Whether the home is already pinned. When not, the deployment pins it
    /// once it reaches `docker compose up`.
    pub pinned: bool,
    /// A move is pending: the host to stop first (`Some(None)` is the control
    /// plane).
    pub vacate: Option<Option<i32>>,
    /// Target Nodes / Label Selectors no longer describe the home.
    pub drift: Option<String>,
}

fn node_label(id: Option<i32>, name: &str) -> String {
    match id {
        None => "the control plane".to_string(),
        Some(id) => format!("worker node '{name}' (id {id})"),
    }
}

/// Why `target_nodes`/`target_labels` no longer describe a pinned home, or
/// `None` when they still do (or say nothing).
pub fn placement_drift(
    home: Option<i32>,
    home_name: &str,
    home_labels: Option<&serde_json::Value>,
    target_nodes: Option<&[i32]>,
    target_labels: Option<&serde_json::Value>,
) -> Option<String> {
    let home_id = home.unwrap_or(CONTROL_PLANE_NODE_ID);
    let mut reasons = Vec::new();
    if let Some(ids) = super::node_scheduler::placement_node_ids(target_nodes) {
        if !ids.contains(&home_id) {
            reasons.push(format!(
                "Target Nodes [{}] do not include it",
                ids.iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(selectors) = target_labels
        .and_then(serde_json::Value::as_object)
        .filter(|selectors| !selectors.is_empty())
    {
        let labels = home_labels.and_then(serde_json::Value::as_object);
        let unmatched: Vec<String> = selectors
            .iter()
            // The scheduler's rule, so drift never disagrees with where a
            // first deployment would have been placed.
            .filter(|(key, value)| {
                !labels
                    .and_then(|labels| labels.get(*key))
                    .is_some_and(|label| super::node_scheduler::label_value_matches(label, value))
            })
            .map(|(key, value)| match value.as_str() {
                Some(text) => format!("{key}={text}"),
                None => format!("{key}={value}"),
            })
            .collect();
        if !unmatched.is_empty() {
            reasons.push(format!(
                "it does not match the Label Selectors {}",
                unmatched.join(", ")
            ));
        }
    }
    (!reasons.is_empty()).then(|| {
        format!(
            "This Compose stack lives on {}, but {}. It stays there because its volumes are \
             there; move it from the environment's Node scheduling settings to change nodes \
             (the data stays behind)",
            node_label(home, home_name),
            reasons.join(" and ")
        )
    })
}

/// Compose placement reads and writes.
pub struct ComposePlacementService {
    db: Arc<DatabaseConnection>,
}

impl ComposePlacementService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get(
        &self,
        environment_id: i32,
    ) -> Result<Option<compose_placements::Model>, ComposePlacementError> {
        compose_placements::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ComposePlacementError::Database {
                environment_id,
                operation: "reading the Compose placement",
                source,
            })
    }

    async fn node(
        &self,
        environment_id: i32,
        node_id: i32,
    ) -> Result<Option<nodes::Model>, ComposePlacementError> {
        nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| ComposePlacementError::Database {
                environment_id,
                operation: "reading the home node",
                source,
            })
    }

    /// Decide where the next deployment of `environment` goes.
    pub async fn plan(
        &self,
        scheduler: Option<&NodeScheduler>,
        project: &projects::Model,
        environment: &environments::Model,
        target_nodes: Option<&[i32]>,
        target_labels: Option<&serde_json::Value>,
    ) -> Result<ComposePlacementPlan, ComposePlacementError> {
        if let Some(placement) = self.get(environment.id).await? {
            let (home_name, home_labels) = match placement.node_id {
                None => ("control-plane".to_string(), None),
                Some(node_id) => {
                    let node = self.node(environment.id, node_id).await?;
                    let Some(node) = node else {
                        // `ON DELETE RESTRICT` keeps this from happening; say
                        // so plainly rather than pick another host.
                        return Err(ComposePlacementError::HomeNodeUnavailable {
                            environment_id: environment.id,
                            environment_name: environment.name.clone(),
                            node_id,
                            node_name: format!("#{node_id}"),
                            state: "no longer registered".to_string(),
                        });
                    };
                    let active = match scheduler {
                        Some(scheduler) => scheduler
                            .active_node(node_id)
                            .await
                            .map_err(|error| ComposePlacementError::NoPlacement {
                                environment_id: environment.id,
                                environment_name: environment.name.clone(),
                                reason: error.to_string(),
                            })?
                            .is_some(),
                        None => false,
                    };
                    if !active {
                        return Err(ComposePlacementError::HomeNodeUnavailable {
                            environment_id: environment.id,
                            environment_name: environment.name.clone(),
                            node_id,
                            node_name: node.name.clone(),
                            state: if node.status == "active" {
                                "not sending heartbeats".to_string()
                            } else {
                                node.status.clone()
                            },
                        });
                    }
                    (node.name.clone(), Some(node.labels.clone()))
                }
            };
            let drift = placement_drift(
                placement.node_id,
                &home_name,
                home_labels.as_ref(),
                target_nodes,
                target_labels,
            );
            return Ok(ComposePlacementPlan {
                home: placement.node_id,
                home_name,
                pinned: true,
                vacate: placement.pending_vacate(),
                drift,
            });
        }

        // First deployment: nothing is pinned yet.
        let constrained = super::node_scheduler::placement_node_ids(target_nodes).is_some()
            || target_labels
                .and_then(serde_json::Value::as_object)
                .is_some_and(|labels| !labels.is_empty());
        let control_plane = ComposePlacementPlan {
            home: None,
            home_name: "control-plane".to_string(),
            pinned: false,
            vacate: None,
            drift: None,
        };
        let Some(scheduler) = scheduler else {
            return Ok(control_plane);
        };
        if !constrained && scheduler.local_workloads_enabled() {
            return Ok(control_plane);
        }
        let outcome = scheduler
            .schedule_placement(ReplicaPlacementRequest {
                replica_count: 1,
                labels: target_labels,
                target_node_ids: target_nodes,
                anti_affinity: false,
                exclude_node_ids: &[],
                image_platforms: &[],
                project_slug: Some(&project.slug),
                exclude_control_plane: false,
            })
            .await
            .map_err(|error| ComposePlacementError::NoPlacement {
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                reason: error.to_string(),
            })?;
        match outcome.assignments.into_iter().next() {
            Some(NodeAssignment::Local) => Ok(control_plane),
            Some(NodeAssignment::Remote {
                node_id, node_name, ..
            }) => Ok(ComposePlacementPlan {
                home: Some(node_id),
                home_name: node_name,
                pinned: false,
                vacate: None,
                drift: None,
            }),
            None => Err(ComposePlacementError::NoPlacement {
                environment_id: environment.id,
                environment_name: environment.name.clone(),
                reason: "the scheduler returned no node".to_string(),
            }),
        }
    }

    /// Pin `environment_id` to `node_id` unless it is already pinned. Fails
    /// when a concurrent deployment pinned it somewhere else first.
    pub async fn pin(
        &self,
        environment_id: i32,
        node_id: Option<i32>,
    ) -> Result<(), ComposePlacementError> {
        let now = chrono::Utc::now();
        compose_placements::Entity::insert(compose_placements::ActiveModel {
            environment_id: Set(environment_id),
            node_id: Set(node_id),
            pinned_at: Set(now),
            vacate_pending: Set(false),
            vacate_node_id: Set(None),
            updated_at: Set(now),
        })
        .on_conflict(
            OnConflict::column(compose_placements::Column::EnvironmentId)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(self.db.as_ref())
        .await
        .map_err(|source| ComposePlacementError::Database {
            environment_id,
            operation: "pinning the Compose stack",
            source,
        })?;
        let pinned = self.get(environment_id).await?;
        match pinned {
            Some(placement) if placement.node_id == node_id => Ok(()),
            Some(placement) => Err(ComposePlacementError::PinnedConcurrently {
                environment_id,
                pinned: node_label(
                    placement.node_id,
                    &format!("#{}", placement.node_id.unwrap_or(0)),
                ),
                expected: node_label(node_id, &format!("#{}", node_id.unwrap_or(0))),
            }),
            None => Err(ComposePlacementError::Database {
                environment_id,
                operation: "reading back the pinned Compose stack",
                source: DbErr::RecordNotFound(format!(
                    "compose_placements row for environment {environment_id}"
                )),
            }),
        }
    }

    /// Fail unless the stack still has the placement deployment
    /// `deployment_id` planned against: home `home` and pending vacate
    /// `vacate`. A move requested after planning makes that deployment
    /// stale; running it would start the stack on a host the placement no
    /// longer names, where no later deployment would stop it.
    pub async fn expect_placement(
        &self,
        environment_id: i32,
        deployment_id: i32,
        home: Option<i32>,
        vacate: Option<Option<i32>>,
    ) -> Result<(), ComposePlacementError> {
        let current = self.get(environment_id).await?;
        let current = current
            .as_ref()
            .map(|placement| (placement.node_id, placement.pending_vacate()));
        if current == Some((home, vacate)) {
            return Ok(());
        }
        Err(moved_concurrently(
            environment_id,
            deployment_id,
            (home, vacate),
            current,
        ))
    }

    /// Record that `vacated` no longer runs the stack, but only while the
    /// placement is still the move this deployment carried out (home `home`,
    /// pending vacate `vacated`). A move requested meanwhile keeps its own
    /// pending vacate and this deployment fails with `MovedConcurrently`, so
    /// the stack it would have started never runs somewhere the placement
    /// no longer names.
    pub async fn complete_vacate(
        &self,
        environment_id: i32,
        deployment_id: i32,
        home: Option<i32>,
        vacated: Option<i32>,
    ) -> Result<(), ComposePlacementError> {
        let node_matches = |column: compose_placements::Column, node_id: Option<i32>| match node_id
        {
            Some(node_id) => column.eq(node_id),
            None => column.is_null(),
        };
        let result = compose_placements::Entity::update_many()
            .col_expr(
                compose_placements::Column::VacatePending,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                compose_placements::Column::VacateNodeId,
                sea_orm::sea_query::Expr::value(Option::<i32>::None),
            )
            .col_expr(
                compose_placements::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(compose_placements::Column::EnvironmentId.eq(environment_id))
            .filter(node_matches(compose_placements::Column::NodeId, home))
            .filter(compose_placements::Column::VacatePending.eq(true))
            .filter(node_matches(
                compose_placements::Column::VacateNodeId,
                vacated,
            ))
            .exec(self.db.as_ref())
            .await
            .map_err(|source| ComposePlacementError::Database {
                environment_id,
                operation: "recording that the old host was stopped",
                source,
            })?;
        if result.rows_affected > 0 {
            return Ok(());
        }
        let current = self.get(environment_id).await?;
        Err(moved_concurrently(
            environment_id,
            deployment_id,
            (home, Some(vacated)),
            current
                .as_ref()
                .map(|placement| (placement.node_id, placement.pending_vacate())),
        ))
    }

    /// Move a pinned stack to `target` (`None` is the control plane). The
    /// next deployment stops it on the host running it and starts it, with
    /// empty volumes, on `target`. Returns the new placement and the one it
    /// replaced.
    ///
    /// An unpinned environment has no data anywhere, so a "move" simply sets
    /// where its first deployment goes. Moving to the current home is
    /// refused, and a move while another is pending keeps the original old
    /// host, which is the one still running the stack. Moving a pinned stack
    /// off the host running it needs `acknowledge_data_loss`; moving it back
    /// there cancels the pending move and does not.
    ///
    /// The placement is read and written under a row lock, so a deployment
    /// recording its vacate ([`Self::complete_vacate`]) either finishes
    /// before this read or waits and then sees the move: the acknowledgement
    /// is always decided against the host that is actually running the stack,
    /// and a move never restores a vacate a deployment already completed.
    pub async fn request_move(
        &self,
        environment_id: i32,
        target: Option<i32>,
        acknowledge_data_loss: bool,
    ) -> Result<(compose_placements::Model, Option<compose_placements::Model>), ComposePlacementError>
    {
        if let Some(node_id) = target {
            let node = self.node(environment_id, node_id).await?.ok_or(
                ComposePlacementError::NodeNotFound {
                    environment_id,
                    node_id,
                },
            )?;
            if super::node_scheduler::is_build_only_node(&node) {
                return Err(ComposePlacementError::InvalidMove {
                    environment_id,
                    reason: format!(
                        "worker node '{}' (id {node_id}) is a build-only node and never runs \
                         application containers",
                        node.name
                    ),
                });
            }
        }
        // A first deployment may pin the stack between the locked read
        // finding no row and the insert; that insert then does nothing and
        // the move is decided again against the pinned row.
        for _ in 0..3 {
            match self
                .request_move_once(environment_id, target, acknowledge_data_loss)
                .await?
            {
                MoveAttempt::Moved {
                    placement,
                    previous,
                } => return Ok((placement, previous)),
                MoveAttempt::DataLossNotAcknowledged { running } => {
                    let (_, name, _, _) = self.describe_node(environment_id, running).await?;
                    return Err(ComposePlacementError::DataLossNotAcknowledged {
                        environment_id,
                        home: node_label(running, &name),
                    });
                }
                MoveAttempt::PinnedMeanwhile => {}
            }
        }
        Err(ComposePlacementError::InvalidMove {
            environment_id,
            reason: "its placement kept changing while the move was being recorded; retry"
                .to_string(),
        })
    }

    async fn request_move_once(
        &self,
        environment_id: i32,
        target: Option<i32>,
        acknowledge_data_loss: bool,
    ) -> Result<MoveAttempt, ComposePlacementError> {
        use sea_orm::{ActiveModelTrait, QuerySelect, TransactionTrait};
        let database = |operation, source| ComposePlacementError::Database {
            environment_id,
            operation,
            source,
        };
        let now = chrono::Utc::now();
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| database("starting the Compose stack move", source))?;
        let current = compose_placements::Entity::find_by_id(environment_id)
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(|source| database("locking the Compose stack placement", source))?;
        let Some(current) = current else {
            let inserted = compose_placements::Entity::insert(compose_placements::ActiveModel {
                environment_id: Set(environment_id),
                node_id: Set(target),
                pinned_at: Set(now),
                vacate_pending: Set(false),
                vacate_node_id: Set(None),
                updated_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(compose_placements::Column::EnvironmentId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(&txn)
            .await
            .map_err(|source| database("choosing where the Compose stack starts", source))?;
            if inserted == 0 {
                return Ok(MoveAttempt::PinnedMeanwhile);
            }
            let placement = compose_placements::Entity::find_by_id(environment_id)
                .one(&txn)
                .await
                .map_err(|source| database("reading back the moved Compose stack", source))?
                .ok_or_else(|| {
                    database(
                        "reading back the moved Compose stack",
                        DbErr::RecordNotFound(format!(
                            "compose_placements row for environment {environment_id}"
                        )),
                    )
                })?;
            txn.commit()
                .await
                .map_err(|source| database("committing the Compose stack move", source))?;
            return Ok(MoveAttempt::Moved {
                placement,
                previous: None,
            });
        };
        if current.node_id == target {
            return Err(ComposePlacementError::InvalidMove {
                environment_id,
                reason: format!(
                    "the stack already lives on {}",
                    node_label(target, &format!("#{}", target.unwrap_or(0)))
                ),
            });
        }
        // Until the next deployment completes a pending move, the stack (and
        // the data it uses) is still on the host it is leaving.
        let running = current.pending_vacate().unwrap_or(current.node_id);
        if running != target && !acknowledge_data_loss {
            return Ok(MoveAttempt::DataLossNotAcknowledged { running });
        }
        let vacate = (running != target).then_some(running);
        let placement = compose_placements::ActiveModel {
            environment_id: Set(environment_id),
            node_id: Set(target),
            pinned_at: Set(now),
            vacate_pending: Set(vacate.is_some()),
            vacate_node_id: Set(vacate.flatten()),
            updated_at: Set(now),
        }
        .update(&txn)
        .await
        .map_err(|source| database("moving the Compose stack", source))?;
        txn.commit()
            .await
            .map_err(|source| database("committing the Compose stack move", source))?;
        Ok(MoveAttempt::Moved {
            placement,
            previous: Some(current),
        })
    }

    /// Load a Compose environment of `project_id`, refusing other presets.
    async fn compose_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<(projects::Model, environments::Model), ComposePlacementError> {
        let database = |operation, source| ComposePlacementError::Database {
            environment_id,
            operation,
            source,
        };
        let environment = environments::Entity::find_by_id(environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| database("reading the environment", source))?
            .filter(|environment| {
                environment.project_id == project_id && environment.deleted_at.is_none()
            })
            .ok_or(ComposePlacementError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| database("reading the project", source))?
            .ok_or(ComposePlacementError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        if project.preset != temps_entities::preset::Preset::DockerCompose {
            return Err(ComposePlacementError::NotComposeProject { project_id });
        }
        Ok((project, environment))
    }

    async fn describe_node(
        &self,
        environment_id: i32,
        node_id: Option<i32>,
    ) -> Result<(i32, String, String, Option<serde_json::Value>), ComposePlacementError> {
        match node_id {
            None => Ok((
                CONTROL_PLANE_NODE_ID,
                "control-plane".to_string(),
                "active".to_string(),
                None,
            )),
            Some(node_id) => Ok(match self.node(environment_id, node_id).await? {
                Some(node) => (node_id, node.name, node.status, Some(node.labels)),
                None => (node_id, format!("#{node_id}"), "removed".to_string(), None),
            }),
        }
    }

    /// Where the stack of a Compose environment lives, any pending move, and
    /// whether its placement settings still describe its home.
    pub async fn status(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<ComposePlacementResponse, ComposePlacementError> {
        let (project, environment) = self.compose_environment(project_id, environment_id).await?;
        let mut response = ComposePlacementResponse {
            project_id,
            environment_id,
            pinned: false,
            home_node_id: None,
            home_node_name: None,
            home_node_status: None,
            pinned_at: None,
            moving_from_node_id: None,
            moving_from_node_name: None,
            drift: None,
        };
        let Some(placement) = self.get(environment_id).await? else {
            return Ok(response);
        };
        let (home_id, home_name, home_status, home_labels) = self
            .describe_node(environment_id, placement.node_id)
            .await?;
        let configs = [
            environment.deployment_config.as_ref(),
            project.deployment_config.as_ref(),
        ];
        let target_nodes = configs
            .iter()
            .flatten()
            .find_map(|config| config.configured_target_nodes().map(<[i32]>::to_vec));
        let target_labels = configs
            .iter()
            .flatten()
            .find_map(|config| config.configured_target_labels().cloned());
        response.drift = placement_drift(
            placement.node_id,
            &home_name,
            home_labels.as_ref(),
            target_nodes.as_deref(),
            target_labels.as_ref(),
        );
        if let Some(old) = placement.pending_vacate() {
            let (old_id, old_name, _, _) = self.describe_node(environment_id, old).await?;
            response.moving_from_node_id = Some(old_id);
            response.moving_from_node_name = Some(old_name);
        }
        response.pinned = true;
        response.home_node_id = Some(home_id);
        response.home_node_name = Some(home_name);
        response.home_node_status = Some(home_status);
        response.pinned_at = Some(placement.pinned_at);
        Ok(response)
    }

    /// Move a Compose environment's stack to `target_node_id` (`0` is the
    /// control plane). The data on the host running the stack is not copied,
    /// so moving a pinned stack off it needs `acknowledge_data_loss`. Moving
    /// it back to that host while a move is pending only cancels the move
    /// and needs no acknowledgement: nothing is lost. Returns the new
    /// placement and the home it leaves.
    pub async fn move_stack(
        &self,
        project_id: i32,
        environment_id: i32,
        target_node_id: i32,
        acknowledge_data_loss: bool,
    ) -> Result<(ComposePlacementResponse, Option<i32>), ComposePlacementError> {
        self.compose_environment(project_id, environment_id).await?;
        let target = (target_node_id != CONTROL_PLANE_NODE_ID).then_some(target_node_id);
        if let Some(node_id) = target {
            let node = self.node(environment_id, node_id).await?.ok_or(
                ComposePlacementError::NodeNotFound {
                    environment_id,
                    node_id,
                },
            )?;
            if node.status != "active" {
                return Err(ComposePlacementError::InvalidMove {
                    environment_id,
                    reason: format!(
                        "worker node '{}' (id {node_id}) is {}; a stack can only move to an \
                         active node",
                        node.name, node.status
                    ),
                });
            }
            if super::node_scheduler::is_build_only_node(&node) {
                return Err(ComposePlacementError::InvalidMove {
                    environment_id,
                    reason: format!(
                        "worker node '{}' (id {node_id}) is a build-only node \
                         (temps.sh/role=builder), which never hosts workloads",
                        node.name
                    ),
                });
            }
        }
        let (_, previous) = self
            .request_move(environment_id, target, acknowledge_data_loss)
            .await?;
        let status = self.status(project_id, environment_id).await?;
        Ok((
            status,
            previous.map(|previous| previous.node_id.unwrap_or(CONTROL_PLANE_NODE_ID)),
        ))
    }

    /// Live Compose environments whose stacks live on `node_id`, or are
    /// still running there pending a move away.
    pub async fn environments_on_node(
        &self,
        node_id: i32,
    ) -> Result<Vec<compose_placements::Model>, DbErr> {
        Ok(placements_on_node(self.db.as_ref(), node_id)
            .await?
            .live
            .into_iter()
            .map(|(placement, _)| placement)
            .collect())
    }
}

/// The placements that tie a Compose stack to a node: see [`on_node`].
#[derive(Debug, Default)]
pub struct PlacementsOnNode {
    /// Stacks of live environments, each with its environment's name. Their
    /// data is on the node, so it cannot be removed until each is moved
    /// (or the node is recorded as lost).
    pub live: Vec<(compose_placements::Model, String)>,
    /// Environments that were deleted. Deleting an environment stops its
    /// stack; the row is kept only so a restored environment returns to its
    /// volumes, and never keeps a node from being removed.
    pub deleted_environment_ids: Vec<i32>,
}

/// Placements on `node_id`, split by whether their environment is live.
pub async fn placements_on_node<C: sea_orm::ConnectionTrait>(
    db: &C,
    node_id: i32,
) -> Result<PlacementsOnNode, DbErr> {
    let rows = compose_placements::Entity::find()
        .find_also_related(environments::Entity)
        .filter(on_node(node_id))
        .all(db)
        .await?;
    let mut placements = PlacementsOnNode::default();
    for (placement, environment) in rows {
        match environment {
            Some(environment) if environment.deleted_at.is_none() => {
                placements.live.push((placement, environment.name));
            }
            _ => placements
                .deleted_environment_ids
                .push(placement.environment_id),
        }
    }
    Ok(placements)
}

/// Detach the placements of `environment_ids` from `node_id`, which is being
/// removed: a home there is forgotten (its data goes with the node, and
/// `node_id` is ON DELETE RESTRICT) and a pending vacate of it is dropped.
/// Called for deleted environments, and for live ones only when the node is
/// recorded as lost.
pub async fn release_placements_on_node<C: sea_orm::ConnectionTrait>(
    db: &C,
    node_id: i32,
    environment_ids: &[i32],
) -> Result<(), DbErr> {
    if environment_ids.is_empty() {
        return Ok(());
    }
    let of_these = compose_placements::Column::EnvironmentId.is_in(environment_ids.to_vec());
    compose_placements::Entity::delete_many()
        .filter(of_these.clone())
        .filter(compose_placements::Column::NodeId.eq(node_id))
        .exec(db)
        .await?;
    compose_placements::Entity::update_many()
        .col_expr(
            compose_placements::Column::VacatePending,
            sea_orm::sea_query::Expr::value(false),
        )
        .col_expr(
            compose_placements::Column::VacateNodeId,
            sea_orm::sea_query::Expr::value(Option::<i32>::None),
        )
        .col_expr(
            compose_placements::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now()),
        )
        .filter(of_these)
        .filter(compose_placements::Column::VacateNodeId.eq(node_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Placements whose stack lives on `node_id` or is still running there
/// pending a move away.
pub fn on_node(node_id: i32) -> sea_orm::Condition {
    sea_orm::Condition::any()
        .add(compose_placements::Column::NodeId.eq(node_id))
        .add(
            sea_orm::Condition::all()
                .add(compose_placements::Column::VacatePending.eq(true))
                .add(compose_placements::Column::VacateNodeId.eq(node_id)),
        )
}

/// One locked attempt at recording a move.
enum MoveAttempt {
    Moved {
        placement: compose_placements::Model,
        previous: Option<compose_placements::Model>,
    },
    /// Moving off `running`, the host running the stack, needs the
    /// acknowledgement.
    DataLossNotAcknowledged { running: Option<i32> },
    /// A first deployment pinned the stack after the locked read found none.
    PinnedMeanwhile,
}

/// A home and pending vacate, as `compose_placements` stores them.
type PlacementState = (Option<i32>, Option<Option<i32>>);

fn moved_concurrently(
    environment_id: i32,
    deployment_id: i32,
    expected: PlacementState,
    current: Option<PlacementState>,
) -> ComposePlacementError {
    ComposePlacementError::MovedConcurrently {
        environment_id,
        deployment_id,
        expected: placement_label(expected.0, expected.1),
        current: match current {
            Some((home, vacate)) => placement_label(home, vacate),
            None => "no node (the placement was removed)".to_string(),
        },
    }
}

/// "worker node #3" / "the control plane", plus a pending move's old host.
fn placement_label(home: Option<i32>, vacate: Option<Option<i32>>) -> String {
    let node = |node_id: Option<i32>| match node_id {
        Some(node_id) => format!("worker node #{node_id}"),
        None => "the control plane".to_string(),
    };
    match vacate {
        Some(old) => format!("{} (moving from {})", node(home), node(old)),
        None => node(home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;

    #[test]
    fn a_home_inside_its_constraints_has_no_drift() {
        assert_eq!(
            placement_drift(None, "control-plane", None, None, None),
            None
        );
        assert_eq!(
            placement_drift(None, "control-plane", None, Some(&[0]), None),
            None
        );
        assert_eq!(
            placement_drift(Some(3), "w", None, Some(&[3, 4]), None),
            None
        );
        let labels = serde_json::json!({ "region": "eu", "disk": "ssd" });
        let selector = serde_json::json!({ "region": "eu" });
        assert_eq!(
            placement_drift(Some(3), "w", Some(&labels), None, Some(&selector)),
            None
        );
    }

    /// Drift reads Label Selectors with the scheduler's rules: an array
    /// matches any of its values, and a non-string value compares with a
    /// string label. A home the scheduler would pick is never drift.
    #[test]
    fn label_selectors_are_matched_as_the_scheduler_matches_them() {
        let labels = serde_json::json!({ "region": "eu", "tier": "2" });
        for selector in [
            serde_json::json!({ "region": ["us", "eu"] }),
            serde_json::json!({ "tier": 2 }),
            serde_json::json!({ "region": ["eu"], "tier": ["1", "2"] }),
        ] {
            assert_eq!(
                placement_drift(Some(3), "w", Some(&labels), None, Some(&selector)),
                None,
                "{selector}"
            );
        }
        let drift = placement_drift(
            Some(3),
            "w",
            Some(&labels),
            None,
            Some(&serde_json::json!({ "region": ["us", "ap"] })),
        )
        .unwrap();
        assert!(drift.contains("region="), "{drift}");
    }

    /// A reported situation: a stack that has always run on the control
    /// plane, with Target Nodes now naming a worker. It stays, and says why.
    #[test]
    fn a_home_outside_its_constraints_reports_drift_with_the_fix() {
        let drift = placement_drift(None, "control-plane", None, Some(&[5]), None).unwrap();
        assert!(drift.contains("lives on the control plane"), "{drift}");
        assert!(drift.contains("Target Nodes [5]"), "{drift}");
        assert!(drift.contains("move it"), "{drift}");
        assert!(drift.contains("data stays behind"), "{drift}");

        let labels = serde_json::json!({ "region": "us" });
        let selector = serde_json::json!({ "region": "eu" });
        let drift = placement_drift(
            Some(3),
            "worker-a",
            Some(&labels),
            Some(&[3]),
            Some(&selector),
        )
        .unwrap();
        assert!(drift.contains("worker node 'worker-a' (id 3)"), "{drift}");
        assert!(drift.contains("region=eu"), "{drift}");
        assert!(!drift.contains("Target Nodes"), "{drift}");
    }

    #[test]
    fn an_unavailable_home_names_the_node_and_both_remedies() {
        let error = ComposePlacementError::HomeNodeUnavailable {
            environment_id: 4,
            environment_name: "production".to_string(),
            node_id: 3,
            node_name: "worker-a".to_string(),
            state: "offline".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("'production' (id 4)"), "{message}");
        assert!(
            message.contains("'worker-a' (id 3), which is offline"),
            "{message}"
        );
        assert!(
            message.contains("bring 'worker-a' back online"),
            "{message}"
        );
        assert!(message.contains("move the stack"), "{message}");
    }

    async fn seed(
        db: &Arc<DatabaseConnection>,
    ) -> Result<(projects::Model, environments::Model, nodes::Model), Box<dyn std::error::Error>>
    {
        use sea_orm::ActiveModelTrait;
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set("Stack".to_string()),
            slug: Set("stack".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            repo_owner: Set("example".to_string()),
            repo_name: Set("stack".to_string()),
            preset: Set(temps_entities::preset::Preset::DockerCompose),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("stack-production".to_string()),
            host: Set(String::new()),
            upstreams: Set(Default::default()),
            project_id: Set(project.id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let node = nodes::ActiveModel {
            name: Set("worker-a".to_string()),
            token_hash: Set("hash-worker-a".to_string()),
            address: Set("http://127.0.0.1:9".to_string()),
            private_address: Set("10.20.0.5".to_string()),
            role: Set("worker".to_string()),
            status: Set("active".to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            last_heartbeat: Set(Some(now)),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        Ok((project, environment, node))
    }

    /// Pinning is first-writer-wins, and a later pin elsewhere is refused
    /// rather than silently moving the stack.
    #[tokio::test]
    async fn the_first_pin_wins_and_a_conflicting_pin_is_refused(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (_, environment, node) = seed(&db).await?;
        let service = ComposePlacementService::new(db.clone());

        service.pin(environment.id, Some(node.id)).await?;
        // Re-pinning the same home is a no-op.
        service.pin(environment.id, Some(node.id)).await?;
        let error = service.pin(environment.id, None).await.unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::PinnedConcurrently { .. }),
            "{error}"
        );
        assert_eq!(
            service.get(environment.id).await?.and_then(|p| p.node_id),
            Some(node.id)
        );
        Ok(())
    }

    /// A move records the host still running the stack, survives a second
    /// move before any deployment, and cancels when moved back.
    #[tokio::test]
    async fn moves_remember_the_host_that_still_runs_the_stack(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (_, environment, node) = seed(&db).await?;
        let service = ComposePlacementService::new(db.clone());

        // Unpinned: a move only chooses where the first deployment goes.
        let placement = service
            .request_move(environment.id, Some(node.id), true)
            .await?
            .0;
        assert_eq!(placement.node_id, Some(node.id));
        assert_eq!(placement.pending_vacate(), None);

        // Pinned on the worker, moved to the control plane.
        let placement = service.request_move(environment.id, None, true).await?.0;
        assert_eq!(placement.node_id, None);
        assert_eq!(placement.pending_vacate(), Some(Some(node.id)));

        // Moving again before deploying keeps the original old host.
        let missing = service.request_move(environment.id, Some(9999), true).await;
        assert!(matches!(
            missing,
            Err(ComposePlacementError::NodeNotFound { node_id: 9999, .. })
        ));

        // Moving back to the host that still runs it cancels the move.
        let placement = service
            .request_move(environment.id, Some(node.id), true)
            .await?
            .0;
        assert_eq!(placement.node_id, Some(node.id));
        assert_eq!(placement.pending_vacate(), None);

        // Moving to the current home is refused.
        let error = service
            .request_move(environment.id, Some(node.id), true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already lives on"), "{error}");

        // A stack moving away still runs on the node until it is vacated.
        service.request_move(environment.id, None, true).await?;
        assert_eq!(service.environments_on_node(node.id).await?.len(), 1);

        // A completed vacate clears the pending move.
        service
            .complete_vacate(environment.id, 1, None, Some(node.id))
            .await?;
        assert_eq!(
            service
                .get(environment.id)
                .await?
                .and_then(|p| p.pending_vacate()),
            None
        );

        // The node now holds no stack; the control plane does.
        assert!(service.environments_on_node(node.id).await?.is_empty());
        Ok(())
    }

    /// A deployment planned before a later move must not start the stack, or
    /// clear the later move's pending vacate: the stack would then run on a
    /// host no deployment ever stops.
    #[tokio::test]
    async fn a_deployment_planned_before_a_later_move_is_refused(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::ActiveModelTrait;
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (_, environment, worker_a) = seed(&db).await?;
        let worker_b = nodes::ActiveModel {
            name: Set("worker-b".to_string()),
            token_hash: Set("hash-worker-b".to_string()),
            address: Set("http://127.0.0.1:10".to_string()),
            private_address: Set("10.20.0.6".to_string()),
            role: Set("worker".to_string()),
            status: Set("active".to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            last_heartbeat: Set(worker_a.last_heartbeat),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let service = ComposePlacementService::new(db.clone());

        // Running on the control plane, moved to worker-a: deployment 7 plans
        // "start on worker-a, stop the control plane".
        service.pin(environment.id, None).await?;
        service
            .request_move(environment.id, Some(worker_a.id), true)
            .await?;
        service
            .expect_placement(environment.id, 7, Some(worker_a.id), Some(None))
            .await?;

        // Moved again, to worker-b, before deployment 7 starts anything.
        service
            .request_move(environment.id, Some(worker_b.id), true)
            .await?;
        let error = service
            .expect_placement(environment.id, 7, Some(worker_a.id), Some(None))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ComposePlacementError::MovedConcurrently {
                    deployment_id: 7,
                    ..
                }
            ),
            "{error}"
        );
        let message = error.to_string();
        assert!(
            message.contains(&format!("worker node #{}", worker_b.id)),
            "{message}"
        );
        assert!(message.contains("redeploy"), "{message}");

        // Nor may deployment 7 record its vacate: worker-b's move still has
        // to stop the control plane.
        let error = service
            .complete_vacate(environment.id, 7, Some(worker_a.id), None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::MovedConcurrently { .. }),
            "{error}"
        );
        let placement = service.get(environment.id).await?.ok_or("no placement")?;
        assert_eq!(placement.node_id, Some(worker_b.id));
        assert_eq!(placement.pending_vacate(), Some(None));

        // The deployment planned against the current placement goes ahead.
        service
            .expect_placement(environment.id, 8, Some(worker_b.id), Some(None))
            .await?;
        service
            .complete_vacate(environment.id, 8, Some(worker_b.id), None)
            .await?;
        let placement = service.get(environment.id).await?.ok_or("no placement")?;
        assert_eq!(placement.pending_vacate(), None);
        Ok(())
    }

    /// A move that overlaps a deployment completing its vacate waits for it,
    /// then decides against what is really running: it never restores the
    /// vacate the deployment just completed, and a no-acknowledgement
    /// "cancel" back to the old host is refused once the data has moved.
    #[tokio::test]
    async fn a_move_overlapping_a_completed_vacate_sees_it(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::{QuerySelect, TransactionTrait};
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (_, environment, worker_a) = seed(&db).await?;
        let service = ComposePlacementService::new(db.clone());

        // Pinned on the control plane, moving to worker-a.
        service.pin(environment.id, None).await?;
        service
            .request_move(environment.id, Some(worker_a.id), true)
            .await?;

        // A deployment holds the row while it records that the control plane
        // is stopped; a cancel back to the control plane arrives meanwhile.
        let deployment = db.begin().await?;
        compose_placements::Entity::find_by_id(environment.id)
            .lock_exclusive()
            .one(&deployment)
            .await?;
        let mover = ComposePlacementService::new(db.clone());
        let environment_id = environment.id;
        let cancel =
            tokio::spawn(async move { mover.request_move(environment_id, None, false).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!cancel.is_finished(), "the move must wait for the row");
        compose_placements::Entity::update_many()
            .col_expr(
                compose_placements::Column::VacatePending,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(compose_placements::Column::EnvironmentId.eq(environment.id))
            .exec(&deployment)
            .await?;
        deployment.commit().await?;

        // The stack now runs on worker-a: going back is a real move.
        let error = cancel.await?.unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::DataLossNotAcknowledged { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("worker-a"), "{error}");

        // Acknowledged, it records worker-a (not the control plane it
        // already left) as the host to stop.
        let (placement, previous) = service.request_move(environment.id, None, true).await?;
        assert_eq!(placement.node_id, None);
        assert_eq!(placement.pending_vacate(), Some(Some(worker_a.id)));
        assert_eq!(
            previous.and_then(|previous| previous.node_id),
            Some(worker_a.id)
        );
        Ok(())
    }

    #[test]
    fn a_node_holds_stacks_at_home_and_stacks_moving_away_from_it() {
        use sea_orm::{DatabaseBackend, QueryTrait};
        let sql = compose_placements::Entity::find()
            .filter(on_node(3))
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""node_id" = 3"#), "{sql}");
        assert!(sql.contains(r#""vacate_pending" = TRUE"#), "{sql}");
        assert!(sql.contains(r#""vacate_node_id" = 3"#), "{sql}");
        assert!(sql.contains(" OR "), "{sql}");
    }

    /// The first deployment stays on the control plane when nothing
    /// constrains it and there is no scheduler (single-node install).
    #[tokio::test]
    async fn an_unconstrained_first_deployment_stays_on_the_control_plane(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (project, environment, node) = seed(&db).await?;
        let service = ComposePlacementService::new(db.clone());
        let plan = service
            .plan(None, &project, &environment, None, None)
            .await?;
        assert_eq!(plan.home, None);
        assert!(!plan.pinned);

        // Pinned to a worker with no scheduler to vouch for it: refused, not
        // moved.
        service.pin(environment.id, Some(node.id)).await?;
        let error = service
            .plan(None, &project, &environment, None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::HomeNodeUnavailable { .. }),
            "{error}"
        );
        Ok(())
    }

    /// Status reports the home and drift; moving a pinned stack requires
    /// acknowledging that its data stays behind.
    #[tokio::test]
    async fn moving_a_pinned_stack_requires_acknowledging_data_loss(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use sea_orm::ActiveModelTrait;
        let Ok(test_db) = TestDatabase::with_migrations().await else {
            println!("Test database not available, skipping");
            return Ok(());
        };
        let db = test_db.connection_arc();
        let (project, environment, node) = seed(&db).await?;
        let service = ComposePlacementService::new(db.clone());

        let status = service.status(project.id, environment.id).await?;
        assert!(!status.pinned);
        assert_eq!(status.home_node_id, None);

        // Pinned on the control plane while Target Nodes name the worker.
        service.pin(environment.id, None).await?;
        let mut configured: environments::ActiveModel = environment.clone().into();
        configured.deployment_config =
            Set(Some(temps_entities::deployment_config::DeploymentConfig {
                target_nodes: Some(vec![node.id]),
                ..Default::default()
            }));
        configured.update(db.as_ref()).await?;
        let status = service.status(project.id, environment.id).await?;
        assert_eq!(status.home_node_id, Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(status.home_node_status.as_deref(), Some("active"));
        let drift = status.drift.unwrap_or_default();
        assert!(drift.contains("the control plane"), "{drift}");

        let error = service
            .move_stack(project.id, environment.id, node.id, false)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::DataLossNotAcknowledged { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("empty volumes"), "{error}");

        let (status, from) = service
            .move_stack(project.id, environment.id, node.id, true)
            .await?;
        assert_eq!(from, Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(status.home_node_id, Some(node.id));
        assert_eq!(status.home_node_name.as_deref(), Some("worker-a"));
        assert_eq!(status.moving_from_node_id, Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(status.drift, None);

        // Another project's environment is not found through this one.
        let error = service
            .status(project.id + 1, environment.id)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::EnvironmentNotFound { .. }),
            "{error}"
        );

        // An offline worker cannot take a stack.
        let mut offline: nodes::ActiveModel = node.clone().into();
        offline.status = Set("offline".to_string());
        offline.update(db.as_ref()).await?;
        // Moving back to the host that still runs it cancels the move, and
        // loses nothing, so it needs no acknowledgement.
        let (moved_back, _) = service
            .move_stack(project.id, environment.id, CONTROL_PLANE_NODE_ID, false)
            .await?;
        assert_eq!(moved_back.home_node_id, Some(CONTROL_PLANE_NODE_ID));
        assert_eq!(moved_back.moving_from_node_id, None);
        let error = service
            .move_stack(project.id, environment.id, node.id, true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is offline"), "{error}");

        // A build-only node never hosts workloads, so it takes no stack.
        let mut builder: nodes::ActiveModel = node.clone().into();
        builder.status = Set("active".to_string());
        builder.labels = Set(serde_json::json!({ "temps.sh/role": "builder" }));
        builder.update(db.as_ref()).await?;
        let error = service
            .move_stack(project.id, environment.id, node.id, true)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposePlacementError::InvalidMove { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("build-only"), "{error}");
        assert_eq!(
            service
                .status(project.id, environment.id)
                .await?
                .home_node_id,
            Some(CONTROL_PLANE_NODE_ID)
        );
        Ok(())
    }
}
