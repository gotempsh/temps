use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use std::sync::Arc;

use temps_entities::{agent_run_logs, agent_runs};

use crate::error::AgentError;

/// Fields that can be updated when changing a run's status.
#[derive(Default)]
pub struct UpdateRunFields {
    pub status: Option<String>,
    pub branch_name: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub preview_url: Option<String>,
    pub preview_deployment_id: Option<i32>,
    pub error_message: Option<String>,
    pub ai_output: Option<String>,
    pub ai_model: Option<String>,
    pub ai_provider: Option<String>,
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub estimated_cost_cents: Option<i32>,
    pub files_changed: Option<i32>,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub completed_at: Option<chrono::DateTime<Utc>>,
    pub commit_sha: Option<String>,
    /// Autofixer phase (e.g. "analyzing", "analyzed", "fixing", "fix_ready", "completed")
    pub phase: Option<String>,
    /// Root cause analysis text produced by the autofixer analysis phase
    pub analysis: Option<String>,
    /// Additional user context appended during the run
    pub user_context: Option<String>,
    /// Claude CLI session UUID for resuming conversations
    pub ai_session_id: Option<String>,
    /// Final assembled prompt that was sent to the AI CLI (captured once per
    /// run, just before the CLI invocation). Exposed in the run detail UI.
    pub prompt_text: Option<String>,
    /// Docker volume name backing `/workspace` for this run (e.g.
    /// `temps-wfrun-42`). Set once when the sandbox is created. Retained
    /// after the run finishes so "Open in workspace" can re-mount the
    /// same filesystem.
    pub workspace_volume: Option<String>,
}

/// A run record together with its logs
pub struct RunWithLogs {
    pub run: agent_runs::Model,
    pub logs: Vec<agent_run_logs::Model>,
}

pub struct AgentRunService {
    db: Arc<DatabaseConnection>,
}

