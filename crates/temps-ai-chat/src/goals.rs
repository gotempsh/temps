// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Goals the Temps assistant tracks for a chat, and heals when they fail.
//!
//! A goal is one observable condition, such as "this deployment is healthy",
//! written as a read-only API operation plus the values that mean success and
//! failure. [`GoalService`] stores them. [`GoalWatcher`] evaluates the open ones
//! on an interval, as the user who created them, and starts a bounded fix turn
//! in the owning chat when a check fails. When a goal watching a deployment
//! is reached, it starts one short turn that checks the live app, since a
//! deployment marked complete has only passed the health check on `/`.
//!
//! The watcher is O(open goals due): one indexed query per tick, no per-goal
//! scan of anything else. Fix turns are capped per goal (`max_attempts`) and by
//! the goal's deadline, so an unattended chat cannot loop forever.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, UpdateMany,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_ai_api_tools::{ApiCallScope, ApiToolsHandle, ProjectSelectorScope};
use temps_auth::context::{AuthContext, AuthSource};
use temps_entities::{ai_conversations, ai_goals};
use thiserror::Error;
use utoipa::ToSchema;

use crate::ConversationService;

pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_HEALING: &str = "healing";
pub const STATUS_NEEDS_YOU: &str = "needs_you";
pub const STATUS_ACHIEVED: &str = "achieved";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELLED: &str = "cancelled";

/// Statuses the watcher still looks at.
const WATCHED_STATUSES: [&str; 3] = [STATUS_ACTIVE, STATUS_HEALING, STATUS_NEEDS_YOU];
/// Statuses the user may give a fresh budget with retry.
const RETRYABLE_STATUSES: [&str; 2] = [STATUS_FAILED, STATUS_NEEDS_YOU];

/// Goals a chat may have open at once. Each costs a check per interval.
pub const MAX_OPEN_GOALS_PER_CONVERSATION: u64 = 5;
/// Goals returned for one chat; closed ones past this fall off the panel.
const MAX_GOALS_LISTED: u64 = 50;
const DEFAULT_MAX_ATTEMPTS: i32 = 3;
const MAX_ATTEMPTS_CEILING: i32 = 5;
const DEFAULT_DEADLINE_MINUTES: i64 = 30;
const MAX_DEADLINE_MINUTES: i64 = 240;
const ACTIVE_CHECK_SECONDS: i64 = 15;
const NEEDS_YOU_CHECK_SECONDS: i64 = 60;
const TICK: Duration = Duration::from_secs(10);
const DUE_BATCH: u64 = 25;
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_OBSERVATION_CHARS: usize = 500;
/// Starts the observation of a reached deployment goal whose live check waits
/// for the chat's current reply to finish.
const LIVE_CHECK_WAITING: &str = "Reached; the live app is checked once the current reply finishes";
const MAX_VALUES: usize = 20;
const MAX_VALUE_CHARS: usize = 100;

#[derive(Debug, Error)]
pub enum GoalError {
    #[error("Goal '{goal_id}' was not found in conversation {conversation_id}")]
    NotFound {
        goal_id: String,
        conversation_id: i64,
    },
    #[error("Invalid goal for conversation {conversation_id}: {message}")]
    Validation {
        conversation_id: i64,
        message: String,
    },
    #[error("Conversation {conversation_id} already has {open} open goals; the limit is {limit}")]
    TooManyOpen {
        conversation_id: i64,
        open: u64,
        limit: u64,
    },
    #[error("Goal '{goal_id}' is {status} and cannot be {action}")]
    InvalidState {
        goal_id: String,
        status: String,
        action: &'static str,
    },
    #[error("Database error while handling goals: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// What the assistant supplies to start tracking a goal.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackGoalInput {
    pub title: String,
    pub operation_id: String,
    #[serde(default)]
    pub params: Value,
    pub success_path: String,
    pub success_values: Vec<String>,
    #[serde(default)]
    pub failure_values: Vec<String>,
    #[serde(default)]
    pub max_attempts: Option<i32>,
    #[serde(default)]
    pub deadline_minutes: Option<i64>,
}

/// What one evaluation of a goal's check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Pending(String),
    Achieved(String),
    Failed(String),
}

impl Observation {
    #[cfg(test)]
    fn text(&self) -> &str {
        match self {
            Self::Pending(text) | Self::Achieved(text) | Self::Failed(text) => text,
        }
    }
}

/// Goal as returned to the console.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GoalResponse {
    /// Opaque goal id.
    pub id: String,
    pub title: String,
    /// `active`, `healing`, `needs_you`, `achieved`, `failed` or `cancelled`.
    pub status: String,
    /// Fix turns started so far.
    pub attempts: i32,
    pub max_attempts: i32,
    /// What the last check saw, in one sentence.
    pub last_observation: Option<String>,
    /// ISO 8601, `Z` suffix.
    pub last_checked_at: Option<String>,
    /// The goal is given up, as failed, after this instant (ISO 8601).
    pub deadline_at: String,
    pub created_at: String,
    /// The read operation that observes the goal.
    pub check_operation: String,
    /// The deployment this goal watches, when it watches one, so the console
    /// can show its build stages next to the chat.
    pub deployment: Option<GoalDeployment>,
}

/// A deployment a goal is watching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GoalDeployment {
    pub project_id: i32,
    /// Absent when the goal watches whichever deployment is the project's
    /// latest, so the console follows that deployment as it changes.
    pub deployment_id: Option<i32>,
}

/// Reads the deployment out of a goal's check, which is a flat parameter
/// object. Only the numeric ids are surfaced; the rest of the parameters stay
/// private to the server.
fn watched_deployment(operation: &str, params: &Value) -> Option<GoalDeployment> {
    let id = |key: &str| -> Option<i32> {
        let value = params.get(key)?;
        let number = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))?;
        i32::try_from(number).ok().filter(|id| *id > 0)
    };
    match operation {
        "get_deployment" => Some(GoalDeployment {
            project_id: id("project_id")?,
            deployment_id: Some(id("deployment_id")?),
        }),
        // These read the project's latest deployment; the project is the path
        // parameter `id`, though a model may also have written `project_id`.
        "get_last_deployment" | "get_project_deployments" => Some(GoalDeployment {
            project_id: id("id").or_else(|| id("project_id"))?,
            deployment_id: None,
        }),
        _ => None,
    }
}

