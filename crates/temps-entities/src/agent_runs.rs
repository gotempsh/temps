// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "agent_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// Pointer into project_agents. Nullable because ephemeral runs (source =
    /// "cli_ephemeral") and historical autofixer runs do not have a parent
    /// agent record. Prefer reading `agent_id` instead.
    pub config_id: Option<i32>,
    /// The agent that created this run (replaces config_id for new runs)
    pub agent_id: Option<i32>,
    pub trigger_type: String,
    pub trigger_source_id: Option<i32>,
    pub trigger_source_type: Option<String>,
    pub status: String,
    pub branch_name: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub preview_url: Option<String>,
    pub preview_deployment_id: Option<i32>,
    pub error_message: Option<String>,
    pub ai_output: Option<String>,
    pub ai_reasoning: Option<String>,
    pub ai_model: Option<String>,
    pub ai_provider: Option<String>,
    pub tokens_input: i32,
    pub tokens_output: i32,
    pub estimated_cost_cents: i32,
    pub files_changed: i32,
    /// Autofixer phase: "analyzing", "analyzed", "fixing", "fix_ready", or NULL for agent runs
    pub phase: Option<String>,
    /// Root cause analysis result (autofixer only)
    pub analysis: Option<String>,
    /// Additional context provided by the user during the run
    pub user_context: Option<String>,
    /// Claude CLI session UUID for resuming conversations via `--resume`
    pub ai_session_id: Option<String>,
    pub started_at: Option<DBDateTime>,
    pub completed_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    /// Where this run's config came from. `committed` (default — read from
    /// `project_agents`) or `cli_ephemeral` (read from `ephemeral_yaml`).
    pub source: String,
    /// Full WorkflowYamlConfig as YAML text. Only set when source =
    /// "cli_ephemeral". Lets the executor build a synthetic config without a
    /// project_agents row.
    pub ephemeral_yaml: Option<String>,
    /// The final assembled prompt the AI CLI actually saw (trigger context
    /// block + YAML prompt, with error-group fields interpolated). Captured
    /// once per run, immediately before the CLI invocation. Nullable because
    /// historical rows never persisted it.
    pub prompt_text: Option<String>,
    /// Docker volume (e.g. `temps-wfrun-123`) that backs this run's
    /// `/workspace`. Populated when the run starts. NOT deleted on run
    /// finish — it's retained so the user can open a follow-up workspace
    /// sandbox mounting the exact same filesystem (including `.git` and
    /// any unpushed commits the AI produced). The TTL sweeper is the only
    /// thing that removes these automatically. Nullable for historical
    /// rows and for runs that failed before volume creation.
    pub workspace_volume: Option<String>,
    /// Per-run AI configuration chosen by the user when starting the run
    /// (autofixer only): provider, model, max_turns, branch. Kept as JSON so
    /// the retry dialog can prefill exactly what the run executed with.
    /// Null for historical rows and generic agent runs.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub run_config: Option<Json>,
    /// Authenticated user who started this run (e.g. clicked "Analyze" in
    /// the autofixer UI). Used to attribute the run's sandbox row so it
    /// appears in that user's sandbox list. NULL for webhook-triggered
    /// runs and historical rows.
    pub triggered_by_user_id: Option<i32>,
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
    #[sea_orm(
        belongs_to = "super::project_agents::Entity",
        from = "Column::AgentId",
        to = "super::project_agents::Column::Id",
        on_delete = "SetNull"
    )]
    Agent,
    #[sea_orm(has_many = "super::agent_run_logs::Entity")]
    Logs,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::project_agents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Agent.def()
    }
}

impl Related<super::agent_run_logs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Logs.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
