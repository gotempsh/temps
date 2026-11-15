// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The long-running operations a chat started — deployments, backups and
//! restores — with their live state. The chat's operations button lists them,
//! so what is still in flight stays in view after the message that started it
//! has scrolled away, and does not depend on the model having set up a goal to
//! watch it.
//!
//! An operation is recorded when it starts: when the write tool executes one
//! of [`TRACKED_OPERATIONS`], and when the chat deploys its sandbox's files. The
//! row only points at the deployment, backup or restore run; its state is read
//! from that record each time, so it is never stale, and a record that has
//! since been deleted drops out of the list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, Order, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_entities::{
    ai_chat_operations, ai_conversations, ai_pending_actions, backups, deployments, environments,
    external_service_backups, external_services, projects, restore_runs,
};
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

/// Most operations listed for one chat, newest first.
pub const MAX_OPERATIONS: u64 = 30;

/// A pipeline trigger does not say which deployment it started, so the first
/// unclaimed deployment of its environment created inside the trigger's window
/// (see [`pipeline_window`]) is taken as that one. The deployment row can be
/// written a moment before the trigger is recorded, so the window starts this
/// long before it.
const PIPELINE_MATCH_SLACK_SECS: i64 = 10;

/// A pipeline trigger that has started no deployment after this long is shown
/// as not having started one, rather than as still starting. It is also where
/// the trigger's matching window ends, so a deployment created later is never
/// attributed to it.
const PIPELINE_START_GRACE_SECS: i64 = 120;

/// The chat tool that deploys a sandbox's files.
pub const SANDBOX_DEPLOY_OPERATION: &str = crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL;

/// Write operations that start something long-running, and so are recorded.
pub const TRACKED_OPERATIONS: [&str; 11] = [
    "trigger_project_pipeline",
    "deploy_from_image",
    "deploy_from_image_upload",
    "deploy_from_uploaded_source",
    "deploy_from_static",
    "rollback_to_deployment",
    "promote_deployment",
    "run_backup_for_source",
    "run_external_service_backup",
    "start_restore",
    // Recorded by the sandbox tool rather than the write tool; listed so the
    // set names everything that can appear.
    SANDBOX_DEPLOY_OPERATION,
];

#[derive(Debug, Error)]
pub enum ChatOperationError {
    #[error("Failed to read the operations started by conversation {conversation_id}: {reason}")]
    Database {
        conversation_id: String,
        reason: String,
    },
    #[error(
        "Failed to record operation '{operation_id}' for conversation {conversation_id}: {reason}"
    )]
    Record {
        conversation_id: i64,
        operation_id: String,
        reason: String,
    },
}

/// What a chat started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatOperationKind {
    Deployment,
    /// A backup of an S3 source.
    Backup,
    /// A backup of a database or other external service.
    ServiceBackup,
    Restore,
}

impl ChatOperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deployment => "deployment",
            Self::Backup => "backup",
            Self::ServiceBackup => "service_backup",
            Self::Restore => "restore",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "deployment" => Some(Self::Deployment),
            "backup" => Some(Self::Backup),
            "service_backup" => Some(Self::ServiceBackup),
            "restore" => Some(Self::Restore),
            _ => None,
        }
    }
}

/// Where an operation is, in the same words for every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatOperationState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// A pipeline trigger that started no deployment.
    NotStarted,
}

/// Map a deployment, backup or restore status onto [`ChatOperationState`].
/// Anything not finished counts as running: the pipelines add intermediate
/// states (`built`, `deploying`, `queued`) more often than final ones.
fn state_of(status: &str) -> ChatOperationState {
    match status.to_ascii_lowercase().as_str() {
        "completed" | "succeeded" | "success" | "ready" => ChatOperationState::Succeeded,
        "failed" | "error" | "errored" | "timed_out" => ChatOperationState::Failed,
        "cancelled" | "canceled" | "stopped" | "skipped" => ChatOperationState::Cancelled,
        _ => ChatOperationState::Running,
    }
}

