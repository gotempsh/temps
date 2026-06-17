use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::jobs::AutopilotTriggerJob;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::workflow_memory::WorkflowMemoryProvider;
use temps_core::{Job, JobQueue, JobReceiver};
use temps_error_tracking::services::source_map_service::SourceMapService;
use temps_git::services::git_provider_manager_trait::GitProviderManagerTrait;
use temps_notifications::services::NotificationService;

use temps_deployments::jobs::configure_agents::{
    AgentSyncError, AgentSyncResult, AgentSyncService,
};
use temps_deployments::services::deployment_token_service::DeploymentTokenService;

use crate::handlers::AppState;
use crate::sandbox::docker::{DockerSandboxConfig, DockerSandboxProvider};
use crate::sandbox::local::LocalSandboxProvider;
use crate::sandbox::SandboxProvider;
use crate::services::autofixer::AutofixerService;
use crate::services::config_service::AgentConfigService;
use crate::services::cron_scheduler::AgentCronScheduler;
use crate::services::executor::AgentExecutor;
use crate::services::run_service::AgentRunService;
use crate::services::sandbox_registry::SandboxRegistry;
use crate::services::secret_service::SecretService;

use temps_entities::project_agents;

/// Adapter: implement the deployment pipeline's AgentSyncService trait
/// using the AgentConfigService from this crate.
struct AgentConfigSyncAdapter {
    config_service: Arc<AgentConfigService>,
}

#[async_trait::async_trait]
impl AgentSyncService for AgentConfigSyncAdapter {
    async fn sync_agents_from_yaml(
        &self,
        project_id: i32,
        agents: Vec<temps_core::AgentYamlConfig>,
    ) -> Result<AgentSyncResult, AgentSyncError> {
        let result = self
            .config_service
            .sync_agents_from_yaml(project_id, agents)
            .await
            .map_err(|e| AgentSyncError::Other(e.to_string()))?;
        Ok(AgentSyncResult {
            created: result.created,
            updated: result.updated,
            deleted: result.deleted,
        })
    }
}

/// Maximum number of simultaneous active runs per project.
const MAX_CONCURRENT_RUNS_PER_PROJECT: u64 = 5;

/// Narrow a list of trigger-matching agents down to the agent the trigger
/// actually identifies, when the trigger type encodes a single source.
///
/// Scheduled triggers come from `AgentCronScheduler::tick`, which emits one
/// job per agent whose cron matched the current minute, tagged with
/// `trigger_source_type = "agent_schedule"` and `trigger_source_id = <agent.id>`.
/// Without this filter, every scheduled agent in the project would run on
/// every other agent's tick — e.g. a daily 07:00 report would also fire at
/// every 2-hour docs pass.
///
/// Non-schedule triggers (errors, deploys, monitoring, manual) keep the
/// existing fan-out behaviour: all agents subscribed to that trigger type
/// get a chance to run.
fn filter_agents_for_trigger(
    trigger: &AutopilotTriggerJob,
    agents: &mut Vec<project_agents::Model>,
) {
    if trigger.trigger_type == "schedule"
        && trigger.trigger_source_type.as_deref() == Some("agent_schedule")
    {
        if let Some(agent_id) = trigger.trigger_source_id {
            agents.retain(|a| a.id == agent_id);
        }
    }
}

/// Guarded constructor for `LocalSandboxProvider`. The local provider runs
/// agent-executed commands **directly on the host** with no namespace
/// isolation, no resource limits, and no capability dropping — it is safe
/// only for single-developer machines. We require an explicit opt-in via
/// `TEMPS_ALLOW_LOCAL_SANDBOX=1` so production deployments that temporarily
/// lose Docker don't silently fall through to executing untrusted agent
/// code as the `temps` service user.
fn ensure_local_sandbox_allowed() -> Result<Arc<dyn SandboxProvider>, PluginError> {
    let allowed = std::env::var("TEMPS_ALLOW_LOCAL_SANDBOX")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if !allowed {
        return Err(PluginError::InitializationFailed(
            "agents: Docker sandbox unavailable and TEMPS_ALLOW_LOCAL_SANDBOX is not set. \
             Refusing to run agents directly on the host — set the env var to '1' to accept \
             the risk on a dev machine, or fix Docker in production."
                .to_string(),
        ));
    }
    tracing::warn!(
        "⚠️  TEMPS_ALLOW_LOCAL_SANDBOX=1 — agents will execute on the host with no isolation. \
         This is INSECURE and intended for development only."
    );
    Ok(Arc::new(LocalSandboxProvider::new()))
}