impl From<ai_goals::Model> for GoalResponse {
    fn from(goal: ai_goals::Model) -> Self {
        let deployment = watched_deployment(&goal.check_operation, &goal.check_params);
        Self {
            id: goal.public_id,
            title: goal.title,
            status: goal.status,
            attempts: goal.attempts,
            max_attempts: goal.max_attempts,
            last_observation: goal.last_observation,
            last_checked_at: goal.last_checked_at.map(|at| at.to_rfc3339()),
            deadline_at: goal.deadline_at.to_rfc3339(),
            created_at: goal.created_at.to_rfc3339(),
            check_operation: goal.check_operation,
            deployment,
        }
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

fn validate_values(label: &str, values: &[String]) -> Result<(), String> {
    if values.len() > MAX_VALUES {
        return Err(format!("{label} has more than {MAX_VALUES} values"));
    }
    if values
        .iter()
        .any(|v| v.trim().is_empty() || v.chars().count() > MAX_VALUE_CHARS)
    {
        return Err(format!(
            "every value in {label} must be 1 to {MAX_VALUE_CHARS} characters"
        ));
    }
    Ok(())
}

impl TrackGoalInput {
    fn validate(&self, conversation_id: i64) -> Result<(), GoalError> {
        let fail = |message: String| GoalError::Validation {
            conversation_id,
            message,
        };
        let title = self.title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Err(fail("title must be 1 to 200 characters".into()));
        }
        if self.operation_id.trim().is_empty() || self.operation_id.len() > 200 {
            return Err(fail("operation_id must be 1 to 200 characters".into()));
        }
        if !(self.params.is_null() || self.params.is_object()) {
            return Err(fail("params must be a JSON object".into()));
        }
        if !valid_path(&self.success_path) {
            return Err(fail(format!(
                "success_path '{}' must be a dot path of letters, digits, '_' or '-', e.g. 'status'",
                self.success_path
            )));
        }
        if self.success_values.is_empty() {
            return Err(fail(
                "success_values must list at least one value that means the goal is achieved"
                    .into(),
            ));
        }
        if self.failure_values.is_empty() {
            return Err(fail(
                "failure_values must list at least one value that means it went wrong (for \
                 example [\"failed\", \"cancelled\"]); without one the goal can never fail, so \
                 it could never be fixed"
                    .into(),
            ));
        }
        validate_values("success_values", &self.success_values).map_err(fail)?;
        validate_values("failure_values", &self.failure_values).map_err(fail)?;
        if self.success_values.iter().any(|s| {
            self.failure_values
                .iter()
                .any(|f| f.eq_ignore_ascii_case(s))
        }) {
            return Err(fail(
                "a value cannot be both a success value and a failure value".into(),
            ));
        }
        if let Some(attempts) = self.max_attempts {
            if !(1..=MAX_ATTEMPTS_CEILING).contains(&attempts) {
                return Err(fail(format!(
                    "max_attempts must be between 1 and {MAX_ATTEMPTS_CEILING}"
                )));
            }
        }
        if let Some(minutes) = self.deadline_minutes {
            if !(1..=MAX_DEADLINE_MINUTES).contains(&minutes) {
                return Err(fail(format!(
                    "deadline_minutes must be between 1 and {MAX_DEADLINE_MINUTES}"
                )));
            }
        }
        Ok(())
    }
}

fn lookup<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(body, |current, part| match current {
            Value::Object(map) => map.get(part),
            Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        })
}

fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_OBSERVATION_CHARS {
        flat
    } else {
        let head: String = flat.chars().take(MAX_OBSERVATION_CHARS).collect();
        format!("{head}…")
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Decide what a check response means for a goal. Pure so the rules are
/// testable without a router.
pub fn evaluate(goal: &ai_goals::Model, status: u16, body: &Value) -> Observation {
    if matches!(status, 401 | 403 | 404 | 410) {
        return Observation::Failed(format!(
            "The check {} answered {status}: what it watches is gone or no longer visible to you",
            goal.check_operation
        ));
    }
    if status == 429 || status >= 500 {
        return Observation::Pending(format!(
            "The check {} answered {status}; trying again",
            goal.check_operation
        ));
    }
    if !(200..300).contains(&status) {
        return Observation::Failed(format!(
            "The check {} answered {status}: {}",
            goal.check_operation,
            clip(&render(body))
        ));
    }
    let Some(found) = lookup(body, &goal.success_path) else {
        return Observation::Pending(format!("The response has no '{}' yet", goal.success_path));
    };
    let shown = render(found);
    let matches_any = |values: Vec<String>| values.iter().any(|v| v.eq_ignore_ascii_case(&shown));
    if matches_any(string_list(&goal.success_values)) {
        Observation::Achieved(format!("{} is {shown}", goal.success_path))
    } else if matches_any(string_list(&goal.failure_values)) {
        let detail = ["error_message", "error", "message", "reason"]
            .iter()
            .find_map(|key| lookup(body, key).and_then(Value::as_str))
            .map(|text| format!(": {}", clip(text)))
            .unwrap_or_default();
        Observation::Failed(format!("{} is {shown}{detail}", goal.success_path))
    } else {
        Observation::Pending(format!("{} is {shown}", goal.success_path))
    }
}

/// Message a fix turn starts from. It is shown in the chat as a note from the
/// goal, so it states plainly what happened and what is allowed.
pub fn heal_prompt(goal: &ai_goals::Model, observation: &str) -> String {
    format!(
        "Goal check failed: \"{title}\" (goal {id}). {observation}.\n\n\
         This is fix attempt {attempt} of {max}. Find the cause with your read tools (logs, status, \
         configuration), make the smallest change that fixes it, and redeploy or retry as needed. \
         If the fix creates a new deployment or resource, call `temps_goal` with action `retarget`, \
         goal_id `{id}` and a check on the new one so the goal keeps being watched. \
         If you cannot fix it without something only the user can provide, say exactly what you need.",
        title = goal.title,
        id = goal.public_id,
        attempt = goal.attempts,
        max = goal.max_attempts,
    )
}

/// Message the live-app check starts from, after a deployment goal is reached.
/// Shown in the chat as a note from the goal, like [`heal_prompt`].
pub fn live_check_prompt(goal: &ai_goals::Model, observation: &str) -> String {
    format!(
        "Goal reached: \"{title}\" (goal {id}). {observation}.\n\n\
         Check the live app once before calling it done. Request its main routes on the \
         deployed URL (from the sandbox with curl, when the sandbox can reach that URL) and read \
         the deployment's container logs for errors since it started. Then reply in two or three \
         lines with what you checked and what you saw; if you could not reach something, say so \
         rather than assuming it works. If something is broken, find the cause, fix it, redeploy \
         and call `temps_goal` to watch the new deployment. Do not change anything that works.",
        title = goal.title,
        id = goal.public_id,
    )
}

/// Whether the goal was reached and only its live check is waiting for the
/// chat's reply to finish.
fn waits_for_live_check(goal: &ai_goals::Model) -> bool {
    goal.last_observation
        .as_deref()
        .is_some_and(|text| text.starts_with(LIVE_CHECK_WAITING))
}

/// Whether a reached goal gets a live check: only a deployment has a running
/// app to look at.
fn wants_live_check(goal: &ai_goals::Model) -> bool {
    watched_deployment(&goal.check_operation, &goal.check_params).is_some()
}

pub struct GoalService {
    db: Arc<DatabaseConnection>,
}

impl GoalService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn track(
        &self,
        conversation_id: i64,
        owner_user_id: i32,
        auth_session_id: Option<i32>,
        input: TrackGoalInput,
    ) -> Result<ai_goals::Model, GoalError> {
        input.validate(conversation_id)?;
        let open = ai_goals::Entity::find()
            .filter(ai_goals::Column::ConversationId.eq(conversation_id))
            .filter(ai_goals::Column::Status.is_in(WATCHED_STATUSES))
            .count(self.db.as_ref())
            .await?;
        if open >= MAX_OPEN_GOALS_PER_CONVERSATION {
            return Err(GoalError::TooManyOpen {
                conversation_id,
                open,
                limit: MAX_OPEN_GOALS_PER_CONVERSATION,
            });
        }
        let now = Utc::now();
        let minutes = input.deadline_minutes.unwrap_or(DEFAULT_DEADLINE_MINUTES);
        let goal = ai_goals::ActiveModel {
            public_id: Set(format!("goal_{}", uuid::Uuid::new_v4().simple())),
            conversation_id: Set(conversation_id),
            owner_user_id: Set(owner_user_id),
            title: Set(input.title.trim().to_string()),
            check_operation: Set(input.operation_id.trim().to_string()),
            check_params: Set(if input.params.is_null() {
                Value::Object(Default::default())
            } else {
                input.params
            }),
            success_path: Set(input.success_path),
            success_values: Set(serde_json::json!(input.success_values)),
            failure_values: Set(serde_json::json!(input.failure_values)),
            status: Set(STATUS_ACTIVE.to_string()),
            attempts: Set(0),
            max_attempts: Set(input.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS)),
            next_check_at: Set(now + chrono::Duration::seconds(ACTIVE_CHECK_SECONDS)),
            deadline_at: Set(now + chrono::Duration::minutes(minutes)),
            auth_session_id: Set(auth_session_id),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(goal)
    }

    pub async fn list(&self, conversation_id: i64) -> Result<Vec<ai_goals::Model>, GoalError> {
        Ok(ai_goals::Entity::find()
            .filter(ai_goals::Column::ConversationId.eq(conversation_id))
            .order_by_desc(ai_goals::Column::CreatedAt)
            .limit(MAX_GOALS_LISTED)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn get(
        &self,
        conversation_id: i64,
        goal_id: &str,
    ) -> Result<ai_goals::Model, GoalError> {
        ai_goals::Entity::find()
            .filter(ai_goals::Column::ConversationId.eq(conversation_id))
            .filter(ai_goals::Column::PublicId.eq(goal_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| GoalError::NotFound {
                goal_id: goal_id.to_string(),
                conversation_id,
            })
    }

    /// Point an existing goal at a new check, usually the deployment a fix just
    /// created. Re-opens it for watching; the attempt count is kept so a fix
    /// that keeps failing still runs out.
    pub async fn retarget(
        &self,
        conversation_id: i64,
        goal_id: &str,
        input: TrackGoalInput,
    ) -> Result<ai_goals::Model, GoalError> {
        input.validate(conversation_id)?;
        let goal = self.get(conversation_id, goal_id).await?;
        if matches!(goal.status.as_str(), STATUS_ACHIEVED | STATUS_CANCELLED) {
            return Err(GoalError::InvalidState {
                goal_id: goal_id.to_string(),
                status: goal.status,
                action: "retargeted",
            });
        }
        let now = Utc::now();
        let minutes = input.deadline_minutes.unwrap_or(DEFAULT_DEADLINE_MINUTES);
        let params = if input.params.is_null() {
            Value::Object(Default::default())
        } else {
            input.params
        };
        // The status is re-checked by the update itself: a goal the user
        // cancelled while the fix turn was working stays cancelled.
        let updated = ai_goals::Entity::update_many()
            .col_expr(
                ai_goals::Column::CheckOperation,
                Expr::value(input.operation_id.trim().to_string()),
            )
            .col_expr(ai_goals::Column::CheckParams, Expr::value(params))
            .col_expr(
                ai_goals::Column::SuccessPath,
                Expr::value(input.success_path),
            )
            .col_expr(
                ai_goals::Column::SuccessValues,
                Expr::value(serde_json::json!(input.success_values)),
            )
            .col_expr(
                ai_goals::Column::FailureValues,
                Expr::value(serde_json::json!(input.failure_values)),
            )
            .col_expr(ai_goals::Column::Status, Expr::value(STATUS_ACTIVE))
            .col_expr(
                ai_goals::Column::LastObservation,
                Expr::value("Watching the new target"),
            )
            .col_expr(
                ai_goals::Column::NextCheckAt,
                Expr::value(now + chrono::Duration::seconds(ACTIVE_CHECK_SECONDS)),
            )
            .col_expr(
                ai_goals::Column::DeadlineAt,
                Expr::value(now + chrono::Duration::minutes(minutes)),
            )
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(now))
            .filter(ai_goals::Column::Id.eq(goal.id))
            .filter(ai_goals::Column::Status.is_not_in([STATUS_ACHIEVED, STATUS_CANCELLED]))
            .exec_with_returning(self.db.as_ref())
            .await?;
        match updated.into_iter().next() {
            Some(goal) => Ok(goal),
            None => Err(self
                .state_conflict(conversation_id, goal_id, "retargeted")
                .await),
        }
    }

    pub async fn cancel(
        &self,
        conversation_id: i64,
        goal_id: &str,
    ) -> Result<ai_goals::Model, GoalError> {
        let goal = self.get(conversation_id, goal_id).await?;
        if !WATCHED_STATUSES.contains(&goal.status.as_str()) {
            return Err(GoalError::InvalidState {
                goal_id: goal_id.to_string(),
                status: goal.status,
                action: "cancelled",
            });
        }
        // The user wins: the update accepts any still-watched status, so a
        // watcher tick that moved the goal between `active`, `healing` and
        // `needs_you` meanwhile cannot make the cancel fail or be undone (its
        // own writes are conditional on the row it loaded, see `record`).
        let updated = ai_goals::Entity::update_many()
            .col_expr(ai_goals::Column::Status, Expr::value(STATUS_CANCELLED))
            .col_expr(
                ai_goals::Column::LastObservation,
                Expr::value("Stopped by you"),
            )
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(ai_goals::Column::Id.eq(goal.id))
            .filter(ai_goals::Column::Status.is_in(WATCHED_STATUSES))
            .exec_with_returning(self.db.as_ref())
            .await?;
        match updated.into_iter().next() {
            Some(goal) => Ok(goal),
            None => Err(self
                .state_conflict(conversation_id, goal_id, "cancelled")
                .await),
        }
    }

    /// Give a goal that ran out of attempts, time or patience a fresh budget.
    pub async fn retry(
        &self,
        conversation_id: i64,
        goal_id: &str,
    ) -> Result<ai_goals::Model, GoalError> {
        let goal = self.get(conversation_id, goal_id).await?;
        if !RETRYABLE_STATUSES.contains(&goal.status.as_str()) {
            return Err(GoalError::InvalidState {
                goal_id: goal_id.to_string(),
                status: goal.status,
                action: "retried",
            });
        }
        let now = Utc::now();
        let updated = ai_goals::Entity::update_many()
            .col_expr(ai_goals::Column::Status, Expr::value(STATUS_ACTIVE))
            .col_expr(ai_goals::Column::Attempts, Expr::value(0))
            .col_expr(ai_goals::Column::LastObservation, Expr::value("Retrying"))
            .col_expr(ai_goals::Column::NextCheckAt, Expr::value(now))
            .col_expr(
                ai_goals::Column::DeadlineAt,
                Expr::value(now + chrono::Duration::minutes(DEFAULT_DEADLINE_MINUTES)),
            )
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(now))
            .filter(ai_goals::Column::Id.eq(goal.id))
            .filter(ai_goals::Column::Status.is_in(RETRYABLE_STATUSES))
            .exec_with_returning(self.db.as_ref())
            .await?;
        match updated.into_iter().next() {
            Some(goal) => Ok(goal),
            None => Err(self
                .state_conflict(conversation_id, goal_id, "retried")
                .await),
        }
    }

    /// The error for a user action whose conditional update matched nothing
    /// because the goal changed after it was read: names the status it has
    /// now, or that it is gone.
    async fn state_conflict(
        &self,
        conversation_id: i64,
        goal_id: &str,
        action: &'static str,
    ) -> GoalError {
        match self.get(conversation_id, goal_id).await {
            Ok(current) => GoalError::InvalidState {
                goal_id: goal_id.to_string(),
                status: current.status,
                action,
            },
            Err(error) => error,
        }
    }

    async fn due(&self, now: DateTime<Utc>) -> Result<Vec<ai_goals::Model>, GoalError> {
        Ok(ai_goals::Entity::find()
            .filter(ai_goals::Column::Status.is_in(WATCHED_STATUSES))
            .filter(ai_goals::Column::NextCheckAt.lte(now))
            .order_by_asc(ai_goals::Column::NextCheckAt)
            .limit(DUE_BATCH)
            .all(self.db.as_ref())
            .await?)
    }

    /// Write what a check saw, but only onto the row exactly as the watcher
    /// loaded it: same status and same `updated_at`. Every write to a goal
    /// sets `updated_at`, so a cancel, retry or retarget that landed while
    /// the check was running makes this match nothing, and the newer write
    /// stands. Returns the stored row, or `None` when the goal changed under
    /// the watcher; callers must then stop, not start any follow-up.
    async fn record(
        &self,
        goal: &ai_goals::Model,
        status: &str,
        observation: &str,
        next_check_in: Option<i64>,
    ) -> Result<Option<ai_goals::Model>, GoalError> {
        let now = Utc::now();
        let mut update = ai_goals::Entity::update_many()
            .col_expr(ai_goals::Column::Status, Expr::value(status))
            .col_expr(
                ai_goals::Column::LastObservation,
                Expr::value(clip(observation)),
            )
            .col_expr(ai_goals::Column::LastCheckedAt, Expr::value(now))
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(now));
        if let Some(seconds) = next_check_in {
            update = update.col_expr(
                ai_goals::Column::NextCheckAt,
                Expr::value(now + chrono::Duration::seconds(seconds)),
            );
        }
        let stored = unchanged_since_loaded(update, goal)
            .exec_with_returning(self.db.as_ref())
            .await?
            .into_iter()
            .next();
        if stored.is_none() {
            tracing::info!(
                goal_id = %goal.public_id,
                loaded_status = %goal.status,
                skipped_status = status,
                "AI goal changed while it was being checked; keeping the newer state"
            );
        }
        Ok(stored)
    }

    /// Claim the one transition from `active` to `healing`, counting the
    /// attempt. Returns `None` when another tick or a user action got there
    /// first, so a goal never starts two fix turns, and a goal the user
    /// stopped or re-pointed never starts one.
    async fn begin_heal(
        &self,
        goal: &ai_goals::Model,
    ) -> Result<Option<ai_goals::Model>, GoalError> {
        let update = ai_goals::Entity::update_many()
            .col_expr(ai_goals::Column::Status, Expr::value(STATUS_HEALING))
            .col_expr(
                ai_goals::Column::Attempts,
                Expr::col(ai_goals::Column::Attempts).add(1),
            )
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(ai_goals::Column::Status.eq(STATUS_ACTIVE));
        Ok(unchanged_since_loaded(update, goal)
            .exec_with_returning(self.db.as_ref())
            .await?
            .into_iter()
            .next())
    }

    /// Hand back the attempt [`Self::begin_heal`] claimed when its fix turn
    /// could not start, and record why, in one write conditional on the
    /// `healing` row `begin_heal` returned.
    async fn abandon_heal(
        &self,
        healing: &ai_goals::Model,
        observation: &str,
    ) -> Result<Option<ai_goals::Model>, GoalError> {
        let now = Utc::now();
        let update = ai_goals::Entity::update_many()
            .col_expr(ai_goals::Column::Status, Expr::value(STATUS_ACTIVE))
            .col_expr(
                ai_goals::Column::Attempts,
                Expr::col(ai_goals::Column::Attempts).sub(1),
            )
            .col_expr(
                ai_goals::Column::LastObservation,
                Expr::value(clip(observation)),
            )
            .col_expr(ai_goals::Column::LastCheckedAt, Expr::value(now))
            .col_expr(
                ai_goals::Column::NextCheckAt,
                Expr::value(now + chrono::Duration::seconds(ACTIVE_CHECK_SECONDS)),
            )
            .col_expr(ai_goals::Column::UpdatedAt, Expr::value(now))
            .filter(ai_goals::Column::Status.eq(STATUS_HEALING));
        Ok(unchanged_since_loaded(update, healing)
            .exec_with_returning(self.db.as_ref())
            .await?
            .into_iter()
            .next())
    }
}