/// One operation the chat started, with its current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatOperation {
    pub id: i64,
    pub kind: ChatOperationKind,
    /// What started it: the API operation id, or `temps_deploy_sandbox_source`.
    pub operation_id: String,
    /// e.g. "Deployment #41", "Backup of orders-db".
    pub title: String,
    /// Where: the project and environment, or the S3 source.
    pub detail: Option<String>,
    pub state: ChatOperationState,
    /// The record's own status, e.g. `running` or `completed`.
    pub status: Option<String>,
    /// Why it failed, when the record says.
    pub error: Option<String>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub service_id: Option<i32>,
    /// Console path that shows the operation.
    pub href: Option<String>,
    /// ISO 8601 time the chat started it.
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// The operations a chat started, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChatOperationsResponse {
    pub operations: Vec<ChatOperation>,
    /// How many of them are still running.
    pub running: usize,
    /// True when the chat started more operations than this response covers.
    pub truncated: bool,
}

/// An operation to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChatOperation {
    pub kind: ChatOperationKind,
    pub operation_id: String,
    pub target_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub service_id: Option<i32>,
}

fn int(value: &Value, key: &str) -> Option<i32> {
    let found = value.get(key)?;
    found
        .as_i64()
        .or_else(|| found.as_str().and_then(|text| text.trim().parse().ok()))
        .and_then(|number| i32::try_from(number).ok())
        .filter(|number| *number > 0)
}

/// What an executed write started, read from its recorded response (falling
/// back to its parameters for ids the response leaves out). `None` for an
/// operation that starts nothing long-running, and for a response that does
/// not name what it started.
pub fn started_by_action(
    operation_id: &str,
    params: &Value,
    result: Option<&Value>,
) -> Option<NewChatOperation> {
    let result = result?;
    let from = |key: &str| int(result, key).or_else(|| int(params, key));
    let deployment = |target_id: Option<i32>| {
        Some(NewChatOperation {
            kind: ChatOperationKind::Deployment,
            operation_id: operation_id.to_string(),
            target_id,
            project_id: Some(from("project_id").or_else(|| int(params, "id"))?),
            environment_id: from("environment_id"),
            service_id: None,
        })
    };
    match operation_id {
        // Starts the pipeline; the deployment it creates is found later.
        "trigger_project_pipeline" => deployment(None),
        "deploy_from_image"
        | "deploy_from_image_upload"
        | "deploy_from_uploaded_source"
        | "deploy_from_static"
        | "rollback_to_deployment"
        | "promote_deployment" => deployment(Some(int(result, "id")?)),
        "run_backup_for_source" => Some(NewChatOperation {
            kind: ChatOperationKind::Backup,
            operation_id: operation_id.to_string(),
            target_id: Some(int(result, "id")?),
            project_id: None,
            environment_id: None,
            service_id: None,
        }),
        "run_external_service_backup" => Some(NewChatOperation {
            kind: ChatOperationKind::ServiceBackup,
            operation_id: operation_id.to_string(),
            target_id: Some(int(result, "id")?),
            project_id: None,
            environment_id: None,
            service_id: int(result, "service_id").or_else(|| int(params, "id")),
        }),
        "start_restore" => Some(NewChatOperation {
            kind: ChatOperationKind::Restore,
            operation_id: operation_id.to_string(),
            target_id: Some(int(result, "id")?),
            project_id: None,
            environment_id: None,
            service_id: int(result, "target_service_id")
                .or_else(|| int(result, "source_service_id"))
                .or_else(|| int(params, "id")),
        }),
        _ => None,
    }
}