pub struct AgentsPlugin;

impl AgentsPlugin {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate whether an autopilot trigger should proceed.
    ///
    /// Returns `Ok(config)` if all gates pass and a run should be created.
    /// Returns `Err(reason)` with a human-readable skip reason if any gate fails.
    /// Evaluate whether a specific agent should run for this trigger.
    /// The agent is already known to match the trigger type (filtered by list_agents_for_trigger).
    /// This checks remaining gates: cooldown, budget, concurrency.
    pub(crate) async fn evaluate_trigger(
        trigger: &AutopilotTriggerJob,
        agent: &project_agents::Model,
        run_service: &AgentRunService,
    ) -> Result<(), String> {
        // Gate 1: Already checked by list_agents_for_trigger (agent is enabled + trigger type matches)

        // Gate 2: Trigger type is enabled (double-check, already filtered but kept for safety)
        let trigger_enabled = match trigger.trigger_type.as_str() {
            "new_issue" => agent
                .trigger_config
                .get("error")
                .and_then(|e| e.get("new_issue"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "regression" => agent
                .trigger_config
                .get("error")
                .and_then(|e| e.get("regression"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "monitoring_downtime" => agent
                .trigger_config
                .get("monitoring")
                .and_then(|m| m.get("downtime"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "monitoring_latency_spike" => agent
                .trigger_config
                .get("monitoring")
                .and_then(|m| m.get("latency_spike"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "deploy_production" => agent
                .trigger_config
                .get("deploy")
                .and_then(|d| d.get("production"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "deploy_preview" => agent
                .trigger_config
                .get("deploy")
                .and_then(|d| d.get("preview"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "alarm" => agent
                .trigger_config
                .get("alarm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "schedule" => agent
                .trigger_config
                .get("schedule")
                .and_then(|s| s.get("cron"))
                .and_then(|v| v.as_str())
                .is_some(),
            "webhook" => agent
                .trigger_config
                .get("webhook")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "manual" => true,
            _ => false,
        };
        if !trigger_enabled {
            return Err(format!(
                "trigger type '{}' not enabled for project {}",
                trigger.trigger_type, trigger.project_id
            ));
        }

        // Gate 3: Cooldown check BEFORE creating run to avoid self-counting.
        let on_cooldown = match run_service
            .check_cooldown(
                trigger.project_id,
                trigger.trigger_source_type.as_deref(),
                trigger.trigger_source_id,
                agent.cooldown_minutes,
            )
            .await
        {
            Ok(cd) => cd,
            Err(e) => {
                return Err(format!(
                    "failed to check cooldown for project {} trigger {:?}: {}",
                    trigger.project_id, trigger.trigger_source_id, e
                ));
            }
        };
        if on_cooldown {
            return Err(format!(
                "cooldown active for project {} trigger source {:?}",
                trigger.project_id, trigger.trigger_source_id
            ));
        }

        // Gate 4: Daily budget check BEFORE creating run.
        let spent = match run_service.get_daily_spend(trigger.project_id).await {
            Ok(s) => s,
            Err(e) => {
                return Err(format!(
                    "failed to check daily spend for project {}: {}",
                    trigger.project_id, e
                ));
            }
        };
        if agent.daily_budget_cents > 0 && spent >= agent.daily_budget_cents {
            return Err(format!(
                "daily budget exceeded for project {}: {} >= {} cents",
                trigger.project_id, spent, agent.daily_budget_cents
            ));
        }

        // Gate 5: Concurrent runs limit.
        let active_count = match run_service.count_active_runs(trigger.project_id).await {
            Ok(c) => c,
            Err(e) => {
                return Err(format!(
                    "failed to count active runs for project {}: {}",
                    trigger.project_id, e
                ));
            }
        };
        if active_count >= MAX_CONCURRENT_RUNS_PER_PROJECT {
            return Err(format!(
                "max concurrent runs ({}) reached for project {}",
                MAX_CONCURRENT_RUNS_PER_PROJECT, trigger.project_id
            ));
        }

        Ok(())
    }

    /// Background loop: listen for job queue events and dispatch autopilot work.
    async fn process_jobs(
        mut receiver: Box<dyn JobReceiver>,
        executor: Arc<AgentExecutor>,
        run_service: Arc<AgentRunService>,
        config_service: Arc<AgentConfigService>,
    ) {
        loop {
            match receiver.recv().await {
                Ok(job) => {
                    match job {
                        Job::AutopilotTrigger(trigger) => {
                            tracing::info!(
                                "Agent trigger for project {} (type: {}, source: {:?})",
                                trigger.project_id,
                                trigger.trigger_type,
                                trigger.trigger_source_id
                            );

                            // Load all agents that match this trigger type
                            let mut agents = match config_service
                                .list_agents_for_trigger(trigger.project_id, &trigger.trigger_type)
                                .await
                            {
                                Ok(a) => a,
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to load agents for project {}: {}",
                                        trigger.project_id,
                                        e
                                    );
                                    continue;
                                }
                            };

                            filter_agents_for_trigger(&trigger, &mut agents);

                            if agents.is_empty() {
                                tracing::debug!(
                                    "No matching agents for trigger type '{}' in project {}",
                                    trigger.trigger_type,
                                    trigger.project_id
                                );
                                continue;
                            }

                            // Evaluate and spawn each matching agent independently
                            for agent in agents {
                                match Self::evaluate_trigger(&trigger, &agent, &run_service).await {
                                    Ok(_) => {}
                                    Err(reason) => {
                                        tracing::info!(
                                            "Agent '{}' skipped for project {}: {}",
                                            agent.slug,
                                            trigger.project_id,
                                            reason
                                        );
                                        continue;
                                    }
                                }

                                // Gates passed — create run and spawn executor
                                let run = match run_service
                                    .create_run(
                                        trigger.project_id,
                                        agent.id,
                                        trigger.trigger_type.clone(),
                                        trigger.trigger_source_id,
                                        trigger.trigger_source_type.clone(),
                                        None, // No user_context for automated triggers
                                    )
                                    .await
                                {
                                    Ok(run) => run,
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to create run for agent '{}' in project {}: {}",
                                            agent.slug,
                                            trigger.project_id,
                                            e
                                        );
                                        continue;
                                    }
                                };

                                tracing::info!(
                                    "Created agent run {} for '{}' in project {}",
                                    run.id,
                                    agent.slug,
                                    trigger.project_id
                                );

                                let exec = executor.clone();
                                let run_id = run.id;
                                tokio::spawn(async move {
                                    exec.execute_run(run_id).await;
                                });
                            }
                        }

                        Job::DeploymentReady(ready) => {
                            // Check if this deployment corresponds to an autopilot branch.
                            // If so, update the preview_url on the matching run.
                            tracing::debug!(
                                "DeploymentReady received for project {}, deployment {}",
                                ready.project_id,
                                ready.deployment_id
                            );
                            // Update any runs that match this deployment
                            if let Some(url) = &ready.url {
                                if let Err(e) = Self::update_preview_url_for_deployment(
                                    &run_service,
                                    ready.deployment_id,
                                    url,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        "Failed to update preview URL for deployment {}: {}",
                                        ready.deployment_id,
                                        e
                                    );
                                }
                            }
                        }

                        _ => {
                            // Not an autopilot job — ignore
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error receiving job in autopilot plugin: {:?}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// When a deployment is ready, find the autopilot run that created it and update preview_url.
    async fn update_preview_url_for_deployment(
        run_service: &Arc<AgentRunService>,
        deployment_id: i32,
        preview_url: &str,
    ) -> Result<(), crate::error::AgentError> {
        // This would need the DB directly — use run_service instead by querying via Sea-ORM.
        // For now we log and skip; a future enhancement can add a find_by_deployment_id method
        // to AgentRunService.
        tracing::debug!(
            "Received DeploymentReady for deployment {} (url: {}); run_service available: {}",
            deployment_id,
            preview_url,
            Arc::strong_count(run_service)
        );
        Ok(())
    }
}

impl Default for AgentsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AgentsPlugin {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            let queue = context.require_service::<dyn JobQueue>();
            let git_provider_manager = context.require_service::<dyn GitProviderManagerTrait>();

            let notification_service = context.require_service::<NotificationService>();

            // Load global sandbox settings to configure the Docker provider
            let global_sandbox = {
                use sea_orm::EntityTrait;
                temps_entities::settings::Entity::find_by_id(1)
                    .one(db.as_ref())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|s| {
                        s.data.get("agent_sandbox").cloned().and_then(|v| {
                            serde_json::from_value::<temps_core::AgentSandboxSettings>(v).ok()
                        })
                    })
                    .unwrap_or_default()
            };

            // Set up sandbox provider: try Docker first, fall back to local.
            //
            // The sandbox image is NOT pulled or built at startup. Doing so —
            // even in a background task — kicks off a GHCR pull and, if the
            // tagged image isn't published yet, a multi-minute local Docker
            // build on every boot. That work is deferred entirely to the
            // first agent run: `create()` pulls/builds the image lazily the
            // first time a sandbox is actually needed.
            let sandbox_provider: Arc<dyn SandboxProvider> =
                match bollard::Docker::connect_with_local_defaults() {
                    Ok(docker) => {
                        let docker = Arc::new(docker);
                        match docker.ping().await {
                            Ok(_) => {
                                let config = DockerSandboxConfig {
                                    runtime: global_sandbox.runtime.clone(),
                                    custom_image: global_sandbox.custom_image.clone(),
                                    default_cpu_limit: global_sandbox.cpu_limit,
                                    default_memory_limit_mb: global_sandbox.memory_limit_mb,
                                    network_mode: global_sandbox.network_mode.clone(),
                                };
                                let provider = Arc::new(DockerSandboxProvider::new(docker, config));
                                tracing::info!(
                                    "Docker sandbox provider initialized (image built on demand at first agent run)"
                                );
                                provider as Arc<dyn SandboxProvider>
                            }
                            Err(e) => {
                                tracing::warn!("Docker not responding, using local sandbox: {}", e);
                                ensure_local_sandbox_allowed()?
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Docker not available, using local sandbox: {}", e);
                        ensure_local_sandbox_allowed()?
                    }
                };
            // Register the bare sandbox provider as `dyn SandboxProvider` so
            // other plugins (e.g. workspace) can pick it up via the trait
            // without depending on temps-agents directly.
            context.register_service(sandbox_provider.clone());

            let sandbox_registry = Arc::new(SandboxRegistry::new(sandbox_provider));

            let config_service = Arc::new(AgentConfigService::new(
                db.clone(),
                encryption_service.clone(),
            ));
            context.register_service(config_service.clone());

            let secret_service =
                Arc::new(SecretService::new(db.clone(), encryption_service.clone()));
            context.register_service(secret_service.clone());

            // Register the sync adapter so the deployment pipeline can sync agents from YAML
            let sync_adapter: Arc<dyn AgentSyncService> = Arc::new(AgentConfigSyncAdapter {
                config_service: config_service.clone(),
            });
            context.register_service(sync_adapter);

            let run_service = Arc::new(AgentRunService::new(db.clone()));
            context.register_service(run_service.clone());

            // Recover any runs that were in progress when the server last stopped
            if let Err(e) = run_service.recover_stuck_runs().await {
                tracing::warn!("Failed to recover stuck agent runs: {}", e);
            }

            let definition_service =
                Arc::new(crate::services::definition_service::DefinitionService::new(
                    context.require_service::<sea_orm::DatabaseConnection>(),
                ));
            context.register_service(definition_service.clone());
            let executor = Arc::new(AgentExecutor::new(
                db.clone(),
                git_provider_manager.clone(),
                encryption_service.clone(),
                queue.clone(),
                run_service.clone(),
                config_service.clone(),
                notification_service,
                sandbox_registry.clone(),
                secret_service.clone(),
                definition_service.clone(),
            ));
            context.register_service(executor.clone());

            let source_map_service = context.require_service::<SourceMapService>();
            let autofixer_service = Arc::new(AutofixerService::new(
                db,
                git_provider_manager,
                encryption_service,
                queue.clone(),
                run_service.clone(),
                source_map_service,
                sandbox_registry,
                executor.clone(),
            ));
            context.register_service(autofixer_service.clone());

            // Subscribe to the job queue and start the background listener
            let job_receiver = queue.subscribe();
            let executor_for_jobs = executor.clone();
            let run_service_for_jobs = run_service.clone();
            let config_service_for_jobs = config_service.clone();
            tokio::spawn(async move {
                tracing::debug!("Starting autopilot job listener");
                Self::process_jobs(
                    job_receiver,
                    executor_for_jobs,
                    run_service_for_jobs,
                    config_service_for_jobs,
                )
                .await;
            });

            // Start the cron scheduler for agents with schedule triggers
            let cron_scheduler = AgentCronScheduler::new(config_service.clone(), queue.clone());
            tokio::spawn(async move {
                tracing::debug!("Starting agent cron scheduler");
                cron_scheduler.run().await;
            });

            // Store state for route configuration
            let app_state = Arc::new(AppState {
                db: context.require_service::<sea_orm::DatabaseConnection>(),
                encryption_service: context.require_service::<temps_core::EncryptionService>(),
                config_service,
                run_service,
                executor,
                audit_service: context.require_service::<dyn temps_core::AuditLogger>(),
                autofixer_service,
                secret_service,
                definition_service,
                docker: context.require_service::<bollard::Docker>(),
                platform_config_service: context.require_service::<temps_config::ConfigService>(),
                telemetry: context
                    .get_service::<dyn temps_core::TelemetryReporter>()
                    .unwrap_or_else(|| Arc::new(temps_core::NoopTelemetryReporter)),
            });
            context.register_plugin_state("agents", app_state);

            tracing::debug!("Autopilot plugin services registered successfully");
            Ok(())
        })
    }

    /// Late-binding phase: attach optional dependencies that weren't available
    /// during `register_services` because the plugins that provide them are
    /// registered later in the boot order.
    ///
    /// Specifically:
    /// - **WorkflowMemoryProvider** is optional; no in-tree implementation
    ///   currently registers one (the workspace feature that provided it was
    ///   removed). The executor degrades gracefully without it.
    /// - **DeploymentTokenService** comes from `temps-deployments`, registered after agents.
    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let executor = context.require_service::<AgentExecutor>();

            if let Some(memory_provider) = context.get_service::<dyn WorkflowMemoryProvider>() {
                executor.attach_memory_provider(memory_provider).await;
                tracing::info!("Agents plugin: workflow memory provider attached");
            } else {
                tracing::debug!(
                    "Agents plugin: no workflow memory provider available; \
                     workflow runs will execute without memory injection"
                );
            }

            if let Some(token_service) = context.get_service::<DeploymentTokenService>() {
                executor
                    .attach_deployment_token_service(token_service)
                    .await;
                tracing::info!("Agents plugin: deployment token service attached");
            } else {
                tracing::debug!(
                    "Agents plugin: no deployment token service available; \
                     workflow run sandboxes will not be able to authenticate to the API"
                );
            }

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.get_plugin_state::<AppState>("agents")?;

        let router = crate::handlers::configure_routes().with_state(app_state);

        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi;
        Some(crate::handlers::AgentsApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;
    use temps_entities::project_agents;

    /// Mock row for a COUNT(*) AS num_items query (used by sea-orm's `.count()` via paginator).
    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("num_items".to_string(), Value::BigInt(Some(n)));
        m
    }

    /// Mock row for the SUM query used by `get_daily_spend` (column alias "total").
    fn sum_row(n: Option<i64>) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("total".to_string(), Value::BigInt(n));
        m
    }

    fn make_config(project_id: i32, enabled: bool) -> project_agents::Model {
        project_agents::Model {
            id: 1,
            project_id,
            slug: "default-agent".to_string(),
            name: "Default Agent".to_string(),
            description: None,
            source: "dashboard".to_string(),
            enabled,
            trigger_config: serde_json::json!({
                "error": { "new_issue": true, "regression": true },
                "manual": true
            }),
            prompt: None,
            ai_provider: "claude_cli".to_string(),
            ai_model: None,
            api_key_encrypted: None,
            ai_provider_key_id: None,
            max_turns: 30,
            timeout_seconds: 600,
            daily_budget_cents: 500,
            cooldown_minutes: 60,
            branch_prefix: String::new(),
            deliverable: "pull_request".to_string(),
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
            webhook_id: None,
            webhook_token: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn make_trigger(project_id: i32, trigger_type: &str) -> AutopilotTriggerJob {
        AutopilotTriggerJob {
            project_id,
            trigger_type: trigger_type.to_string(),
            trigger_source_id: Some(7),
            trigger_source_type: Some("error_group".to_string()),
            error_group_id: Some(7),
        }
    }

    #[test]
    fn test_autopilot_plugin_name() {
        let plugin = AgentsPlugin::new();
        assert_eq!(plugin.name(), "agents");
    }

    #[test]
    fn test_autopilot_plugin_default() {
        let plugin = AgentsPlugin;
        assert_eq!(plugin.name(), "agents");
    }

    // ---------------------------------------------------------------------------
    // evaluate_trigger tests
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_evaluate_trigger_trigger_type_not_enabled() {
        // Config has no "alarm" key in trigger_config, so alarm is disabled → gate 2 fails
        let mut config = make_config(42, true);
        config.trigger_config = serde_json::json!({
            "error": { "new_issue": true, "regression": true },
            "manual": true
        });

        let run_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "alarm");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_err());
        let reason = result.unwrap_err();
        assert!(
            reason.contains("trigger type") && reason.contains("not enabled"),
            "unexpected reason: {}",
            reason
        );
    }

    #[tokio::test]
    async fn test_evaluate_trigger_cooldown_active() {
        // Config is enabled, trigger type enabled, but cooldown returns count = 1 → gate 3 fails
        let config = make_config(42, true);

        // cooldown check returns count = 1
        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(1)]])
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "new_issue");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_err());
        let reason = result.unwrap_err();
        assert!(
            reason.contains("cooldown active"),
            "unexpected reason: {}",
            reason
        );
    }

    #[tokio::test]
    async fn test_evaluate_trigger_budget_exceeded() {
        // Cooldown passes (count = 0), but daily spend >= budget → gate 4 fails
        let mut config = make_config(42, true);
        config.daily_budget_cents = 100;

        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            // cooldown check: count = 0
            .append_query_results(vec![vec![count_row(0)]])
            // daily spend: sum = 100 cents (at limit)
            .append_query_results(vec![vec![sum_row(Some(100))]])
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "new_issue");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_err());
        let reason = result.unwrap_err();
        assert!(
            reason.contains("daily budget exceeded"),
            "unexpected reason: {}",
            reason
        );
    }

    #[tokio::test]
    async fn test_evaluate_trigger_max_concurrent() {
        // Cooldown passes, budget passes, but active runs = MAX_CONCURRENT_RUNS_PER_PROJECT → gate 5 fails
        let config = make_config(42, true);

        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            // cooldown check: count = 0
            .append_query_results(vec![vec![count_row(0)]])
            // daily spend: 0
            .append_query_results(vec![vec![sum_row(None)]])
            // active runs: 5 (at limit)
            .append_query_results(vec![vec![count_row(5)]])
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "new_issue");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_err());
        let reason = result.unwrap_err();
        assert!(
            reason.contains("max concurrent runs"),
            "unexpected reason: {}",
            reason
        );
    }

    #[tokio::test]
    async fn test_evaluate_trigger_passes_all_checks() {
        // All gates pass → Ok(())
        let config = make_config(42, true);

        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            // cooldown check: count = 0
            .append_query_results(vec![vec![count_row(0)]])
            // daily spend: 50 cents (under 500 limit)
            .append_query_results(vec![vec![sum_row(Some(50))]])
            // active runs: 2 (under limit of 5)
            .append_query_results(vec![vec![count_row(2)]])
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "new_issue");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_evaluate_trigger_manual_always_enabled() {
        // "manual" trigger type is always allowed regardless of config flags
        let mut config = make_config(42, true);
        // Disable all automatic trigger types via trigger_config
        config.trigger_config = serde_json::json!({
            "error": { "new_issue": false, "regression": false },
            "manual": true
        });

        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(0)]]) // cooldown
            .append_query_results(vec![vec![sum_row(None)]]) // spend
            .append_query_results(vec![vec![count_row(0)]]) // active runs
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "manual");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_ok(), "manual trigger should always pass gate 2");
    }

    #[tokio::test]
    async fn test_evaluate_trigger_unknown_type_rejected() {
        // Unknown trigger type → gate 2 fails
        let config = make_config(42, true);

        let run_db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "bogus_event");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(result.is_err());
        let reason = result.unwrap_err();
        assert!(
            reason.contains("not enabled"),
            "unexpected reason: {}",
            reason
        );
    }

    #[tokio::test]
    async fn test_evaluate_trigger_zero_budget_skips_budget_check() {
        // daily_budget_cents = 0 means "unlimited" — even if spend is large, gate 4 passes
        let mut config = make_config(42, true);
        config.daily_budget_cents = 0;

        let run_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(0)]]) // cooldown
            .append_query_results(vec![vec![sum_row(Some(99999))]]) // large spend — ignored
            .append_query_results(vec![vec![count_row(0)]]) // active runs
            .into_connection();
        let run_svc = AgentRunService::new(Arc::new(run_db));

        let trigger = make_trigger(42, "new_issue");
        let result = AgentsPlugin::evaluate_trigger(&trigger, &config, &run_svc).await;

        assert!(
            result.is_ok(),
            "zero daily_budget_cents should mean unlimited"
        );
    }

    // ---------------------------------------------------------------------------
    // filter_agents_for_trigger tests
    //
    // Regression coverage for the bug where a 2-hour scheduled workflow
    // (e.g. "Docs Auto-Improve" at `17 */2 * * *`) was dragging every other
    // scheduled agent in the project along with it — including a daily 07:00
    // report — because `list_agents_for_trigger` returns every agent that has
    // any cron, and the dispatch loop wasn't narrowing back down to the agent
    // identified by `trigger_source_id`.
    // ---------------------------------------------------------------------------

    fn schedule_trigger(project_id: i32, source_agent_id: i32) -> AutopilotTriggerJob {
        AutopilotTriggerJob {
            project_id,
            trigger_type: "schedule".to_string(),
            trigger_source_id: Some(source_agent_id),
            trigger_source_type: Some("agent_schedule".to_string()),
            error_group_id: None,
        }
    }

    fn with_id(mut config: project_agents::Model, id: i32) -> project_agents::Model {
        config.id = id;
        config
    }

    #[test]
    fn test_filter_schedule_trigger_keeps_only_source_agent() {
        let trigger = schedule_trigger(42, 10);
        let mut agents = vec![
            with_id(make_config(42, true), 10),
            with_id(make_config(42, true), 11),
            with_id(make_config(42, true), 12),
        ];

        filter_agents_for_trigger(&trigger, &mut agents);

        assert_eq!(agents.len(), 1, "only the firing agent should remain");
        assert_eq!(agents[0].id, 10);
    }

    #[test]
    fn test_filter_schedule_trigger_drops_all_when_source_missing() {
        // Source agent id refers to an agent that no longer matches the trigger
        // (e.g. it was disabled between cron tick and dispatch). The siblings
        // must NOT be promoted as a fallback.
        let trigger = schedule_trigger(42, 99);
        let mut agents = vec![
            with_id(make_config(42, true), 10),
            with_id(make_config(42, true), 11),
        ];

        filter_agents_for_trigger(&trigger, &mut agents);

        assert!(
            agents.is_empty(),
            "no agent matches source id 99 — siblings must not run"
        );
    }

    #[test]
    fn test_filter_schedule_trigger_without_source_keeps_all() {
        // Defensive: a malformed schedule trigger with no source id keeps the
        // existing fan-out behaviour rather than silently dropping every
        // candidate. Real cron ticks always set the source id.
        let mut trigger = schedule_trigger(42, 0);
        trigger.trigger_source_id = None;
        let mut agents = vec![
            with_id(make_config(42, true), 10),
            with_id(make_config(42, true), 11),
        ];

        filter_agents_for_trigger(&trigger, &mut agents);

        assert_eq!(agents.len(), 2);
    }

    #[test]
    fn test_filter_non_schedule_trigger_keeps_all() {
        // Error triggers, deploy triggers, manual, etc. fan out — every
        // subscribed agent runs. The filter must not touch them.
        let trigger = make_trigger(42, "new_issue");
        let mut agents = vec![
            with_id(make_config(42, true), 10),
            with_id(make_config(42, true), 11),
        ];

        filter_agents_for_trigger(&trigger, &mut agents);

        assert_eq!(
            agents.len(),
            2,
            "non-schedule triggers must keep their fan-out semantics"
        );
    }

    #[test]
    fn test_filter_schedule_trigger_ignores_non_agent_schedule_source() {
        // If a future trigger source emits trigger_type="schedule" with a
        // different source type (e.g. an external scheduler), do not narrow —
        // we'd be filtering on an id that doesn't refer to a project_agents row.
        let mut trigger = schedule_trigger(42, 10);
        trigger.trigger_source_type = Some("external_scheduler".to_string());
        let mut agents = vec![
            with_id(make_config(42, true), 10),
            with_id(make_config(42, true), 11),
        ];

        filter_agents_for_trigger(&trigger, &mut agents);

        assert_eq!(agents.len(), 2);
    }
}