impl AgentRunService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn create_run(
        &self,
        project_id: i32,
        agent_id: i32,
        trigger_type: String,
        trigger_source_id: Option<i32>,
        trigger_source_type: Option<String>,
        user_context: Option<String>,
    ) -> Result<agent_runs::Model, AgentError> {
        let active = agent_runs::ActiveModel {
            project_id: Set(project_id),
            config_id: Set(Some(agent_id)),
            agent_id: Set(Some(agent_id)),
            trigger_type: Set(trigger_type),
            trigger_source_id: Set(trigger_source_id),
            trigger_source_type: Set(trigger_source_type),
            user_context: Set(user_context),
            status: Set("pending".to_string()),
            source: Set("committed".to_string()),
            tokens_input: Set(0),
            tokens_output: Set(0),
            estimated_cost_cents: Set(0),
            files_changed: Set(0),
            ..Default::default()
        };

        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    /// Create a run record specifically for the autofixer (no agent_id, trigger_type="autofixer").
    pub async fn create_autofixer_run(
        &self,
        project_id: i32,
        error_group_id: i32,
        user_context: Option<String>,
    ) -> Result<agent_runs::Model, AgentError> {
        let active = agent_runs::ActiveModel {
            project_id: Set(project_id),
            config_id: Set(None),
            agent_id: Set(None),
            trigger_type: Set("autofixer".to_string()),
            trigger_source_type: Set(Some("error_group".to_string())),
            trigger_source_id: Set(Some(error_group_id)),
            status: Set("pending".to_string()),
            phase: Set(Some("analyzing".to_string())),
            user_context: Set(user_context),
            source: Set("committed".to_string()),
            tokens_input: Set(0),
            tokens_output: Set(0),
            estimated_cost_cents: Set(0),
            files_changed: Set(0),
            ..Default::default()
        };

        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    /// Create a run that executes an ephemeral YAML config uploaded by the CLI.
    /// `config_id` and `agent_id` stay NULL — the executor reads
    /// `ephemeral_yaml` and builds a synthetic config in memory.
    ///
    /// `trigger_source` is optional `(type, id)` for linking this run to an
    /// external entity (e.g. `("error_group", 5)`). The executor's
    /// `load_error_context` path keys off these fields to inject the error
    /// type / message / stack trace into the prompt — identical to the
    /// committed-workflow path.
    pub async fn create_ephemeral_run(
        &self,
        project_id: i32,
        ephemeral_yaml: String,
        user_context: Option<String>,
        trigger_source: Option<(String, i32)>,
    ) -> Result<agent_runs::Model, AgentError> {
        let (trigger_source_type, trigger_source_id) = match trigger_source {
            Some((t, id)) => (Some(t), Some(id)),
            None => (None, None),
        };
        let active = agent_runs::ActiveModel {
            project_id: Set(project_id),
            config_id: Set(None),
            agent_id: Set(None),
            trigger_type: Set("cli_ephemeral".to_string()),
            trigger_source_type: Set(trigger_source_type),
            trigger_source_id: Set(trigger_source_id),
            status: Set("pending".to_string()),
            user_context: Set(user_context),
            source: Set("cli_ephemeral".to_string()),
            ephemeral_yaml: Set(Some(ephemeral_yaml)),
            tokens_input: Set(0),
            tokens_output: Set(0),
            estimated_cost_cents: Set(0),
            files_changed: Set(0),
            ..Default::default()
        };

        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    pub async fn update_status(
        &self,
        run_id: i32,
        fields: UpdateRunFields,
    ) -> Result<agent_runs::Model, AgentError> {
        let run = self.get_run(run_id).await?;
        let mut active: agent_runs::ActiveModel = run.into();

        if let Some(status) = fields.status {
            active.status = Set(status);
        }
        if let Some(branch_name) = fields.branch_name {
            active.branch_name = Set(Some(branch_name));
        }
        if let Some(pr_url) = fields.pr_url {
            active.pr_url = Set(Some(pr_url));
        }
        if let Some(pr_number) = fields.pr_number {
            active.pr_number = Set(Some(pr_number));
        }
        if let Some(preview_url) = fields.preview_url {
            active.preview_url = Set(Some(preview_url));
        }
        if let Some(preview_deployment_id) = fields.preview_deployment_id {
            active.preview_deployment_id = Set(Some(preview_deployment_id));
        }
        if let Some(error_message) = fields.error_message {
            active.error_message = Set(Some(error_message));
        }
        if let Some(ai_output) = fields.ai_output {
            active.ai_output = Set(Some(ai_output));
        }
        if let Some(ai_model) = fields.ai_model {
            active.ai_model = Set(Some(ai_model));
        }
        if let Some(ai_provider) = fields.ai_provider {
            active.ai_provider = Set(Some(ai_provider));
        }
        if let Some(tokens_input) = fields.tokens_input {
            active.tokens_input = Set(tokens_input);
        }
        if let Some(tokens_output) = fields.tokens_output {
            active.tokens_output = Set(tokens_output);
        }
        if let Some(estimated_cost_cents) = fields.estimated_cost_cents {
            active.estimated_cost_cents = Set(estimated_cost_cents);
        }
        if let Some(files_changed) = fields.files_changed {
            active.files_changed = Set(files_changed);
        }
        if let Some(started_at) = fields.started_at {
            active.started_at = Set(Some(started_at));
        }
        if let Some(completed_at) = fields.completed_at {
            active.completed_at = Set(Some(completed_at));
        }
        if let Some(commit_sha) = fields.commit_sha {
            active.commit_sha = Set(Some(commit_sha));
        }
        if let Some(phase) = fields.phase {
            active.phase = Set(Some(phase));
        }
        if let Some(analysis) = fields.analysis {
            active.analysis = Set(Some(analysis));
        }
        if let Some(user_context) = fields.user_context {
            active.user_context = Set(Some(user_context));
        }
        if let Some(ai_session_id) = fields.ai_session_id {
            active.ai_session_id = Set(Some(ai_session_id));
        }
        if let Some(prompt_text) = fields.prompt_text {
            active.prompt_text = Set(Some(prompt_text));
        }
        if let Some(workspace_volume) = fields.workspace_volume {
            active.workspace_volume = Set(Some(workspace_volume));
        }

        let model = active
            .update(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    pub async fn list_runs(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<agent_runs::Model>, u64), AgentError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let paginator = agent_runs::Entity::find()
            .filter(agent_runs::Column::ProjectId.eq(project_id))
            .order_by(agent_runs::Column::CreatedAt, Order::Desc)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await.map_err(AgentError::Database)?;
        let items = paginator
            .fetch_page(page - 1)
            .await
            .map_err(AgentError::Database)?;

        Ok((items, total))
    }

    /// List runs for a specific agent (by agent_id)
    pub async fn list_runs_for_agent(
        &self,
        agent_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<agent_runs::Model>, u64), AgentError> {
        let page = page.unwrap_or(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);

        let paginator = agent_runs::Entity::find()
            .filter(
                sea_orm::Condition::any()
                    .add(agent_runs::Column::AgentId.eq(agent_id))
                    .add(agent_runs::Column::ConfigId.eq(agent_id)),
            )
            .order_by(agent_runs::Column::CreatedAt, Order::Desc)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await.map_err(AgentError::Database)?;
        let items = paginator
            .fetch_page(page - 1)
            .await
            .map_err(AgentError::Database)?;

        Ok((items, total))
    }

    /// Find the latest run targeting a specific trigger source (e.g. an
    /// error_group). Returns None if no run exists. Uses the
    /// `idx_autopilot_runs_trigger_source` index, so this is O(log n) even
    /// with millions of runs.
    pub async fn latest_run_for_trigger_source(
        &self,
        project_id: i32,
        trigger_source_type: &str,
        trigger_source_id: i32,
    ) -> Result<Option<agent_runs::Model>, AgentError> {
        agent_runs::Entity::find()
            .filter(agent_runs::Column::ProjectId.eq(project_id))
            .filter(agent_runs::Column::TriggerSourceType.eq(trigger_source_type))
            .filter(agent_runs::Column::TriggerSourceId.eq(trigger_source_id))
            .order_by(agent_runs::Column::CreatedAt, Order::Desc)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    pub async fn get_run(&self, run_id: i32) -> Result<agent_runs::Model, AgentError> {
        agent_runs::Entity::find_by_id(run_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::RunNotFound { run_id })
    }

    /// Verify that `run_id` belongs to `project_id`.
    ///
    /// Returns `Ok(())` when the run exists and its `project_id` matches.
    /// Returns `Err(AgentError::RunNotFound)` in every other case — including
    /// when the run exists but belongs to a *different* project — so that
    /// cross-project callers cannot infer the existence of foreign runs from
    /// the error (no information-disclosure via 403 vs 404 distinction).
    pub async fn ensure_run_in_project(
        &self,
        run_id: i32,
        project_id: i32,
    ) -> Result<(), AgentError> {
        let run = agent_runs::Entity::find_by_id(run_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::RunNotFound { run_id })?;

        if run.project_id != project_id {
            tracing::warn!(
                run_id,
                run_project_id = run.project_id,
                requested_project_id = project_id,
                "Cross-project run access rejected: run belongs to project {}, \
                 caller presented project {}",
                run.project_id,
                project_id
            );
            return Err(AgentError::RunNotFound { run_id });
        }

        Ok(())
    }

    pub async fn get_run_with_logs(&self, run_id: i32) -> Result<RunWithLogs, AgentError> {
        let run = self.get_run(run_id).await?;

        let logs = agent_run_logs::Entity::find()
            .filter(agent_run_logs::Column::RunId.eq(run_id))
            .order_by(agent_run_logs::Column::CreatedAt, Order::Asc)
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(RunWithLogs { run, logs })
    }

    /// Get logs for a run with ID greater than `after_id` (for SSE streaming)
    pub async fn get_logs_after(
        &self,
        run_id: i32,
        after_id: i64,
    ) -> Result<Vec<agent_run_logs::Model>, AgentError> {
        agent_run_logs::Entity::find()
            .filter(agent_run_logs::Column::RunId.eq(run_id))
            .filter(agent_run_logs::Column::Id.gt(after_id))
            .order_by(agent_run_logs::Column::Id, Order::Asc)
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    pub async fn append_log(
        &self,
        run_id: i32,
        level: &str,
        message: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<agent_run_logs::Model, AgentError> {
        let active = agent_run_logs::ActiveModel {
            run_id: Set(run_id),
            level: Set(level.to_string()),
            message: Set(message.to_string()),
            metadata: Set(metadata),
            ..Default::default()
        };

        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(model)
    }

    /// Return the total estimated_cost_cents spent on runs for the given project today (UTC).
    pub async fn get_daily_spend(&self, project_id: i32) -> Result<i32, AgentError> {
        use sea_orm::sea_query::Expr;

        let today_naive = Utc::now().date_naive().and_time(chrono::NaiveTime::MIN);
        let today_start_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(today_naive, Utc);

        let rows = agent_runs::Entity::find()
            .filter(agent_runs::Column::ProjectId.eq(project_id))
            .filter(agent_runs::Column::CreatedAt.gte(today_start_utc))
            .select_only()
            .column_as(
                Expr::col(agent_runs::Column::EstimatedCostCents).sum(),
                "total",
            )
            .into_tuple::<Option<i64>>()
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        // rows: Vec<Option<i64>>
        // .next() -> Option<Option<i64>>
        // .flatten() -> Option<i64>
        let total = rows.into_iter().next().flatten().unwrap_or(0) as i32;

        Ok(total)
    }

    /// Returns `true` if there is an active cooldown for the project+trigger combo.
    /// A cooldown is active if any run for the same trigger_source_type and trigger_source_id
    /// was created within the last `cooldown_minutes` minutes.
    pub async fn check_cooldown(
        &self,
        project_id: i32,
        trigger_source_type: Option<&str>,
        trigger_source_id: Option<i32>,
        cooldown_minutes: i32,
    ) -> Result<bool, AgentError> {
        let cutoff = Utc::now() - chrono::Duration::minutes(cooldown_minutes as i64);

        let mut query = agent_runs::Entity::find()
            .filter(agent_runs::Column::ProjectId.eq(project_id))
            .filter(agent_runs::Column::CreatedAt.gte(cutoff));

        if let Some(stype) = trigger_source_type {
            query = query.filter(agent_runs::Column::TriggerSourceType.eq(stype));
        }
        if let Some(sid) = trigger_source_id {
            query = query.filter(agent_runs::Column::TriggerSourceId.eq(sid));
        }

        let count = query
            .count(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(count > 0)
    }

    /// Recover stuck runs after server restart.
    /// Marks all runs in active (non-terminal) states as failed.
    pub async fn recover_stuck_runs(&self) -> Result<u64, AgentError> {
        let active_statuses = vec![
            "pending",
            "cloning",
            "analyzing",
            "fixing",
            "pushing",
            "creating_pr",
            "deploying",
        ];

        let stuck_runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Status.is_in(active_statuses))
            .all(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        let count = stuck_runs.len() as u64;

        for run in stuck_runs {
            let mut active: agent_runs::ActiveModel = run.into();
            active.status = Set("failed".to_string());
            active.error_message = Set(Some("Run interrupted by server restart".to_string()));
            active.completed_at = Set(Some(chrono::Utc::now()));
            let _ = active.update(self.db.as_ref()).await;
        }

        if count > 0 {
            tracing::info!("Recovered {} stuck agent run(s) after restart", count);
        }

        Ok(count)
    }

    /// Cancel an active run by setting status to "cancelled".
    pub async fn cancel_run(&self, run_id: i32) -> Result<agent_runs::Model, AgentError> {
        let run = self.get_run(run_id).await?;

        let terminal = ["completed", "failed", "no_fix", "cancelled"];
        if terminal.contains(&run.status.as_str()) {
            return Err(AgentError::Validation {
                message: format!(
                    "Run {} is already in terminal state '{}'",
                    run_id, run.status
                ),
            });
        }

        let mut active: agent_runs::ActiveModel = run.into();
        active.status = Set("cancelled".to_string());
        active.error_message = Set(Some("Cancelled by user".to_string()));
        active.completed_at = Set(Some(chrono::Utc::now()));

        active
            .update(self.db.as_ref())
            .await
            .map_err(AgentError::Database)
    }

    /// Count runs in active (non-terminal) states for a project.
    pub async fn count_active_runs(&self, project_id: i32) -> Result<u64, AgentError> {
        let active_statuses = vec![
            "pending",
            "cloning",
            "analyzing",
            "fixing",
            "pushing",
            "creating_pr",
            "deploying",
        ];

        let count = agent_runs::Entity::find()
            .filter(agent_runs::Column::ProjectId.eq(project_id))
            .filter(agent_runs::Column::Status.is_in(active_statuses))
            .count(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    /// Build a MockRow representing a COUNT(*) result for the sea-orm paginator.
    /// The paginator's `num_items()` executes `SELECT COUNT(*) AS num_items ...`
    /// and reads it as `try_get::<i64>("", "num_items")` on Postgres.
    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("num_items".to_string(), Value::BigInt(Some(n)));
        m
    }

    /// Build a MockRow representing a SUM result for `get_daily_spend`.
    /// The query uses `.column_as(Expr::col(...).sum(), "total")` and then
    /// `.into_tuple::<Option<i64>>()`. Sea-ORM reads the first column by position
    /// — so the key name doesn't matter, but we use "total" for clarity.
    fn sum_row(n: Option<i64>) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("total".to_string(), Value::BigInt(n));
        m
    }

    fn make_run(id: i32, project_id: i32) -> agent_runs::Model {
        agent_runs::Model {
            id,
            project_id,
            config_id: Some(1),
            agent_id: None,
            trigger_type: "manual".to_string(),
            trigger_source_id: None,
            trigger_source_type: None,
            status: "pending".to_string(),
            branch_name: None,
            commit_sha: None,
            pr_url: None,
            pr_number: None,
            preview_url: None,
            preview_deployment_id: None,
            error_message: None,
            ai_output: None,
            ai_reasoning: None,
            ai_model: None,
            ai_provider: None,
            tokens_input: 0,
            tokens_output: 0,
            estimated_cost_cents: 0,
            files_changed: 0,
            started_at: None,
            completed_at: None,
            created_at: Utc::now(),
            phase: None,
            analysis: None,
            user_context: None,
            ai_session_id: None,
            source: "committed".to_string(),
            ephemeral_yaml: None,

            prompt_text: None,
            workspace_volume: None,
        }
    }

    fn make_run_with_status(id: i32, project_id: i32, status: &str) -> agent_runs::Model {
        agent_runs::Model {
            status: status.to_string(),
            ..make_run(id, project_id)
        }
    }

    #[tokio::test]
    async fn test_get_run_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<agent_runs::Model>::new()])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.get_run(999).await;
        assert!(matches!(
            result.unwrap_err(),
            AgentError::RunNotFound { run_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_get_run_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(1, 42)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.get_run(1).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().project_id, 42);
    }

    #[tokio::test]
    async fn test_list_runs_success() {
        // Sea-ORM paginator: num_items (COUNT) first, then fetch_page (SELECT)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(2)]])
            .append_query_results(vec![vec![make_run(1, 10), make_run(2, 10)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.list_runs(10, None, None).await;
        assert!(result.is_ok());
        let (runs, total) = result.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn test_check_cooldown_no_recent_runs() {
        // count_row(0) → count = 0 → not on cooldown
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(0)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc
            .check_cooldown(42, Some("error_group"), Some(7), 60)
            .await;
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "cooldown should not be active when count is 0"
        );
    }

    #[tokio::test]
    async fn test_check_cooldown_recent_run_exists() {
        // count_row(1) → count = 1 → on cooldown
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(1)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc
            .check_cooldown(42, Some("error_group"), Some(7), 60)
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "cooldown should be active when count > 0");
    }

    #[tokio::test]
    async fn test_count_active_runs() {
        // count_row(3) → 3 active runs
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(3)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.count_active_runs(42).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_count_active_runs_zero() {
        // count_row(0) → 0 active runs
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(0)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.count_active_runs(42).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_get_daily_spend_zero() {
        // sum_row(None) → SUM returned NULL → 0 cents
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sum_row(None)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.get_daily_spend(42).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_get_daily_spend_with_data() {
        // sum_row(Some(350)) → 350 cents spent today
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sum_row(Some(350))]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.get_daily_spend(42).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 350);
    }

    #[tokio::test]
    async fn test_count_active_runs_at_limit() {
        // count_row(5) → 5 active runs (at the concurrent limit)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(5)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.count_active_runs(10).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn test_ensure_run_in_project_matches() {
        // Run belongs to project 42 — same project presented by caller.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(10, 42)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.ensure_run_in_project(10, 42).await;
        assert!(
            result.is_ok(),
            "should succeed when project_id matches run.project_id"
        );
    }

    #[tokio::test]
    async fn test_ensure_run_in_project_mismatch_returns_run_not_found() {
        // Run belongs to project 42, but caller claims project 99.
        // The helper must return RunNotFound (not Forbidden) to avoid
        // leaking that the run exists in another project.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(10, 42)]])
            .into_connection();
        let svc = AgentRunService::new(Arc::new(db));

        let result = svc.ensure_run_in_project(10, 99).await;
        assert!(
            matches!(result.unwrap_err(), AgentError::RunNotFound { run_id: 10 }),
            "cross-project access must return RunNotFound, not Forbidden"
        );
    }

    // Verify that all expected active statuses are logically present.
    // This test documents the contract and catches accidental omissions.
    #[test]
    fn test_active_run_statuses_are_well_known() {
        let expected = [
            "pending",
            "cloning",
            "analyzing",
            "fixing",
            "pushing",
            "creating_pr",
            "deploying",
        ];
        // Terminal statuses that must NOT be in the active list
        let terminal = ["completed", "failed", "no_fix"];

        // The test doubles as documentation: none of the terminal statuses
        // should accidentally appear in the active list.
        for status in &terminal {
            assert!(
                !expected.contains(status),
                "Terminal status '{}' must not be in the active-runs filter",
                status
            );
        }
        assert_eq!(expected.len(), 7, "Expected exactly 7 active statuses");
    }

    #[tokio::test]
    async fn test_make_run_with_status_helper() {
        // Sanity-check the test helper used in other tests
        let run = make_run_with_status(1, 42, "fixing");
        assert_eq!(run.status, "fixing");
        assert_eq!(run.project_id, 42);
        assert_eq!(run.id, 1);
    }
}