/// Record that `conversation_id` started `operation`. Failing to record never
/// fails the operation itself: it already ran, and only the list would miss it.
pub async fn record(
    db: &DatabaseConnection,
    conversation_id: i64,
    operation: NewChatOperation,
) -> Result<(), ChatOperationError> {
    let operation_id = operation.operation_id.clone();
    ai_chat_operations::ActiveModel {
        conversation_id: Set(conversation_id),
        kind: Set(operation.kind.as_str().to_string()),
        operation_id: Set(operation.operation_id),
        target_id: Set(operation.target_id),
        project_id: Set(operation.project_id),
        environment_id: Set(operation.environment_id),
        service_id: Set(operation.service_id),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .map(|_| ())
    .map_err(|error| ChatOperationError::Record {
        conversation_id,
        operation_id,
        reason: error.to_string(),
    })
}

/// Record what an executed pending action started, if anything. Logged, not
/// returned: the action has already run.
pub async fn record_executed_action(db: &DatabaseConnection, action: &ai_pending_actions::Model) {
    let Some(operation) =
        started_by_action(&action.operation_id, &action.params, action.result.as_ref())
    else {
        return;
    };
    if let Err(error) = record(db, action.conversation_id, operation).await {
        warn!(
            action = %action.public_id,
            "failed to record the operation an assistant action started: {error}"
        );
    }
}

/// Which operations the caller may see. Deployments need deployment read
/// access to their project; backups and restores need backup read access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationVisibility {
    pub deployments: bool,
    pub backups: bool,
}

/// A recorded pipeline trigger that names no deployment yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PipelineTrigger {
    row_id: i64,
    environment_id: i32,
    created_at: DateTime<Utc>,
}

/// A deployment that could be the one a pipeline trigger started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeploymentStart {
    deployment_id: i32,
    environment_id: i32,
    created_at: DateTime<Utc>,
}

/// The creation times a deployment started by a trigger recorded at `at` can
/// have: from a moment before it (the deployment row can be written first) to
/// the end of the grace period after which the trigger is shown as having
/// started nothing. Using the same bound for both keeps them consistent: a
/// trigger shown as not started can never later claim a deployment.
fn pipeline_window(at: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        at - Duration::seconds(PIPELINE_MATCH_SLACK_SECS),
        at + Duration::seconds(PIPELINE_START_GRACE_SECS),
    )
}

/// Pair each trigger with the earliest deployment of its environment inside
/// its window that no other operation holds. Triggers are taken oldest first,
/// so two triggers of one environment get their deployments in order, and a
/// deployment is given to at most one trigger.
fn match_pipeline_triggers(
    triggers: &[PipelineTrigger],
    candidates: &[DeploymentStart],
    claimed: &HashSet<i32>,
) -> HashMap<i64, i32> {
    let mut triggers: Vec<&PipelineTrigger> = triggers.iter().collect();
    triggers.sort_by_key(|trigger| (trigger.created_at, trigger.row_id));
    let mut candidates: Vec<&DeploymentStart> = candidates.iter().collect();
    candidates.sort_by_key(|start| (start.created_at, start.deployment_id));
    let mut taken: HashSet<i32> = claimed.clone();
    let mut matched = HashMap::new();
    for trigger in triggers {
        let (from, until) = pipeline_window(trigger.created_at);
        let found = candidates.iter().find(|start| {
            start.environment_id == trigger.environment_id
                && start.created_at >= from
                && start.created_at <= until
                && !taken.contains(&start.deployment_id)
        });
        if let Some(start) = found {
            taken.insert(start.deployment_id);
            matched.insert(trigger.row_id, start.deployment_id);
        }
    }
    matched
}

/// Reads the operations a conversation started.
pub struct ChatOperationService {
    db: Arc<DatabaseConnection>,
}