/// Restrict a watcher write to the goal row as it was read: same id, status
/// and `updated_at`. `updated_at` is compared with the value read back from
/// the database, so it matches to the microsecond.
fn unchanged_since_loaded(
    update: UpdateMany<ai_goals::Entity>,
    goal: &ai_goals::Model,
) -> UpdateMany<ai_goals::Entity> {
    update
        .filter(ai_goals::Column::Id.eq(goal.id))
        .filter(ai_goals::Column::Status.eq(goal.status.as_str()))
        .filter(ai_goals::Column::UpdatedAt.eq(goal.updated_at))
}

/// Everything the assistant's `temps_goal` tool needs.
#[derive(Clone)]
pub struct GoalToolSupport {
    pub goals: Arc<GoalService>,
    pub read_api: Arc<ApiToolsHandle>,
}

fn session_id_of(auth: &AuthContext) -> Option<i32> {
    match &auth.source {
        AuthSource::Session { session_id, .. } => *session_id,
        _ => None,
    }
}

impl GoalToolSupport {
    /// Handle one `temps_goal` call from the model and return the text it
    /// reads back. Failures are sentences written to be acted on.
    pub async fn dispatch(
        &self,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> String {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => return format!("`temps_goal` arguments are not valid JSON: {error}"),
        };
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("track");
        match action {
            "track" | "retarget" => {}
            other => {
                return format!(
                    "Unknown `temps_goal` action '{other}'. Use `track` or `retarget`; the user \
                     stops a goal from the goal panel."
                )
            }
        }
        let input: TrackGoalInput = match serde_json::from_value(args.clone()) {
            Ok(input) => input,
            Err(error) => {
                return format!(
                    "`temps_goal` arguments are incomplete: {error}. Send every field, for example \
                     {{\"action\":\"track\",\"title\":\"Deployment 4 is live\",\
                     \"operation_id\":\"get_deployment\",\"params\":{{\"project_id\":2,\"deployment_id\":4}},\
                     \"success_path\":\"status\",\"success_values\":[\"completed\"],\
                     \"failure_values\":[\"failed\",\"cancelled\"]}}"
                )
            }
        };
        if let Err(message) = self.probe(&input, auth).await {
            return message;
        }
        let outcome = if action == "retarget" {
            let Some(goal_id) = args.get("goal_id").and_then(Value::as_str) else {
                return "`temps_goal` retarget needs the goal_id of the goal to re-point.".into();
            };
            self.goals.retarget(conversation_id, goal_id, input).await
        } else {
            let owner = auth.user_id();
            self.goals
                .track(conversation_id, owner, session_id_of(auth), input)
                .await
        };
        match outcome {
            Ok(goal) => serde_json::json!({
                "status": "tracking",
                "goal_id": goal.public_id,
                "title": goal.title,
                "checks": goal.check_operation,
                "max_attempts": goal.max_attempts,
                "note": if wants_live_check(&goal) {
                    "Temps now watches this. It brings you back into the chat if the deployment \
                     fails, and once when it is live, to check the running app."
                } else {
                    "Temps now watches this and will bring you back into the chat if it fails."
                }
            })
            .to_string(),
            Err(error) => error.to_string(),
        }
    }

    /// Run the check once now. A goal whose check cannot even run is a mistake
    /// to report to the model immediately, not one to discover minutes later.
    async fn probe(&self, input: &TrackGoalInput, auth: &AuthContext) -> Result<(), String> {
        let Some(caller) = self.read_api.get() else {
            return Err("Goal tracking is not ready yet; try again in a moment.".to_string());
        };
        let Some(schema) = caller.describe(input.operation_id.trim()) else {
            return Err(format!(
                "'{}' is not an operation `temps_goal` can watch. Use a read-only operation you \
                 have already called with the `temps` tool.",
                input.operation_id
            ));
        };
        if !schema.method.eq_ignore_ascii_case("GET") {
            return Err(format!(
                "'{}' is a {} operation. A goal check must be a GET so watching it never changes anything.",
                input.operation_id, schema.method
            ));
        }
        let scope = ApiCallScope {
            auth: auth.clone(),
            project_scope: ProjectSelectorScope::Unrestricted,
            origin: None,
            secrets: None,
        };
        let params = if input.params.is_null() {
            Value::Object(Default::default())
        } else {
            input.params.clone()
        };
        match caller.call(input.operation_id.trim(), params, &scope).await {
            Ok(response) if (200..300).contains(&response.status) => {
                if lookup(&response.body, &input.success_path).is_none() {
                    Err(format!(
                        "The check ran, but its response has no '{}'. Pick a success_path that exists in what `{}` returns.",
                        input.success_path, input.operation_id
                    ))
                } else {
                    Ok(())
                }
            }
            Ok(response) => Err(format!(
                "The check `{}` answered {} right now, so it cannot be watched as given: {}",
                input.operation_id,
                response.status,
                clip(&render(&response.body))
            )),
            Err(error) => Err(format!(
                "The check `{}` could not run: {error}",
                input.operation_id
            )),
        }
    }
}

/// Evaluates open goals and starts fix turns. One instance per server.
pub struct GoalWatcher {
    goals: Arc<GoalService>,
    db: Arc<DatabaseConnection>,
    read_api: Arc<ApiToolsHandle>,
    conversations: Arc<ConversationService>,
    project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    audit: Arc<dyn temps_core::AuditLogger>,
}

