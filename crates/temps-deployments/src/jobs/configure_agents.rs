//! Configure Agents Job
//!
//! Syncs agent definitions from .temps/agents/*.yaml files after deployment.
//! Follows the same pattern as ConfigureCronsJob.

use async_trait::async_trait;
use sea_orm::EntityTrait;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use temps_core::{
    AgentTriggers, AgentYamlConfig, DeployTrigger, ErrorTrigger, JobResult, MonitoringTrigger,
    ScheduleTrigger, WorkflowContext, WorkflowError, WorkflowTask, WorkflowYamlConfig,
};
use temps_database::DbConnection;
use temps_entities::projects;
use temps_logs::{LogLevel, LogService};
use tracing::{debug, info, warn};

use crate::jobs::RepositoryOutput;

/// Service interface for agent sync — implemented by temps-agents crate
#[async_trait]
pub trait AgentSyncService: Send + Sync {
    async fn sync_agents_from_yaml(
        &self,
        project_id: i32,
        agents: Vec<AgentYamlConfig>,
    ) -> Result<AgentSyncResult, AgentSyncError>;
}

#[derive(Debug)]
pub struct AgentSyncResult {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentSyncError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Sync error: {0}")]
    Other(String),
}

/// No-op implementation for when agent service is not available
pub struct NoOpAgentSyncService;

#[async_trait]
impl AgentSyncService for NoOpAgentSyncService {
    async fn sync_agents_from_yaml(
        &self,
        _project_id: i32,
        _agents: Vec<AgentYamlConfig>,
    ) -> Result<AgentSyncResult, AgentSyncError> {
        warn!("Agent sync skipped - no agent service available");
        Ok(AgentSyncResult {
            created: 0,
            updated: 0,
            deleted: 0,
        })
    }
}

/// Job for syncing agent definitions from .temps/agents/*.yaml
pub struct ConfigureAgentsJob {
    job_id: String,
    download_job_id: String,
    deploy_container_job_id: String,
    project_id: i32,
    db: Arc<DbConnection>,
    agent_sync_service: Arc<dyn AgentSyncService>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl std::fmt::Debug for ConfigureAgentsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigureAgentsJob")
            .field("job_id", &self.job_id)
            .field("project_id", &self.project_id)
            .finish()
    }
}

impl ConfigureAgentsJob {
    async fn log(&self, message: String) -> Result<(), WorkflowError> {
        debug!("{}", message);
        if let (Some(log_id), Some(log_service)) = (&self.log_id, &self.log_service) {
            let level = if message.contains('✅') {
                LogLevel::Success
            } else if message.contains('❌') {
                LogLevel::Error
            } else {
                LogLevel::Info
            };
            log_service
                .append_structured_log(log_id, level, message)
                .await
                .map_err(|e| WorkflowError::Other(format!("Failed to write log: {}", e)))?;
        }
        Ok(())
    }