impl ChatOperationService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// The newest recorded operations of `conversation`, before their state is
    /// read: the caller drops the ones it may not see first. The second value
    /// says whether there were more.
    pub async fn recorded(
        &self,
        conversation: &ai_conversations::Model,
        visibility: OperationVisibility,
    ) -> Result<(Vec<ai_chat_operations::Model>, bool), ChatOperationError> {
        let mut kinds: Vec<&str> = Vec::with_capacity(4);
        if visibility.deployments {
            kinds.push(ChatOperationKind::Deployment.as_str());
        }
        if visibility.backups {
            kinds.extend([
                ChatOperationKind::Backup.as_str(),
                ChatOperationKind::ServiceBackup.as_str(),
                ChatOperationKind::Restore.as_str(),
            ]);
        }
        if kinds.is_empty() {
            return Ok((Vec::new(), false));
        }
        let mut rows = ai_chat_operations::Entity::find()
            .filter(ai_chat_operations::Column::ConversationId.eq(conversation.id))
            .filter(ai_chat_operations::Column::Kind.is_in(kinds))
            .order_by(ai_chat_operations::Column::CreatedAt, Order::Desc)
            .order_by(ai_chat_operations::Column::Id, Order::Desc)
            .limit(MAX_OPERATIONS + 1)
            .all(self.db.as_ref())
            .await
            .map_err(|error| self.database(conversation, error))?;
        let truncated = rows.len() as u64 > MAX_OPERATIONS;
        rows.truncate(MAX_OPERATIONS as usize);
        Ok((rows, truncated))
    }

    fn database(
        &self,
        conversation: &ai_conversations::Model,
        error: sea_orm::DbErr,
    ) -> ChatOperationError {
        ChatOperationError::Database {
            conversation_id: conversation.public_id.clone(),
            reason: error.to_string(),
        }
    }

    /// Read the live state of `rows`. One query per table, whatever the count.
    pub async fn resolve(
        &self,
        conversation: &ai_conversations::Model,
        rows: Vec<ai_chat_operations::Model>,
        truncated: bool,
    ) -> Result<ChatOperationsResponse, ChatOperationError> {
        let db = self.db.as_ref();
        let database = |error| self.database(conversation, error);
        let targets = |kind: ChatOperationKind| -> Vec<i32> {
            rows.iter()
                .filter(|row| ChatOperationKind::parse(&row.kind) == Some(kind))
                .filter_map(|row| row.target_id)
                .collect()
        };

        // Deployments: the ones named, plus the candidates for pipeline
        // triggers that did not name one.
        let deployment_ids = targets(ChatOperationKind::Deployment);
        let mut live_deployments: HashMap<i32, deployments::Model> = if deployment_ids.is_empty() {
            HashMap::new()
        } else {
            deployments::Entity::find()
                .filter(deployments::Column::Id.is_in(deployment_ids))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|deployment| (deployment.id, deployment))
                .collect()
        };
        let triggers: Vec<PipelineTrigger> = rows
            .iter()
            .filter(|row| {
                row.kind == ChatOperationKind::Deployment.as_str() && row.target_id.is_none()
            })
            .filter_map(|row| {
                Some(PipelineTrigger {
                    row_id: row.id,
                    environment_id: row.environment_id?,
                    created_at: row.created_at,
                })
            })
            .collect();
        let mut matched: HashMap<i64, i32> = HashMap::new();
        if !triggers.is_empty() {
            // Only deployments inside some trigger's own window are read, so
            // an old trigger widens nothing and cannot crowd newer ones out of
            // the capped result.
            let windows = triggers.iter().fold(Condition::any(), |any, trigger| {
                let (from, until) = pipeline_window(trigger.created_at);
                any.add(
                    Condition::all()
                        .add(deployments::Column::EnvironmentId.eq(trigger.environment_id))
                        .add(deployments::Column::CreatedAt.gte(from))
                        .add(deployments::Column::CreatedAt.lte(until)),
                )
            });
            let candidates = deployments::Entity::find()
                .filter(windows)
                .order_by(deployments::Column::CreatedAt, Order::Asc)
                .order_by(deployments::Column::Id, Order::Asc)
                .limit(MAX_OPERATIONS * 4)
                .all(db)
                .await
                .map_err(database)?;
            // A deployment another operation row already names was started by
            // that operation, whichever chat it belongs to.
            let candidate_ids: Vec<i32> = candidates.iter().map(|d| d.id).collect();
            let claimed: HashSet<i32> = if candidate_ids.is_empty() {
                HashSet::new()
            } else {
                ai_chat_operations::Entity::find()
                    .select_only()
                    .column(ai_chat_operations::Column::TargetId)
                    .filter(
                        ai_chat_operations::Column::Kind.eq(ChatOperationKind::Deployment.as_str()),
                    )
                    .filter(ai_chat_operations::Column::TargetId.is_in(candidate_ids))
                    .into_tuple::<Option<i32>>()
                    .all(db)
                    .await
                    .map_err(database)?
                    .into_iter()
                    .flatten()
                    .collect()
            };
            let starts: Vec<DeploymentStart> = candidates
                .iter()
                .map(|deployment| DeploymentStart {
                    deployment_id: deployment.id,
                    environment_id: deployment.environment_id,
                    created_at: deployment.created_at,
                })
                .collect();
            matched = match_pipeline_triggers(&triggers, &starts, &claimed);
            for deployment in candidates {
                if matched.values().any(|id| *id == deployment.id) {
                    live_deployments.insert(deployment.id, deployment);
                }
            }
            // Remember the match, so the next read looks the deployment up by
            // id instead of searching again. Only a row that still names no
            // deployment is written, so a concurrent read cannot overwrite one.
            for (row_id, deployment_id) in &matched {
                let update = ai_chat_operations::Entity::update_many()
                    .col_expr(
                        ai_chat_operations::Column::TargetId,
                        Expr::value(*deployment_id),
                    )
                    .filter(ai_chat_operations::Column::Id.eq(*row_id))
                    .filter(ai_chat_operations::Column::TargetId.is_null())
                    .exec(db)
                    .await;
                if let Err(error) = update {
                    warn!(
                        operation = row_id,
                        deployment_id, "failed to remember a pipeline's deployment: {error}"
                    );
                }
            }
        }

        let project_ids: Vec<i32> = rows.iter().filter_map(|row| row.project_id).collect();
        let live_projects: HashMap<i32, projects::Model> = if project_ids.is_empty() {
            HashMap::new()
        } else {
            projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids))
                .filter(projects::Column::IsDeleted.eq(false))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|project| (project.id, project))
                .collect()
        };
        let environment_ids: Vec<i32> = rows
            .iter()
            .filter_map(|row| row.environment_id)
            .chain(live_deployments.values().map(|d| d.environment_id))
            .collect();
        let environment_names: HashMap<i32, String> = if environment_ids.is_empty() {
            HashMap::new()
        } else {
            environments::Entity::find()
                .filter(environments::Column::Id.is_in(environment_ids))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|environment| (environment.id, environment.name))
                .collect()
        };

        let backup_ids = targets(ChatOperationKind::Backup);
        let live_backups: HashMap<i32, backups::Model> = if backup_ids.is_empty() {
            HashMap::new()
        } else {
            backups::Entity::find()
                .filter(backups::Column::Id.is_in(backup_ids))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|backup| (backup.id, backup))
                .collect()
        };
        let service_backup_ids = targets(ChatOperationKind::ServiceBackup);
        let live_service_backups: HashMap<i32, external_service_backups::Model> =
            if service_backup_ids.is_empty() {
                HashMap::new()
            } else {
                external_service_backups::Entity::find()
                    .filter(external_service_backups::Column::Id.is_in(service_backup_ids))
                    .all(db)
                    .await
                    .map_err(database)?
                    .into_iter()
                    .map(|backup| (backup.id, backup))
                    .collect()
            };
        let restore_ids = targets(ChatOperationKind::Restore);
        let live_restores: HashMap<i32, restore_runs::Model> = if restore_ids.is_empty() {
            HashMap::new()
        } else {
            restore_runs::Entity::find()
                .filter(restore_runs::Column::Id.is_in(restore_ids))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|run| (run.id, run))
                .collect()
        };
        let service_ids: Vec<i32> = rows.iter().filter_map(|row| row.service_id).collect();
        let service_names: HashMap<i32, String> = if service_ids.is_empty() {
            HashMap::new()
        } else {
            external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids))
                .all(db)
                .await
                .map_err(database)?
                .into_iter()
                .map(|service| (service.id, service.name))
                .collect()
        };

        let now = Utc::now();
        let mut operations = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(kind) = ChatOperationKind::parse(&row.kind) else {
                continue;
            };
            let operation = match kind {
                ChatOperationKind::Deployment => {
                    let deployment_id = row.target_id.or_else(|| matched.get(&row.id).copied());
                    let project = row.project_id.and_then(|id| live_projects.get(&id));
                    // A deleted project takes its deployments with it.
                    let Some(project) = project else { continue };
                    match deployment_id {
                        Some(deployment_id) => {
                            let Some(deployment) = live_deployments.get(&deployment_id) else {
                                continue;
                            };
                            let environment = environment_names.get(&deployment.environment_id);
                            ChatOperation {
                                id: row.id,
                                kind,
                                operation_id: row.operation_id,
                                title: format!("Deployment #{}", deployment.id),
                                detail: Some(match environment {
                                    Some(environment) => {
                                        format!("{} · {environment}", project.name)
                                    }
                                    None => project.name.clone(),
                                }),
                                state: state_of(&deployment.state),
                                status: Some(deployment.state.clone()),
                                error: deployment.cancelled_reason.clone(),
                                project_id: Some(project.id),
                                environment_id: Some(deployment.environment_id),
                                deployment_id: Some(deployment.id),
                                service_id: None,
                                href: Some(format!(
                                    "/projects/{}/deployments/{}",
                                    project.slug, deployment.id
                                )),
                                started_at: row.created_at,
                                finished_at: deployment.finished_at,
                            }
                        }
                        None => {
                            let starting =
                                now - row.created_at < Duration::seconds(PIPELINE_START_GRACE_SECS);
                            ChatOperation {
                                id: row.id,
                                kind,
                                operation_id: row.operation_id,
                                title: "Deployment".to_string(),
                                detail: Some(project.name.clone()),
                                state: if starting {
                                    ChatOperationState::Running
                                } else {
                                    ChatOperationState::NotStarted
                                },
                                status: None,
                                error: (!starting).then(|| {
                                    "The pipeline was triggered but started no deployment"
                                        .to_string()
                                }),
                                project_id: Some(project.id),
                                environment_id: row.environment_id,
                                deployment_id: None,
                                service_id: None,
                                href: Some(format!("/projects/{}/deployments", project.slug)),
                                started_at: row.created_at,
                                finished_at: None,
                            }
                        }
                    }
                }
                ChatOperationKind::Backup => {
                    let Some(backup) = row.target_id.and_then(|id| live_backups.get(&id)) else {
                        continue;
                    };
                    ChatOperation {
                        id: row.id,
                        kind,
                        operation_id: row.operation_id,
                        title: format!("Backup {}", backup.name),
                        detail: Some(backup.backup_type.clone()),
                        state: state_of(&backup.state),
                        status: Some(backup.state.clone()),
                        error: backup.error_message.clone(),
                        project_id: None,
                        environment_id: None,
                        deployment_id: None,
                        service_id: None,
                        href: Some(format!(
                            "/backups/s3-sources/{}/backups/{}",
                            backup.s3_source_id, backup.id
                        )),
                        started_at: row.created_at,
                        finished_at: backup.finished_at,
                    }
                }
                ChatOperationKind::ServiceBackup => {
                    let Some(backup) = row.target_id.and_then(|id| live_service_backups.get(&id))
                    else {
                        continue;
                    };
                    let name = service_names
                        .get(&backup.service_id)
                        .cloned()
                        .or_else(|| backup.service_name_snapshot.clone());
                    ChatOperation {
                        id: row.id,
                        kind,
                        operation_id: row.operation_id,
                        title: match &name {
                            Some(name) => format!("Backup of {name}"),
                            None => format!("Backup of service {}", backup.service_id),
                        },
                        detail: backup.service_type_snapshot.clone(),
                        state: state_of(&backup.state),
                        status: Some(backup.state.clone()),
                        error: backup.error_message.clone(),
                        project_id: None,
                        environment_id: None,
                        deployment_id: None,
                        service_id: Some(backup.service_id),
                        href: Some(format!("/storage/{}", backup.service_id)),
                        started_at: row.created_at,
                        finished_at: backup.finished_at,
                    }
                }
                ChatOperationKind::Restore => {
                    let Some(run) = row.target_id.and_then(|id| live_restores.get(&id)) else {
                        continue;
                    };
                    let service_id = run.target_service_id.unwrap_or(run.source_service_id);
                    let name = run
                        .target_service_name
                        .clone()
                        .or_else(|| service_names.get(&service_id).cloned());
                    ChatOperation {
                        id: row.id,
                        kind,
                        operation_id: row.operation_id,
                        title: match &name {
                            Some(name) => format!("Restore into {name}"),
                            None => format!("Restore into service {service_id}"),
                        },
                        detail: Some(run.phase.replace('_', " ")),
                        state: state_of(&run.status),
                        status: Some(run.status.clone()),
                        error: run.error_message.clone(),
                        project_id: None,
                        environment_id: None,
                        deployment_id: None,
                        service_id: Some(service_id),
                        href: Some(format!("/storage/{service_id}")),
                        started_at: row.created_at,
                        finished_at: run.finished_at,
                    }
                }
            };
            operations.push(operation);
        }

        let running = operations
            .iter()
            .filter(|operation| operation.state == ChatOperationState::Running)
            .count();
        Ok(ChatOperationsResponse {
            operations,
            running,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap_or_default()
            + Duration::seconds(seconds)
    }

    fn trigger(row_id: i64, environment_id: i32, seconds: i64) -> PipelineTrigger {
        PipelineTrigger {
            row_id,
            environment_id,
            created_at: at(seconds),
        }
    }

    fn start(deployment_id: i32, environment_id: i32, seconds: i64) -> DeploymentStart {
        DeploymentStart {
            deployment_id,
            environment_id,
            created_at: at(seconds),
        }
    }

    #[test]
    fn a_trigger_claims_the_first_deployment_inside_its_window() {
        let matched = match_pipeline_triggers(
            &[trigger(1, 5, 0)],
            &[start(10, 5, -30), start(11, 5, -5), start(12, 5, 20)],
            &HashSet::new(),
        );
        // -30s is before the slack; -5s is the deployment written just before
        // the trigger was recorded.
        assert_eq!(matched, HashMap::from([(1, 11)]));
    }

    #[test]
    fn a_deployment_after_the_grace_window_is_not_attributed_to_the_trigger() {
        let matched = match_pipeline_triggers(
            &[trigger(1, 5, 0)],
            &[start(10, 5, PIPELINE_START_GRACE_SECS + 1)],
            &HashSet::new(),
        );
        assert!(matched.is_empty());
        // The edge of the window still counts, so "not started" and "matched"
        // agree on where the window ends.
        let matched = match_pipeline_triggers(
            &[trigger(1, 5, 0)],
            &[start(10, 5, PIPELINE_START_GRACE_SECS)],
            &HashSet::new(),
        );
        assert_eq!(matched, HashMap::from([(1, 10)]));
    }

    #[test]
    fn a_deployment_another_operation_holds_is_never_claimed_again() {
        let matched = match_pipeline_triggers(
            &[trigger(1, 5, 0)],
            &[start(10, 5, 5), start(11, 5, 8)],
            &HashSet::from([10]),
        );
        assert_eq!(matched, HashMap::from([(1, 11)]));
        let matched = match_pipeline_triggers(
            &[trigger(1, 5, 0)],
            &[start(10, 5, 5)],
            &HashSet::from([10]),
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn a_stale_trigger_does_not_take_a_newer_triggers_deployment() {
        // Trigger 1 started nothing; trigger 2 is an hour later.
        let matched = match_pipeline_triggers(
            &[trigger(2, 5, 3600), trigger(1, 5, 0)],
            &[start(20, 5, 3605)],
            &HashSet::new(),
        );
        assert_eq!(matched, HashMap::from([(2, 20)]));
    }

    #[test]
    fn close_triggers_get_their_deployments_in_order_and_by_environment() {
        let matched = match_pipeline_triggers(
            // Given newest first, as the rows are read.
            &[trigger(3, 6, 2), trigger(2, 5, 1), trigger(1, 5, 0)],
            &[start(31, 6, 3), start(21, 5, 40), start(20, 5, 30)],
            &HashSet::new(),
        );
        assert_eq!(matched, HashMap::from([(1, 20), (2, 21), (3, 31)]));
    }

    #[test]
    fn a_source_deploy_records_the_deployment_it_started() {
        let started = started_by_action(
            "deploy_from_uploaded_source",
            &json!({"project_id": 7, "environment_id": 9}),
            Some(&json!({"id": 41, "project_id": 7, "environment_id": 9, "state": "pending"})),
        )
        .expect("recorded");
        assert_eq!(
            started,
            NewChatOperation {
                kind: ChatOperationKind::Deployment,
                operation_id: "deploy_from_uploaded_source".into(),
                target_id: Some(41),
                project_id: Some(7),
                environment_id: Some(9),
                service_id: None,
            }
        );
    }

    #[test]
    fn a_pipeline_trigger_is_recorded_without_a_deployment() {
        // The pipeline answers with the project and environment only; the
        // deployment it creates is matched when the list is read.
        let started = started_by_action(
            "trigger_project_pipeline",
            &json!({"id": 7}),
            Some(&json!({"message": "Pipeline triggered", "project_id": 7, "environment_id": 9})),
        )
        .expect("recorded");
        assert_eq!(started.kind, ChatOperationKind::Deployment);
        assert_eq!(started.target_id, None);
        assert_eq!(started.project_id, Some(7));
        assert_eq!(started.environment_id, Some(9));
    }

    #[test]
    fn a_pipeline_trigger_falls_back_to_the_project_in_its_path() {
        let started = started_by_action(
            "trigger_project_pipeline",
            &json!({"id": "7"}),
            Some(&json!({"message": "Pipeline triggered", "environment_id": 9})),
        )
        .expect("recorded");
        assert_eq!(started.project_id, Some(7));
    }

    #[test]
    fn backups_and_restores_record_their_run() {
        let backup = started_by_action(
            "run_external_service_backup",
            &json!({"id": 5}),
            Some(&json!({"id": 12, "service_id": 5, "state": "running"})),
        )
        .expect("recorded");
        assert_eq!(backup.kind, ChatOperationKind::ServiceBackup);
        assert_eq!((backup.target_id, backup.service_id), (Some(12), Some(5)));

        let source_backup = started_by_action(
            "run_backup_for_source",
            &json!({"id": 3}),
            Some(&json!({"id": 30, "s3_source_id": 3, "state": "running"})),
        )
        .expect("recorded");
        assert_eq!(source_backup.kind, ChatOperationKind::Backup);
        assert_eq!(source_backup.target_id, Some(30));

        let restore = started_by_action(
            "start_restore",
            &json!({"id": 5}),
            Some(&json!({"id": 2, "source_service_id": 5, "target_service_id": 8})),
        )
        .expect("recorded");
        assert_eq!(restore.kind, ChatOperationKind::Restore);
        assert_eq!((restore.target_id, restore.service_id), (Some(2), Some(8)));
    }

    #[test]
    fn nothing_is_recorded_without_an_identity_or_for_other_operations() {
        assert!(started_by_action("deploy_from_static", &json!({}), Some(&json!({}))).is_none());
        assert!(started_by_action("deploy_from_static", &json!({}), None).is_none());
        assert!(started_by_action("create_project", &json!({}), Some(&json!({"id": 3}))).is_none());
        assert!(started_by_action(
            "run_external_service_backup",
            &json!({}),
            Some(&json!({"id": 0}))
        )
        .is_none());
    }

    #[test]
    fn every_tracked_write_operation_is_understood() {
        let result = json!({"id": 1, "project_id": 2, "environment_id": 3});
        for operation in TRACKED_OPERATIONS {
            if operation == SANDBOX_DEPLOY_OPERATION {
                continue;
            }
            assert!(
                started_by_action(operation, &json!({}), Some(&result)).is_some(),
                "{operation} is tracked but not recorded"
            );
        }
    }

    #[test]
    fn states_read_the_same_for_every_kind() {
        assert_eq!(state_of("pending"), ChatOperationState::Running);
        assert_eq!(state_of("running"), ChatOperationState::Running);
        assert_eq!(state_of("built"), ChatOperationState::Running);
        assert_eq!(state_of("completed"), ChatOperationState::Succeeded);
        assert_eq!(state_of("failed"), ChatOperationState::Failed);
        assert_eq!(state_of("cancelled"), ChatOperationState::Cancelled);
    }

    #[test]
    fn kinds_round_trip_through_the_stored_name() {
        for kind in [
            ChatOperationKind::Deployment,
            ChatOperationKind::Backup,
            ChatOperationKind::ServiceBackup,
            ChatOperationKind::Restore,
        ] {
            assert_eq!(ChatOperationKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).expect("json"),
                json!(kind.as_str())
            );
        }
        assert_eq!(ChatOperationKind::parse("unknown"), None);
    }
}