impl GoalWatcher {
    pub fn new(
        goals: Arc<GoalService>,
        db: Arc<DatabaseConnection>,
        read_api: Arc<ApiToolsHandle>,
        conversations: Arc<ConversationService>,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        audit: Arc<dyn temps_core::AuditLogger>,
    ) -> Self {
        Self {
            goals,
            db,
            read_api,
            conversations,
            project_access_checker,
            audit,
        }
    }

    /// Run until the process ends. Each tick is bounded: at most [`DUE_BATCH`]
    /// goals, each check capped at [`CHECK_TIMEOUT`].
    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = self.tick().await {
                tracing::warn!("AI goal watcher tick failed: {error}");
            }
        }
    }

    async fn tick(&self) -> Result<(), GoalError> {
        for goal in self.goals.due(Utc::now()).await? {
            let goal_id = goal.public_id.clone();
            if let Err(error) = self.step(goal).await {
                tracing::warn!(goal_id, "AI goal step failed: {error}");
            }
        }
        Ok(())
    }

    async fn step(&self, goal: ai_goals::Model) -> Result<(), GoalError> {
        let now = Utc::now();
        // Reached, but the chat stayed busy until the deadline: the goal was
        // met, it is only the live check that did not run.
        if goal.deadline_at <= now && waits_for_live_check(&goal) {
            self.goals
                .record(
                    &goal,
                    STATUS_ACHIEVED,
                    "Reached; the live app was not checked because the chat stayed busy",
                    None,
                )
                .await?;
            return Ok(());
        }
        if goal.deadline_at <= now {
            self.goals
                .record(
                    &goal,
                    STATUS_FAILED,
                    "Gave up: the goal was not reached before its deadline",
                    None,
                )
                .await?;
            return Ok(());
        }
        let conversation = ai_conversations::Entity::find_by_id(goal.conversation_id)
            .one(self.db.as_ref())
            .await?;
        let Some(conversation) = conversation else {
            self.goals
                .record(&goal, STATUS_CANCELLED, "The chat no longer exists", None)
                .await?;
            return Ok(());
        };
        if conversation.status != "active" {
            self.goals
                .record(&goal, STATUS_CANCELLED, "The chat was archived", None)
                .await?;
            return Ok(());
        }
        let turn_running = conversation.turn_status == "running";
        if goal.status == STATUS_HEALING && turn_running {
            // The fix turn is still working; look again later.
            self.goals
                .record(
                    &goal,
                    STATUS_HEALING,
                    "Working on a fix",
                    Some(ACTIVE_CHECK_SECONDS),
                )
                .await?;
            return Ok(());
        }

        let auth = match self.conversations.authorization_for_goal(&goal).await {
            Ok(auth) => auth,
            Err(reason) => {
                self.goals
                    .record(
                        &goal,
                        STATUS_NEEDS_YOU,
                        &format!("Watching paused: {reason}. Open the chat to continue."),
                        Some(NEEDS_YOU_CHECK_SECONDS),
                    )
                    .await?;
                return Ok(());
            }
        };

        let observation = self.check(&goal, &auth).await;
        match observation {
            Observation::Achieved(text) => self.on_achieved(goal, conversation, auth, text).await?,
            Observation::Pending(text) => {
                // A healing goal whose fix turn already ended and which was not
                // re-targeted is waiting on the user, not on a turn.
                let (status, next) = match goal.status.as_str() {
                    STATUS_HEALING => (STATUS_NEEDS_YOU, NEEDS_YOU_CHECK_SECONDS),
                    STATUS_NEEDS_YOU => (STATUS_NEEDS_YOU, NEEDS_YOU_CHECK_SECONDS),
                    _ => (STATUS_ACTIVE, ACTIVE_CHECK_SECONDS),
                };
                self.goals.record(&goal, status, &text, Some(next)).await?;
            }
            Observation::Failed(text) => self.on_failed(goal, conversation, auth, text).await?,
        }
        Ok(())
    }

    async fn check(&self, goal: &ai_goals::Model, auth: &AuthContext) -> Observation {
        let Some(caller) = self.read_api.get() else {
            return Observation::Pending("The API is not ready yet".to_string());
        };
        let scope = ApiCallScope {
            auth: auth.clone(),
            project_scope: ProjectSelectorScope::Unrestricted,
            origin: None,
            secrets: None,
        };
        let call = caller.call(&goal.check_operation, goal.check_params.clone(), &scope);
        match tokio::time::timeout(CHECK_TIMEOUT, call).await {
            Ok(Ok(response)) => evaluate(goal, response.status, &response.body),
            Ok(Err(error)) => Observation::Pending(format!(
                "The check {} could not run: {error}",
                goal.check_operation
            )),
            Err(_) => Observation::Pending(format!(
                "The check {} took longer than {}s",
                goal.check_operation,
                CHECK_TIMEOUT.as_secs()
            )),
        }
    }

    async fn on_failed(
        &self,
        goal: ai_goals::Model,
        conversation: ai_conversations::Model,
        auth: AuthContext,
        observation: String,
    ) -> Result<(), GoalError> {
        if goal.status != STATUS_ACTIVE {
            // A goal already being healed, or waiting on the user, keeps
            // showing what it sees without spending another attempt.
            let status = if goal.status == STATUS_HEALING {
                STATUS_NEEDS_YOU
            } else {
                goal.status.as_str()
            };
            self.goals
                .record(&goal, status, &observation, Some(NEEDS_YOU_CHECK_SECONDS))
                .await?;
            return Ok(());
        }
        if goal.attempts >= goal.max_attempts {
            let text = format!(
                "Gave up after {} fix attempts. Last check: {observation}",
                goal.attempts
            );
            self.goals.record(&goal, STATUS_FAILED, &text, None).await?;
            return Ok(());
        }
        if conversation.turn_status == "running" {
            // The user is mid-conversation; do not talk over them.
            self.goals
                .record(
                    &goal,
                    STATUS_ACTIVE,
                    &format!("{observation}. Waiting for the current reply to finish"),
                    Some(ACTIVE_CHECK_SECONDS),
                )
                .await?;
            return Ok(());
        }
        // Conditional on the row as loaded: a goal the user cancelled or
        // re-pointed while its check was running starts no fix turn.
        let Some(goal) = self.goals.begin_heal(&goal).await? else {
            return Ok(());
        };
        let prompt = heal_prompt(&goal, &observation);
        let metadata = serde_json::json!({
            "origin": "goal",
            "goal_id": goal.public_id,
            "goal_title": goal.title,
            "attempt": goal.attempts,
        });
        let started = self
            .conversations
            .start_goal_turn(
                &conversation,
                &prompt,
                metadata,
                &auth,
                self.project_access_checker.clone(),
            )
            .await;
        match started {
            Ok(()) => {
                tracing::info!(
                    goal_id = %goal.public_id,
                    conversation_id = conversation.id,
                    attempt = goal.attempts,
                    "AI goal failed; started a fix turn as the owner"
                );
                self.audit_heal(&goal, &conversation).await;
                // A retarget from the fix turn, or a cancel, may already have
                // landed; `record` then leaves that newer state alone.
                self.goals
                    .record(
                        &goal,
                        STATUS_HEALING,
                        &format!("{observation}. Working on a fix"),
                        Some(ACTIVE_CHECK_SECONDS),
                    )
                    .await?;
            }
            Err(error) => {
                tracing::warn!(
                    goal_id = %goal.public_id,
                    conversation_id = conversation.id,
                    "could not start a fix turn: {error}"
                );
                self.goals
                    .abandon_heal(&goal, &format!("{observation}. Could not start a fix yet"))
                    .await?;
            }
        }
        Ok(())
    }

    async fn on_achieved(
        &self,
        goal: ai_goals::Model,
        conversation: ai_conversations::Model,
        auth: AuthContext,
        observation: String,
    ) -> Result<(), GoalError> {
        if !wants_live_check(&goal) {
            self.goals
                .record(&goal, STATUS_ACHIEVED, &observation, None)
                .await?;
            return Ok(());
        }
        if conversation.turn_status == "running" {
            // The user is mid-conversation; check once the reply is done.
            self.goals
                .record(
                    &goal,
                    STATUS_ACTIVE,
                    &format!("{LIVE_CHECK_WAITING}. {observation}"),
                    Some(ACTIVE_CHECK_SECONDS),
                )
                .await?;
            return Ok(());
        }
        // Close the goal first: an achieved goal is never looked at again, so
        // the check starts at most once whatever happens to the turn. When the
        // goal changed while it was checked (the user stopped it), nothing
        // was closed and no live check starts.
        let Some(goal) = self
            .goals
            .record(&goal, STATUS_ACHIEVED, &observation, None)
            .await?
        else {
            return Ok(());
        };
        let prompt = live_check_prompt(&goal, &observation);
        let metadata = serde_json::json!({
            "origin": "goal",
            "goal_id": goal.public_id,
            "goal_title": goal.title,
            "check": "live",
        });
        match self
            .conversations
            .start_goal_turn(
                &conversation,
                &prompt,
                metadata,
                &auth,
                self.project_access_checker.clone(),
            )
            .await
        {
            Ok(()) => {
                tracing::info!(
                    goal_id = %goal.public_id,
                    conversation_id = conversation.id,
                    "AI goal reached; started a live check as the owner"
                );
                self.audit_live_check(&goal, &conversation).await;
            }
            Err(error) => {
                tracing::warn!(
                    goal_id = %goal.public_id,
                    conversation_id = conversation.id,
                    "could not start the live check of a reached goal: {error}"
                );
                self.goals
                    .record(
                        &goal,
                        STATUS_ACHIEVED,
                        &format!("{observation}. The live app was not checked: {error}"),
                        None,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn audit_live_check(
        &self,
        goal: &ai_goals::Model,
        conversation: &ai_conversations::Model,
    ) {
        let entry = crate::audit::AiGoalLiveCheckStartedAudit {
            context: temps_core::AuditContext {
                user_id: goal.owner_user_id,
                ip_address: None,
                user_agent: "temps-ai-goal-watcher".to_string(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            goal_id: goal.public_id.clone(),
        };
        if let Err(error) = self.audit.create_audit_log(&entry).await {
            tracing::error!(
                goal_id = %goal.public_id,
                "failed to audit the goal live check: {error}"
            );
        }
    }

    async fn audit_heal(&self, goal: &ai_goals::Model, conversation: &ai_conversations::Model) {
        let entry = crate::audit::AiGoalHealStartedAudit {
            context: temps_core::AuditContext {
                user_id: goal.owner_user_id,
                ip_address: None,
                user_agent: "temps-ai-goal-watcher".to_string(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
            goal_id: goal.public_id.clone(),
            attempt: goal.attempts,
        };
        if let Err(error) = self.audit.create_audit_log(&entry).await {
            tracing::error!(
                goal_id = %goal.public_id,
                "failed to audit the goal fix turn: {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(success: &[&str], failure: &[&str]) -> ai_goals::Model {
        let now = Utc::now();
        ai_goals::Model {
            id: 1,
            public_id: "goal_test".into(),
            conversation_id: 7,
            owner_user_id: 3,
            title: "Deployment is healthy".into(),
            check_operation: "get_deployment".into(),
            check_params: serde_json::json!({"project_id": 1, "deployment_id": 2}),
            success_path: "status".into(),
            success_values: serde_json::json!(success),
            failure_values: serde_json::json!(failure),
            status: STATUS_ACTIVE.into(),
            attempts: 1,
            max_attempts: 3,
            last_observation: None,
            last_checked_at: None,
            next_check_at: now,
            deadline_at: now + chrono::Duration::minutes(30),
            auth_session_id: Some(9),
            created_at: now,
            updated_at: now,
        }
    }

    fn input() -> TrackGoalInput {
        TrackGoalInput {
            title: "Deployment is healthy".into(),
            operation_id: "get_deployment".into(),
            params: serde_json::json!({"project_id": 1}),
            success_path: "status".into(),
            success_values: vec!["completed".into()],
            failure_values: vec!["failed".into()],
            max_attempts: None,
            deadline_minutes: None,
        }
    }

    #[test]
    fn success_failure_and_pending_are_told_apart() {
        let g = goal(&["completed"], &["failed", "cancelled"]);
        assert!(matches!(
            evaluate(&g, 200, &serde_json::json!({"status": "Completed"})),
            Observation::Achieved(_)
        ));
        assert!(matches!(
            evaluate(&g, 200, &serde_json::json!({"status": "running"})),
            Observation::Pending(_)
        ));
        let failed = evaluate(
            &g,
            200,
            &serde_json::json!({"status": "failed", "error_message": "build exited 1"}),
        );
        assert_eq!(
            failed,
            Observation::Failed("status is failed: build exited 1".into())
        );
    }

    #[test]
    fn nested_paths_and_non_string_values_are_matched() {
        let mut g = goal(&["true"], &["false"]);
        g.success_path = "health.ready".into();
        assert!(matches!(
            evaluate(&g, 200, &serde_json::json!({"health": {"ready": true}})),
            Observation::Achieved(_)
        ));
        assert!(matches!(
            evaluate(&g, 200, &serde_json::json!({"health": {"ready": false}})),
            Observation::Failed(_)
        ));
        assert!(matches!(
            evaluate(&g, 200, &serde_json::json!({"health": {}})),
            Observation::Pending(_)
        ));
    }

    #[test]
    fn http_errors_are_classified_by_whether_waiting_can_help() {
        let g = goal(&["completed"], &["failed"]);
        for status in [401, 403, 404, 410] {
            assert!(
                matches!(
                    evaluate(&g, status, &serde_json::json!({})),
                    Observation::Failed(_)
                ),
                "{status} must fail the goal"
            );
        }
        for status in [429, 500, 502, 503] {
            assert!(
                matches!(
                    evaluate(&g, status, &serde_json::json!({})),
                    Observation::Pending(_)
                ),
                "{status} must be retried"
            );
        }
        assert!(matches!(
            evaluate(&g, 422, &serde_json::json!({"detail": "bad"})),
            Observation::Failed(_)
        ));
    }

    #[test]
    fn observations_are_clipped_to_one_short_line() {
        let g = goal(&["completed"], &["failed"]);
        let long = "line\n".repeat(400);
        let observation = evaluate(&g, 418, &serde_json::json!(long));
        assert!(observation.text().chars().count() < MAX_OBSERVATION_CHARS + 200);
        assert!(!observation.text().contains('\n'));
    }

    #[test]
    fn valid_input_passes_and_bad_input_names_the_problem() {
        assert!(input().validate(1).is_ok());

        let mut bad = input();
        bad.title = "  ".into();
        assert!(bad.validate(1).unwrap_err().to_string().contains("title"));

        let mut bad = input();
        bad.success_values.clear();
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("success_values"));

        let mut bad = input();
        bad.success_path = "status; drop".into();
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("success_path"));

        let mut bad = input();
        bad.failure_values = Vec::new();
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("failure_values must list at least one value"));

        let mut bad = input();
        bad.failure_values = vec!["COMPLETED".into()];
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("both a success value and a failure value"));

        let mut bad = input();
        bad.params = serde_json::json!([1]);
        assert!(bad.validate(1).unwrap_err().to_string().contains("params"));

        let mut bad = input();
        bad.max_attempts = Some(0);
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("max_attempts"));

        let mut bad = input();
        bad.deadline_minutes = Some(MAX_DEADLINE_MINUTES + 1);
        assert!(bad
            .validate(1)
            .unwrap_err()
            .to_string()
            .contains("deadline_minutes"));
    }

    #[test]
    fn heal_prompt_names_the_goal_attempt_and_retarget_instruction() {
        let g = goal(&["completed"], &["failed"]);
        let prompt = heal_prompt(&g, "status is failed: build exited 1");
        assert!(prompt.contains("goal_test"));
        assert!(prompt.contains("attempt 1 of 3"));
        assert!(prompt.contains("build exited 1"));
        assert!(prompt.contains("retarget"));
    }

    #[test]
    fn the_live_check_names_the_goal_and_asks_for_evidence_not_changes() {
        let g = goal(&["completed"], &["failed"]);
        let prompt = live_check_prompt(&g, "status is completed");
        assert!(prompt.contains("goal_test"));
        assert!(prompt.contains("status is completed"));
        assert!(prompt.contains("container logs"));
        assert!(prompt.contains("say so rather than assuming it works"));
        assert!(prompt.contains("`temps_goal`"));
        assert!(prompt.contains("Do not change anything that works"));
    }

    #[test]
    fn only_a_deployment_goal_gets_a_live_check() {
        let mut g = goal(&["completed"], &["failed"]);
        assert!(wants_live_check(&g));
        g.check_operation = "get_external_service".into();
        g.check_params = serde_json::json!({ "id": 3 });
        assert!(!wants_live_check(&g));
    }

    #[test]
    fn a_goal_waiting_for_its_live_check_is_told_apart_by_its_observation() {
        let mut g = goal(&["completed"], &["failed"]);
        assert!(!waits_for_live_check(&g));
        g.last_observation = Some(clip(&format!("{LIVE_CHECK_WAITING}. status is completed")));
        assert!(waits_for_live_check(&g));
        g.last_observation = Some("status is building".into());
        assert!(!waits_for_live_check(&g));
    }

    #[test]
    fn a_deployment_goal_exposes_only_the_two_ids() {
        let response = GoalResponse::from(goal(&["completed"], &["failed"]));
        assert_eq!(
            response.deployment,
            Some(GoalDeployment {
                project_id: 1,
                deployment_id: Some(2)
            })
        );
    }

    #[test]
    fn deployment_ids_are_read_from_numbers_or_numeric_strings_only() {
        let ids = |params: Value| watched_deployment("get_deployment", &params);
        assert!(ids(serde_json::json!({"project_id": "4", "deployment_id": " 9 "})).is_some());
        assert!(ids(serde_json::json!({"project_id": 4})).is_none());
        assert!(ids(serde_json::json!({"project_id": 0, "deployment_id": 9})).is_none());
        assert!(ids(serde_json::json!({"project_id": "x", "deployment_id": 9})).is_none());
        assert!(
            ids(serde_json::json!({"project_id": 4, "deployment_id": 99999999999_i64})).is_none()
        );
        assert!(watched_deployment(
            "get_service",
            &serde_json::json!({"project_id": 4, "deployment_id": 9})
        )
        .is_none());
    }

    #[test]
    fn latest_deployment_checks_name_only_the_project() {
        for operation in ["get_last_deployment", "get_project_deployments"] {
            assert_eq!(
                watched_deployment(operation, &serde_json::json!({"id": 7})),
                Some(GoalDeployment {
                    project_id: 7,
                    deployment_id: None
                }),
                "{operation} with the path parameter"
            );
        }
        assert_eq!(
            watched_deployment(
                "get_last_deployment",
                &serde_json::json!({"project_id": "3"})
            ),
            Some(GoalDeployment {
                project_id: 3,
                deployment_id: None
            })
        );
        assert!(watched_deployment("get_last_deployment", &serde_json::json!({})).is_none());
        assert!(
            watched_deployment("get_last_deployment", &serde_json::json!({"id": -2})).is_none()
        );
    }

    #[test]
    fn response_uses_iso_timestamps_and_hides_the_check_params() {
        let response = GoalResponse::from(goal(&["completed"], &["failed"]));
        let json = serde_json::to_value(&response).unwrap();
        assert!(
            json["deadline_at"].as_str().unwrap().ends_with("+00:00")
                || json["deadline_at"].as_str().unwrap().ends_with('Z')
        );
        assert!(json.get("check_params").is_none());
        assert_eq!(json["id"], "goal_test");
    }

    fn count_row(n: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        std::collections::BTreeMap::from([(
            "num_items".to_string(),
            sea_orm::Value::BigInt(Some(n)),
        )])
    }

    fn service(db: sea_orm::DatabaseConnection) -> GoalService {
        GoalService::new(Arc::new(db))
    }

    #[tokio::test]
    async fn track_stores_an_active_goal_for_the_owner() {
        let stored = goal(&["completed"], &["failed"]);
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(0)]])
            .append_query_results([vec![stored.clone()]])
            .into_connection();
        let goal = service(db).track(7, 3, Some(9), input()).await.unwrap();
        assert_eq!(goal.conversation_id, 7);
        assert_eq!(goal.owner_user_id, 3);
        assert_eq!(goal.status, STATUS_ACTIVE);
    }

    #[tokio::test]
    async fn track_refuses_a_sixth_open_goal_and_names_the_limit() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(MAX_OPEN_GOALS_PER_CONVERSATION as i64)]])
            .into_connection();
        let error = service(db).track(7, 3, None, input()).await.unwrap_err();
        assert!(matches!(
            error,
            GoalError::TooManyOpen {
                conversation_id: 7,
                open: 5,
                limit: 5
            }
        ));
        assert!(error.to_string().contains("Conversation 7"));
    }

    #[tokio::test]
    async fn track_rejects_invalid_input_before_touching_the_database() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let mut bad = input();
        bad.success_values.clear();
        let error = service(db).track(7, 3, None, bad).await.unwrap_err();
        assert!(matches!(
            error,
            GoalError::Validation {
                conversation_id: 7,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_missing_goal_is_not_found_with_both_ids() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_goals::Model>::new()])
            .into_connection();
        let error = service(db).get(7, "goal_nope").await.unwrap_err();
        assert!(matches!(
            &error,
            GoalError::NotFound { goal_id, conversation_id: 7 } if goal_id == "goal_nope"
        ));
    }

    #[tokio::test]
    async fn a_finished_goal_cannot_be_cancelled_or_retargeted() {
        let mut achieved = goal(&["completed"], &["failed"]);
        achieved.status = STATUS_ACHIEVED.into();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![achieved.clone()], vec![achieved.clone()]])
            .into_connection();
        let svc = service(db);
        assert!(matches!(
            svc.cancel(7, "goal_test").await.unwrap_err(),
            GoalError::InvalidState {
                action: "cancelled",
                ..
            }
        ));
        assert!(matches!(
            svc.retarget(7, "goal_test", input()).await.unwrap_err(),
            GoalError::InvalidState {
                action: "retargeted",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn only_failed_or_waiting_goals_can_be_retried() {
        let mut active = goal(&["completed"], &["failed"]);
        active.status = STATUS_ACTIVE.into();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![active]])
            .into_connection();
        assert!(matches!(
            service(db).retry(7, "goal_test").await.unwrap_err(),
            GoalError::InvalidState {
                action: "retried",
                ..
            }
        ));
    }

    /// The SQL the mock database saw, once every holder of it is dropped.
    fn statements(db: Arc<sea_orm::DatabaseConnection>) -> Vec<String> {
        let db = Arc::try_unwrap(db).expect("every holder of the mock database was dropped");
        db.into_transaction_log()
            .iter()
            .flat_map(|transaction| {
                transaction
                    .statements()
                    .iter()
                    .map(|statement| statement.sql.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The conditions of a statement, without its `RETURNING` column list.
    fn where_clause(sql: &str) -> &str {
        let conditions = sql.split_once("WHERE").map(|(_, rest)| rest).unwrap_or("");
        conditions
            .split_once("RETURNING")
            .map(|(conditions, _)| conditions)
            .unwrap_or(conditions)
    }

    fn mock(results: Vec<Vec<ai_goals::Model>>) -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(results)
                .into_connection(),
        )
    }

    #[tokio::test]
    async fn a_check_is_not_written_over_a_goal_that_changed_while_it_ran() {
        let loaded = goal(&["completed"], &["failed"]);
        let db = mock(vec![vec![]]);
        let svc = GoalService::new(db.clone());
        let stored = svc
            .record(&loaded, STATUS_ACTIVE, "status is running", Some(15))
            .await
            .unwrap();
        assert!(stored.is_none());
        drop(svc);
        let sql = statements(db);
        assert_eq!(sql.len(), 1);
        let guard = where_clause(&sql[0]);
        assert!(guard.contains(r#""status" = $"#), "{}", sql[0]);
        assert!(guard.contains(r#""updated_at" = $"#), "{}", sql[0]);
    }

    #[tokio::test]
    async fn a_check_on_an_unchanged_goal_returns_the_stored_row() {
        let loaded = goal(&["completed"], &["failed"]);
        let mut stored = loaded.clone();
        stored.last_observation = Some("status is running".into());
        let svc = GoalService::new(mock(vec![vec![stored.clone()]]));
        let written = svc
            .record(&loaded, STATUS_ACTIVE, "status is running", Some(15))
            .await
            .unwrap();
        assert_eq!(written, Some(stored));
    }

    #[tokio::test]
    async fn cancel_wins_whatever_watched_status_the_watcher_moved_the_goal_to() {
        let loaded = goal(&["completed"], &["failed"]);
        let mut cancelled = loaded.clone();
        cancelled.status = STATUS_CANCELLED.into();
        let db = mock(vec![vec![loaded], vec![cancelled]]);
        let svc = GoalService::new(db.clone());
        let goal = svc.cancel(7, "goal_test").await.unwrap();
        assert_eq!(goal.status, STATUS_CANCELLED);
        drop(svc);
        let sql = statements(db);
        let update = sql.last().expect("an update was sent");
        assert!(update.starts_with("UPDATE"), "{update}");
        // Conditional on any open status, not on the one read a moment ago.
        assert!(
            where_clause(update).contains(r#""status" IN ("#),
            "{update}"
        );
        assert!(!where_clause(update).contains("updated_at"), "{update}");
    }

    #[tokio::test]
    async fn a_cancel_that_lost_to_a_closing_write_names_the_new_status() {
        let loaded = goal(&["completed"], &["failed"]);
        let mut achieved = loaded.clone();
        achieved.status = STATUS_ACHIEVED.into();
        let svc = GoalService::new(mock(vec![vec![loaded], vec![], vec![achieved]]));
        let error = svc.cancel(7, "goal_test").await.unwrap_err();
        assert!(matches!(
            &error,
            GoalError::InvalidState { status, action: "cancelled", .. } if status == STATUS_ACHIEVED
        ));
    }

    #[tokio::test]
    async fn a_retry_of_a_goal_cancelled_meanwhile_is_refused() {
        let mut waiting = goal(&["completed"], &["failed"]);
        waiting.status = STATUS_NEEDS_YOU.into();
        let mut cancelled = waiting.clone();
        cancelled.status = STATUS_CANCELLED.into();
        let svc = GoalService::new(mock(vec![vec![waiting], vec![], vec![cancelled]]));
        assert!(matches!(
            svc.retry(7, "goal_test").await.unwrap_err(),
            GoalError::InvalidState { status, action: "retried", .. } if status == STATUS_CANCELLED
        ));
    }

    #[tokio::test]
    async fn a_retarget_of_a_goal_cancelled_meanwhile_is_refused() {
        let healing = {
            let mut g = goal(&["completed"], &["failed"]);
            g.status = STATUS_HEALING.into();
            g
        };
        let mut cancelled = healing.clone();
        cancelled.status = STATUS_CANCELLED.into();
        let db = mock(vec![vec![healing], vec![], vec![cancelled]]);
        let svc = GoalService::new(db.clone());
        assert!(matches!(
            svc.retarget(7, "goal_test", input()).await.unwrap_err(),
            GoalError::InvalidState { status, action: "retargeted", .. } if status == STATUS_CANCELLED
        ));
        drop(svc);
        let sql = statements(db);
        assert!(
            where_clause(&sql[1]).contains(r#""status" NOT IN ("#),
            "{}",
            sql[1]
        );
    }

    struct NoModel;

    #[async_trait::async_trait]
    impl temps_ai::AiService for NoModel {
        async fn is_available(&self) -> bool {
            false
        }

        async fn complete(
            &self,
            _request: temps_ai::AiRequest,
        ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn chat_stream(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn chat_stream_turn(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::ChatTurnStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }
    }

    struct NoAudit;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }

    fn watcher(db: &Arc<sea_orm::DatabaseConnection>) -> GoalWatcher {
        GoalWatcher::new(
            Arc::new(GoalService::new(db.clone())),
            db.clone(),
            Arc::new(ApiToolsHandle::new()),
            Arc::new(ConversationService::new(
                db.clone(),
                Arc::new(NoModel),
                Vec::new(),
            )),
            None,
            Arc::new(NoAudit),
        )
    }

    fn chat(turn_status: &str) -> ai_conversations::Model {
        ai_conversations::Model {
            id: 7,
            public_id: "conv_goal_7".to_string(),
            project_id: Some(1),
            application_id: None,
            context_type: "assistant".to_string(),
            context_id: "assistant_1".to_string(),
            title: None,
            status: "active".to_string(),
            kind: "ai".to_string(),
            created_by: 3,
            metadata: None,
            created_at: Utc::now(),
            last_activity_at: Utc::now(),
            ai_provider: "gateway".to_string(),
            ai_model: "vendor/chat-model".to_string(),
            ai_thinking_level: None,
            ai_permission_mode: "ask".to_string(),
            cli_session_id: None,
            cli_session_fingerprint: None,
            turn_status: turn_status.to_string(),
            active_turn_id: None,
            last_turn_id: None,
            turn_started_at: None,
            next_step_suggestions: None,
        }
    }

    fn owner() -> AuthContext {
        let user = temps_entities::users::Model {
            id: 3,
            name: "owner".to_string(),
            email: "owner@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        AuthContext::new_session(user, temps_auth::permissions::Role::User)
    }

    #[tokio::test]
    async fn a_goal_stopped_during_its_check_starts_no_fix_turn() {
        // The check failed, but the conditional claim of the fix matched no
        // row: the goal is no longer the active one the watcher read.
        let db = mock(vec![vec![]]);
        let w = watcher(&db);
        w.on_failed(
            goal(&["completed"], &["failed"]),
            chat("idle"),
            owner(),
            "status is failed".into(),
        )
        .await
        .unwrap();
        drop(w);
        let sql = statements(db);
        assert_eq!(sql.len(), 1, "nothing follows the lost claim: {sql:?}");
        assert!(
            where_clause(&sql[0]).contains(r#""updated_at" = $"#),
            "{}",
            sql[0]
        );
    }

    #[tokio::test]
    async fn a_goal_stopped_during_its_check_is_not_set_back_to_active() {
        let db = mock(vec![vec![]]);
        let w = watcher(&db);
        // The reply is still running, so the watcher only records and waits;
        // its write matches nothing and it does not try again.
        w.on_failed(
            goal(&["completed"], &["failed"]),
            chat("running"),
            owner(),
            "status is failed".into(),
        )
        .await
        .unwrap();
        drop(w);
        assert_eq!(statements(db).len(), 1);
    }

    #[tokio::test]
    async fn a_goal_stopped_during_its_check_starts_no_live_check() {
        let db = mock(vec![vec![]]);
        let w = watcher(&db);
        w.on_achieved(
            goal(&["completed"], &["failed"]),
            chat("idle"),
            owner(),
            "status is completed".into(),
        )
        .await
        .unwrap();
        drop(w);
        let sql = statements(db);
        assert_eq!(sql.len(), 1, "no turn was claimed: {sql:?}");
        assert!(sql[0].starts_with("UPDATE"), "{}", sql[0]);
    }
}