    /// Load .temps/agents/*.yaml files from the cloned repo
    fn load_agent_yamls(&self, repo_dir: &Path, project: &projects::Model) -> Vec<AgentYamlConfig> {
        let project_dir = repo_dir.join(&project.directory);
        let agents_dir = project_dir.join(".temps").join("agents");
        let mut agents = Vec::new();

        if !agents_dir.is_dir() {
            return agents;
        }

        let entries = match fs::read_dir(&agents_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read .temps/agents/ directory: {}", e);
                return agents;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }

            match fs::read_to_string(&path) {
                Ok(contents) => match serde_yaml::from_str::<AgentYamlConfig>(&contents) {
                    Ok(agent) => {
                        info!("Loaded agent '{}' from {:?}", agent.name, path);
                        agents.push(agent);
                    }
                    Err(e) => {
                        warn!("Failed to parse {:?}: {}", path, e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read {:?}: {}", path, e);
                }
            }
        }

        agents
    }

    /// Load .temps/workflows/*.yaml files and convert them to AgentYamlConfig
    /// so they can be synced through the same project_agents table.
    fn load_workflow_yamls(
        &self,
        repo_dir: &Path,
        project: &projects::Model,
    ) -> Vec<AgentYamlConfig> {
        let project_dir = repo_dir.join(&project.directory);
        let workflows_dir = project_dir.join(".temps").join("workflows");
        let mut workflows = Vec::new();

        if !workflows_dir.is_dir() {
            return workflows;
        }

        let entries = match fs::read_dir(&workflows_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read .temps/workflows/ directory: {}", e);
                return workflows;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }

            match fs::read_to_string(&path) {
                Ok(contents) => match serde_yaml::from_str::<WorkflowYamlConfig>(&contents) {
                    Ok(workflow) => {
                        info!("Loaded workflow '{}' from {:?}", workflow.name, path);
                        workflows.push(workflow_to_agent(workflow));
                    }
                    Err(e) => {
                        warn!("Failed to parse {:?}: {}", path, e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read {:?}: {}", path, e);
                }
            }
        }

        workflows
    }
}

/// Convert a WorkflowYamlConfig into an AgentYamlConfig for DB storage.
/// Workflows and agents share the same `project_agents` table — `AgentTriggers`
/// has been extended with `deploy` and `monitoring` so all workflow trigger
/// types map cleanly into the same row.
fn workflow_to_agent(workflow: WorkflowYamlConfig) -> AgentYamlConfig {
    AgentYamlConfig {
        name: workflow.name,
        description: workflow.description,
        on: AgentTriggers {
            error: workflow.on.error.map(|e| ErrorTrigger {
                new_issue: e.new_issue,
                regression: e.regression,
            }),
            deploy: workflow.on.deploy.map(|d| DeployTrigger {
                production: d.production,
                preview: d.preview,
            }),
            monitoring: workflow.on.monitoring.map(|m| MonitoringTrigger {
                downtime: m.downtime,
                latency_spike: m.latency_spike,
            }),
            schedule: workflow
                .on
                .schedule
                .map(|s| ScheduleTrigger { cron: s.cron }),
            manual: workflow.on.manual,
            webhook: false,
        },
        system: None,
        prompt: Some(workflow.prompt),
        model: None,
        provider: workflow.provider,
        ai_model: workflow.ai_model,
        ai_provider: None, // workflows use legacy `provider`; clean field stays None
        max_turns: workflow.max_turns,
        timeout_seconds: workflow.timeout_seconds,
        daily_budget_cents: workflow.daily_budget_cents,
        cooldown_minutes: workflow.cooldown_minutes,
        branch_prefix: "workflows/".to_string(),
        deliverable: workflow.deliverable,
        enabled: workflow.enabled,
        sandbox: workflow.sandbox,
        tools: workflow.tools,
        mcp_servers: workflow.mcp_servers,
        skills: workflow.skills,
        config_repo: workflow.config_repo,
        config_repo_branch: workflow.config_repo_branch,
    }
}

#[async_trait]
impl WorkflowTask for ConfigureAgentsJob {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn name(&self) -> &str {
        "Configure Agents"
    }

    fn description(&self) -> &str {
        "Syncs agent definitions from .temps/agents/*.yaml"
    }

    fn depends_on(&self) -> Vec<String> {
        vec![self.deploy_container_job_id.clone()]
    }

    async fn execute(&self, context: WorkflowContext) -> Result<JobResult, WorkflowError> {
        let repo_output = RepositoryOutput::from_context(&context, &self.download_job_id)?;

        self.log(
            "Checking for .temps/agents/*.yaml and .temps/workflows/*.yaml files...".to_string(),
        )
        .await?;

        // Load project
        let project = projects::Entity::find_by_id(self.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| WorkflowError::Other(format!("Failed to load project: {}", e)))?
            .ok_or_else(|| {
                WorkflowError::Other(format!("Project {} not found", self.project_id))
            })?;

        // Load agent YAML files (legacy .temps/agents/) and workflow YAML files
        // (new .temps/workflows/). Both sync into the same project_agents table.
        let mut agents = self.load_agent_yamls(&repo_output.repo_dir, &project);
        let workflows = self.load_workflow_yamls(&repo_output.repo_dir, &project);

        let agent_count = agents.len();
        let workflow_count = workflows.len();
        agents.extend(workflows);

        if agents.is_empty() {
            self.log("No agent or workflow YAML files found".to_string())
                .await?;
            return Ok(JobResult::success(context));
        }

        self.log(format!(
            "Found {} agent(s) and {} workflow(s) to sync",
            agent_count, workflow_count
        ))
        .await?;

        // Sync to database
        match self
            .agent_sync_service
            .sync_agents_from_yaml(self.project_id, agents)
            .await
        {
            Ok(result) => {
                self.log(format!(
                    "✅ Agent sync complete: {} created, {} updated, {} deleted",
                    result.created, result.updated, result.deleted
                ))
                .await?;
            }
            Err(e) => {
                let msg = format!("❌ Failed to sync agents: {}", e);
                self.log(msg.clone()).await?;
                return Err(WorkflowError::JobExecutionFailed(msg));
            }
        }

        Ok(JobResult::success(context))
    }

    async fn validate_prerequisites(&self, context: &WorkflowContext) -> Result<(), WorkflowError> {
        RepositoryOutput::from_context(context, &self.download_job_id)?;
        Ok(())
    }

    async fn cleanup(&self, _context: &WorkflowContext) -> Result<(), WorkflowError> {
        Ok(())
    }
}

/// Builder for ConfigureAgentsJob
pub struct ConfigureAgentsJobBuilder {
    job_id: Option<String>,
    download_job_id: Option<String>,
    deploy_container_job_id: Option<String>,
    project_id: Option<i32>,
    log_id: Option<String>,
    log_service: Option<Arc<LogService>>,
}

impl Default for ConfigureAgentsJobBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigureAgentsJobBuilder {
    pub fn new() -> Self {
        Self {
            job_id: None,
            download_job_id: None,
            deploy_container_job_id: None,
            project_id: None,
            log_id: None,
            log_service: None,
        }
    }

    pub fn job_id(mut self, id: String) -> Self {
        self.job_id = Some(id);
        self
    }
    pub fn download_job_id(mut self, id: String) -> Self {
        self.download_job_id = Some(id);
        self
    }
    pub fn deploy_container_job_id(mut self, id: String) -> Self {
        self.deploy_container_job_id = Some(id);
        self
    }
    pub fn project_id(mut self, id: i32) -> Self {
        self.project_id = Some(id);
        self
    }
    pub fn log_id(mut self, id: Option<String>) -> Self {
        self.log_id = id;
        self
    }
    pub fn log_service(mut self, svc: Arc<LogService>) -> Self {
        self.log_service = Some(svc);
        self
    }

    pub fn build(
        self,
        db: Arc<DbConnection>,
        agent_sync_service: Arc<dyn AgentSyncService>,
    ) -> Result<ConfigureAgentsJob, WorkflowError> {
        let job_id = self
            .job_id
            .unwrap_or_else(|| "configure_agents".to_string());
        let download_job_id = self
            .download_job_id
            .unwrap_or_else(|| "download_repo".to_string());
        let deploy_container_job_id = self
            .deploy_container_job_id
            .unwrap_or_else(|| "deploy_container".to_string());
        let project_id = self.project_id.ok_or_else(|| {
            WorkflowError::JobExecutionFailed(
                "project_id is required for ConfigureAgentsJob".into(),
            )
        })?;

        Ok(ConfigureAgentsJob {
            job_id,
            download_job_id,
            deploy_container_job_id,
            project_id,
            db,
            agent_sync_service,
            log_id: self.log_id,
            log_service: self.log_service,
        })
    }
}
