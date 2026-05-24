//! Repository Configuration (.temps.yaml)
//!
//! Central definition for the .temps.yaml configuration file format
//! that can be placed in user repositories to configure deployment behavior.

use serde::{Deserialize, Serialize};

/// Complete configuration structure for .temps.yaml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TempsConfig {
    /// Cron job configurations
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<Vec<CronJobConfig>>,

    /// Build configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildConfig>,

    /// Environment variables to inject at build/runtime
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,

    /// Health check configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthCheckConfig>,

    /// Agent configurations (alternative to .temps/agents/*.yaml files)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentYamlConfig>>,

    /// Workflow configurations (alternative to .temps/workflows/*.yaml files)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflows: Option<Vec<WorkflowYamlConfig>>,
}

/// Cron job configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobConfig {
    /// HTTP path to invoke for this cron job
    pub path: String,

    /// Cron schedule in standard cron format
    /// Format: "minute hour day month weekday"
    /// Example: "0 0 * * *" (daily at midnight)
    pub schedule: String,

    /// Optional name/description for the cron job
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Build configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Custom Dockerfile path (relative to repository root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,

    /// Build context path (relative to repository root)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    /// Build arguments to pass to Docker build
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<std::collections::HashMap<String, String>>,

    /// Install command (overrides preset detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,

    /// Build command (overrides preset detection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,

    /// Output directory for static builds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    /// HTTP path for health checks
    pub path: String,

    /// Expected HTTP status code (default: 200)
    #[serde(default = "default_health_status")]
    pub status: u16,

    /// Interval between health checks in seconds (default: 30)
    #[serde(default = "default_health_interval")]
    pub interval: u64,

    /// Timeout for health check requests in seconds (default: 5)
    #[serde(default = "default_health_timeout")]
    pub timeout: u64,

    /// Number of consecutive failures before marking unhealthy (default: 3)
    #[serde(default = "default_health_retries")]
    pub retries: u32,
}

fn default_health_status() -> u16 {
    200
}

fn default_health_interval() -> u64 {
    30
}

fn default_health_timeout() -> u64 {
    5
}

fn default_health_retries() -> u32 {
    3
}

// ── Agent YAML config ─────────────────────────────────────────────────────────

/// Agent YAML configuration from .temps/agents/*.yaml
///
/// Field naming follows Claude Managed Agents conventions where possible:
/// - `model` = AI model/harness to use (maps to `provider` internally)
/// - `system` = system prompt (alias for `prompt`)
/// - `tools` = inline tool definitions
/// - `mcp_servers` = slugs referencing project-level MCP server definitions
/// - `skills` = slugs referencing project-level skill definitions
///
/// Example:
/// ```yaml
/// name: error-fixer
/// description: Fixes production errors automatically
/// model: claude_cli               # or: opencode, codex_cli
/// system: |
///   You are a production error fixer.
///   Analyze the error and create a fix.
/// on:
///   error:
///     new_issue: true
/// mcp_servers: [github, sentry]
/// skills: [code-review, testing]
/// tools:
///   - type: web_search
/// deliverable: pull_request
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentYamlConfig {
    /// Required. Human-readable name for the agent.
    pub name: String,
    /// A description of what the agent does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Trigger configuration — when this agent runs.
    #[serde(default)]
    pub on: AgentTriggers,
    /// System prompt that defines the agent's behavior and persona.
    /// Supports template variables like `{{error_type}}`, `{{error_message}}`.
    /// Alias: `prompt` (for backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Legacy alias for `system`. If both are set, `system` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// AI provider to use. Values: `claude_cli` (default), `opencode`, `codex_cli`.
    /// Alias: `provider` (for backward compatibility). If `model` is set, it takes precedence.
    /// NOTE: historically this field overloaded as both provider id and model id.
    /// Prefer `ai_provider` + `ai_model` going forward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Legacy alias for `model`. `None` means "use the platform default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Preferred model identifier for the CLI (e.g. "sonnet", "opus", "gpt-5-codex").
    /// NULL means: let the CLI pick its default. This is independent from `provider` —
    /// the provider decides which CLI runs; `ai_model` picks the model inside that CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
    /// Clean provider identifier (preferred over legacy `provider`/`model`).
    /// Values: `claude_cli`, `opencode`, `codex_cli`. If set, takes precedence over
    /// both `model` and `provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_provider: Option<String>,
    /// Maximum number of AI turns before the agent stops.
    #[serde(default = "default_max_turns")]
    pub max_turns: i32,
    /// Maximum execution time in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_seconds: i32,
    /// Daily budget limit in cents.
    #[serde(default = "default_budget")]
    pub daily_budget_cents: i32,
    /// Cooldown between runs in minutes.
    #[serde(default = "default_cooldown")]
    pub cooldown_minutes: i32,
    /// Git branch prefix for agent-created branches.
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    /// What the agent produces: `pull_request`, `commit`, `analysis`, `log_only`.
    #[serde(default = "default_deliverable")]
    pub deliverable: String,
    /// Whether this agent is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Sandbox override. None = use global setting, Some(true) = force on, Some(false) = force off.
    #[serde(default)]
    pub sandbox: Option<bool>,
    /// Inline tool definitions available to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AgentToolConfig>>,
    /// MCP server slugs referencing project-level definitions in Temps.
    /// Each slug is resolved to its full config at runtime and injected into the sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,
    /// Skill slugs referencing project-level definitions in Temps.
    /// Each slug is resolved to its full content at runtime and written to `.claude/skills/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Private config repo containing `.claude/` directory (skills, MCP servers, settings).
    /// Format: "owner/repo" (e.g. "myorg/claude-config"). Cloned at runtime and
    /// overlaid into the sandbox's `/home/temps/.claude/` directory (kept out
    /// of the bind-mounted `/workspace` repo to avoid leaking resolved secrets
    /// into PR diffs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_repo: Option<String>,
    /// Branch of the config repo to use (default: "main").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_repo_branch: Option<String>,
}

/// Inline tool configuration for agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolConfig {
    /// Tool type identifier (e.g. "web_search", "file_search", "bash", "mcp").
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Optional tool-specific configuration.
    #[serde(flatten)]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentTriggers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring: Option<MonitoringTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleTrigger>,
    #[serde(default = "default_true")]
    pub manual: bool,
    /// Enable public webhook trigger. When true, Temps auto-generates a unique
    /// webhook token and exposes `POST /api/agents/webhook/{token}`.
    #[serde(default)]
    pub webhook: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorTrigger {
    #[serde(default)]
    pub new_issue: bool,
    #[serde(default)]
    pub regression: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleTrigger {
    pub cron: Option<String>,
}

fn default_max_turns() -> i32 {
    25
}
fn default_timeout() -> i32 {
    600
}
fn default_budget() -> i32 {
    500
}
fn default_cooldown() -> i32 {
    30
}
fn default_branch_prefix() -> String {
    "agents/".to_string()
}
fn default_deliverable() -> String {
    "pull_request".to_string()
}
fn default_true() -> bool {
    true
}

impl AgentYamlConfig {
    /// Resolved provider: `ai_provider` > legacy `model` > legacy `provider`.
    /// Returns `None` when no provider is specified in the YAML, meaning
    /// "use the platform default" from agent_sandbox settings.
    pub fn resolved_provider(&self) -> Option<&str> {
        self.ai_provider
            .as_deref()
            .or(self.model.as_deref())
            .or(self.provider.as_deref())
    }

    /// Resolved model id for the CLI (None means "let the CLI pick").
    /// Only the dedicated `ai_model` field counts — we never interpret the
    /// legacy `model` field as a CLI model id, since that field is already
    /// overloaded as a provider alias.
    pub fn resolved_model(&self) -> Option<&str> {
        self.ai_model.as_deref()
    }

    /// Resolved system prompt: `system` takes precedence over `prompt`.
    pub fn resolved_prompt(&self) -> Option<&str> {
        self.system.as_deref().or(self.prompt.as_deref())
    }

    /// Convert inline tools to a JSON value for DB storage.
    pub fn tools_config_json(&self) -> Option<serde_json::Value> {
        self.tools.as_ref().map(|tools| {
            serde_json::Value::Array(
                tools
                    .iter()
                    .map(|t| {
                        let mut obj = serde_json::Map::new();
                        obj.insert(
                            "type".to_string(),
                            serde_json::Value::String(t.tool_type.clone()),
                        );
                        if let serde_json::Value::Object(extra) = &t.config {
                            for (k, v) in extra {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                        serde_json::Value::Object(obj)
                    })
                    .collect(),
            )
        })
    }

    /// Convert MCP server slugs to a JSON array for DB storage.
    /// Output: `["github", "sentry"]`
    pub fn mcp_servers_json(&self) -> Option<serde_json::Value> {
        self.mcp_servers.as_ref().map(|slugs| {
            serde_json::Value::Array(
                slugs
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            )
        })
    }

    /// Convert skill slugs to a JSON array for DB storage.
    /// Output: `["blog-writer", "seo-optimizer"]`
    pub fn skills_config_json(&self) -> Option<serde_json::Value> {
        self.skills.as_ref().map(|slugs| {
            serde_json::Value::Array(
                slugs
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            )
        })
    }

    /// Convert the `on:` triggers to a JSON value for DB storage
    pub fn trigger_config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": self.on.error.as_ref().map(|e| serde_json::json!({
                "new_issue": e.new_issue,
                "regression": e.regression,
            })),
            "deploy": self.on.deploy.as_ref().map(|d| serde_json::json!({
                "production": d.production,
                "preview": d.preview,
            })),
            "monitoring": self.on.monitoring.as_ref().map(|m| serde_json::json!({
                "downtime": m.downtime,
                "latency_spike": m.latency_spike,
            })),
            "schedule": self.on.schedule.as_ref().map(|s| serde_json::json!({
                "cron": s.cron,
            })),
            "manual": self.on.manual,
            "webhook": self.on.webhook,
        })
    }

    /// Derive a slug from the agent name (lowercase, hyphens)
    pub fn slug(&self) -> String {
        self.name
            .to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }
}

// ── Workflow YAML config ──────────────────────────────────────────────────────

/// Workflow YAML configuration from .temps/workflows/*.yaml
///
/// All workflows are AI-powered. The `prompt` field IS the workflow logic.
/// The AI harness (Claude/Codex/OpenCode) is the workflow engine.
/// Template variables ({{error_type}}, {{deployment_id}}) inject event context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowYamlConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub on: WorkflowTriggers,
    /// The prompt IS the workflow. Template variables are injected by Temps
    /// based on the trigger type.
    pub prompt: String,
    /// AI provider override. `None` means "use the platform default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Preferred model for the CLI (e.g. "sonnet", "gpt-5-codex"). `None` = CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
    #[serde(default = "default_max_turns")]
    pub max_turns: i32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: i32,
    #[serde(default = "default_budget")]
    pub daily_budget_cents: i32,
    #[serde(default = "default_cooldown")]
    pub cooldown_minutes: i32,
    /// "pull_request", "report", or "none"
    #[serde(default = "default_deliverable")]
    pub deliverable: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// CPU cores allocated to the sandbox (e.g. 1.0, 2.5). `None` falls back
    /// to the platform default (`AgentSandboxSettings.cpu_limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_limit: Option<f64>,
    /// Memory limit in MB. `None` falls back to the platform default
    /// (`AgentSandboxSettings.memory_limit_mb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_mb: Option<u64>,
    /// Sandbox override. None = use platform default, Some(true) = force on.
    /// Some(false) is rejected at sync time — sandbox execution is mandatory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<bool>,
    /// Inline tool definitions available to the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AgentToolConfig>>,
    /// MCP server slugs referencing project- or platform-level definitions.
    /// Each slug is resolved at runtime and injected into the sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,
    /// Skill slugs referencing project- or platform-level definitions.
    /// Each slug is resolved at runtime and written to `.claude/skills/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Private config repo containing a `.claude/` directory (skills, MCP
    /// servers, settings). Format: "owner/repo". Cloned at runtime and
    /// overlaid into the sandbox's `/home/temps/.claude/` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_repo: Option<String>,
    /// Branch of the config repo to use (default: "main").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_repo_branch: Option<String>,
}

/// All possible workflow triggers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTriggers {
    /// Trigger on error events
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorTrigger>,
    /// Trigger on deployments
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployTrigger>,
    /// Trigger on monitoring events
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring: Option<MonitoringTrigger>,
    /// Trigger on a cron schedule
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleTrigger>,
    /// Allow manual triggering
    #[serde(default = "default_true")]
    pub manual: bool,
}

impl Default for WorkflowTriggers {
    fn default() -> Self {
        Self {
            error: None,
            deploy: None,
            monitoring: None,
            schedule: None,
            manual: true,
        }
    }
}

/// Trigger when a deployment completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployTrigger {
    /// Trigger on production deployments
    #[serde(default)]
    pub production: bool,
    /// Trigger on preview deployments
    #[serde(default)]
    pub preview: bool,
}

/// Trigger on monitoring events (uptime, latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringTrigger {
    /// Trigger when a monitor goes down
    #[serde(default)]
    pub downtime: bool,
    /// Trigger on latency spikes
    #[serde(default)]
    pub latency_spike: bool,
}

impl WorkflowYamlConfig {
    /// Derive a slug from the workflow name (lowercase, hyphens).
    pub fn slug(&self) -> String {
        self.name
            .to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }

    /// Convert the `on:` triggers to a JSON value for DB storage.
    pub fn trigger_config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": self.on.error.as_ref().map(|e| serde_json::json!({
                "new_issue": e.new_issue,
                "regression": e.regression,
            })),
            "deploy": self.on.deploy.as_ref().map(|d| serde_json::json!({
                "production": d.production,
                "preview": d.preview,
            })),
            "monitoring": self.on.monitoring.as_ref().map(|m| serde_json::json!({
                "downtime": m.downtime,
                "latency_spike": m.latency_spike,
            })),
            "schedule": self.on.schedule.as_ref().map(|s| serde_json::json!({
                "cron": s.cron,
            })),
            "manual": self.on.manual,
        })
    }
}

impl TempsConfig {
    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Serialize configuration to YAML string
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    /// Check if configuration has any cron jobs defined
    pub fn has_crons(&self) -> bool {
        self.cron.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Get cron jobs, or empty vec if none defined
    pub fn cron_jobs(&self) -> Vec<&CronJobConfig> {
        self.cron.as_ref().map_or(vec![], |c| c.iter().collect())
    }

    /// Check if configuration has custom build settings
    pub fn has_build_config(&self) -> bool {
        self.build.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete_config() {
        let yaml = r#"
cron:
  - path: /api/cron/cleanup
    schedule: "0 0 * * *"
    name: "Daily Cleanup"
  - path: /api/cron/reports
    schedule: "0 9 * * 1"
    name: "Weekly Reports"

build:
  dockerfile: docker/Dockerfile
  context: .
  args:
    NODE_ENV: production
    API_URL: https://api.example.com

env:
  DATABASE_URL: postgres://localhost/db
  REDIS_URL: redis://localhost:6379

health:
  path: /health
  status: 200
  interval: 30
  timeout: 5
  retries: 3
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();

        // Verify cron jobs
        assert!(config.has_crons());
        let crons = config.cron.as_ref().unwrap();
        assert_eq!(crons.len(), 2);
        assert_eq!(crons[0].path, "/api/cron/cleanup");
        assert_eq!(crons[0].schedule, "0 0 * * *");
        assert_eq!(crons[0].name.as_deref(), Some("Daily Cleanup"));
        assert_eq!(crons[1].path, "/api/cron/reports");
        assert_eq!(crons[1].schedule, "0 9 * * 1");

        // Verify build config
        assert!(config.has_build_config());
        let build = config.build.as_ref().unwrap();
        assert_eq!(build.dockerfile.as_deref(), Some("docker/Dockerfile"));
        assert_eq!(build.context.as_deref(), Some("."));
        assert_eq!(
            build.args.as_ref().unwrap().get("NODE_ENV"),
            Some(&"production".to_string())
        );

        // Verify env vars
        let env = config.env.as_ref().unwrap();
        assert_eq!(
            env.get("DATABASE_URL"),
            Some(&"postgres://localhost/db".to_string())
        );

        // Verify health check
        let health = config.health.as_ref().unwrap();
        assert_eq!(health.path, "/health");
        assert_eq!(health.status, 200);
        assert_eq!(health.interval, 30);
        assert_eq!(health.timeout, 5);
        assert_eq!(health.retries, 3);
    }

    #[test]
    fn test_parse_minimal_config() {
        let yaml = r#"
cron:
  - path: /api/cron/task
    schedule: "*/5 * * * *"
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(config.has_crons());
        assert!(!config.has_build_config());
        assert!(config.env.is_none());
        assert!(config.health.is_none());

        let crons = config.cron_jobs();
        assert_eq!(crons.len(), 1);
        assert_eq!(crons[0].path, "/api/cron/task");
    }

    #[test]
    fn test_parse_empty_config() {
        let yaml = "";
        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
        assert!(!config.has_build_config());
        assert_eq!(config.cron_jobs().len(), 0);
    }

    #[test]
    fn test_parse_config_no_crons() {
        let yaml = r#"
build:
  dockerfile: Dockerfile
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        assert!(!config.has_crons());
        assert!(config.has_build_config());
    }

    #[test]
    fn test_serialize_config() {
        let config = TempsConfig {
            cron: Some(vec![CronJobConfig {
                path: "/api/cron/test".to_string(),
                schedule: "0 0 * * *".to_string(),
                name: Some("Test Cron".to_string()),
            }]),
            build: None,
            env: None,
            health: None,
            agents: None,
            workflows: None,
        };

        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("path: /api/cron/test"));
        assert!(yaml.contains("schedule: 0 0 * * *"));
    }

    #[test]
    fn test_health_check_defaults() {
        let yaml = r#"
health:
  path: /health
"#;

        let config = TempsConfig::from_yaml(yaml).unwrap();
        let health = config.health.as_ref().unwrap();
        assert_eq!(health.path, "/health");
        assert_eq!(health.status, 200); // default
        assert_eq!(health.interval, 30); // default
        assert_eq!(health.timeout, 5); // default
        assert_eq!(health.retries, 3); // default
    }

    #[test]
    fn test_agent_yaml_parsing() {
        let yaml = r#"
name: error-fixer
description: Fixes production errors
on:
  error:
    new_issue: true
    regression: true
  manual: true
prompt: |
  Fix the error: {{error_type}}
provider: claude_cli
max_turns: 25
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.name, "error-fixer");
        assert_eq!(agent.slug(), "error-fixer");
        assert!(agent.on.error.as_ref().unwrap().new_issue);
        assert!(agent.prompt.unwrap().contains("{{error_type}}"));
    }

    #[test]
    fn test_agent_yaml_defaults() {
        let yaml = "name: minimal-agent\n";
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.provider, None);
        assert_eq!(agent.max_turns, 25);
        assert_eq!(agent.timeout_seconds, 600);
        assert!(agent.enabled);
    }

    #[test]
    fn test_agent_slug_normalizes_spaces_and_special_chars() {
        let yaml = "name: My Cool Agent!\n";
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.slug(), "my-cool-agent");
    }

    #[test]
    fn test_workflow_yaml_parsing() {
        let yaml = r#"
name: Error Autofix
description: Investigates and fixes new production errors
on:
  error:
    new_issue: true
    regression: true
  manual: true
prompt: |
  A production error was detected:
  Type: {{error_type}}
  Message: {{error_message}}
provider: claude_cli
max_turns: 25
deliverable: pull_request
"#;
        let workflow: WorkflowYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(workflow.name, "Error Autofix");
        assert_eq!(workflow.slug(), "error-autofix");
        assert!(workflow.on.error.as_ref().unwrap().new_issue);
        assert!(workflow.prompt.contains("{{error_type}}"));
        assert_eq!(workflow.deliverable, "pull_request");
    }

    #[test]
    fn test_workflow_deploy_trigger() {
        let yaml = r#"
name: Deploy Guardian
on:
  deploy:
    production: true
    preview: false
prompt: Watch this deploy for regressions
deliverable: report
"#;
        let workflow: WorkflowYamlConfig = serde_yaml::from_str(yaml).unwrap();
        let deploy = workflow.on.deploy.as_ref().unwrap();
        assert!(deploy.production);
        assert!(!deploy.preview);
        assert_eq!(workflow.deliverable, "report");
    }

    #[test]
    fn test_workflow_monitoring_trigger() {
        let yaml = r#"
name: Downtime Investigator
on:
  monitoring:
    downtime: true
    latency_spike: false
prompt: Investigate the downtime
deliverable: report
"#;
        let workflow: WorkflowYamlConfig = serde_yaml::from_str(yaml).unwrap();
        let monitoring = workflow.on.monitoring.as_ref().unwrap();
        assert!(monitoring.downtime);
        assert!(!monitoring.latency_spike);
    }

    #[test]
    fn test_workflow_trigger_config_json() {
        let yaml = r#"
name: test-workflow
on:
  error:
    new_issue: true
    regression: false
  deploy:
    production: true
  schedule:
    cron: "0 */6 * * *"
  manual: false
prompt: Do something
"#;
        let workflow: WorkflowYamlConfig = serde_yaml::from_str(yaml).unwrap();
        let json = workflow.trigger_config_json();
        assert_eq!(json["manual"], false);
        assert_eq!(json["error"]["new_issue"], true);
        assert_eq!(json["deploy"]["production"], true);
        assert_eq!(json["schedule"]["cron"], "0 */6 * * *");
    }

    #[test]
    fn test_workflow_defaults() {
        let yaml = r#"
name: minimal
prompt: do something
"#;
        let workflow: WorkflowYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(workflow.provider, None);
        assert_eq!(workflow.max_turns, 25);
        assert_eq!(workflow.timeout_seconds, 600);
        assert!(workflow.enabled);
        assert!(workflow.on.manual);
    }

    #[test]
    fn test_agent_trigger_config_json() {
        let yaml = r#"
name: test-agent
on:
  error:
    new_issue: true
    regression: false
  schedule:
    cron: "0 9 * * 1"
  manual: false
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        let json = agent.trigger_config_json();
        assert_eq!(json["manual"], false);
        assert_eq!(json["error"]["new_issue"], true);
        assert_eq!(json["error"]["regression"], false);
        assert_eq!(json["schedule"]["cron"], "0 9 * * 1");
    }

    #[test]
    fn test_agent_yaml_with_config_repo() {
        let yaml = r#"
name: error-fixer
on:
  error:
    new_issue: true
config_repo: myorg/claude-config
config_repo_branch: develop
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.config_repo.as_deref(), Some("myorg/claude-config"));
        assert_eq!(agent.config_repo_branch.as_deref(), Some("develop"));
    }

    #[test]
    fn test_agent_yaml_without_config_repo_defaults_to_none() {
        let yaml = "name: minimal-agent\n";
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(agent.config_repo.is_none());
        assert!(agent.config_repo_branch.is_none());
    }

    #[test]
    fn test_agent_yaml_model_alias_takes_precedence_over_provider() {
        let yaml = r#"
name: test-agent
model: opencode
provider: claude_cli
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.resolved_provider(), Some("opencode"));
    }

    #[test]
    fn test_agent_yaml_system_alias_takes_precedence_over_prompt() {
        let yaml = r#"
name: test-agent
system: You are a system prompt
prompt: You are a prompt
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.resolved_prompt(), Some("You are a system prompt"));
    }

    #[test]
    fn test_agent_yaml_prompt_used_when_no_system() {
        let yaml = r#"
name: test-agent
prompt: You are a prompt
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.resolved_prompt(), Some("You are a prompt"));
    }

    #[test]
    fn test_agent_yaml_no_provider_returns_none() {
        let yaml = "name: test-agent\n";
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.resolved_provider(), None);
    }

    #[test]
    fn test_agent_yaml_with_tools() {
        let yaml = r#"
name: test-agent
model: claude_cli
system: Fix errors
tools:
  - type: web_search
  - type: mcp
    server: my-server
    url: http://localhost:8080
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        let tools = agent.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].tool_type, "web_search");
        assert_eq!(tools[1].tool_type, "mcp");

        let json = agent.tools_config_json().unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr[0]["type"], "web_search");
        assert_eq!(arr[1]["type"], "mcp");
        assert_eq!(arr[1]["server"], "my-server");
    }

    #[test]
    fn test_agent_yaml_claude_managed_style() {
        // Full Claude Managed Agents-style YAML
        let yaml = r#"
name: Coding Assistant
description: Fixes production errors and creates PRs
model: claude_cli
system: |
  You are a helpful coding agent. Analyze errors and create fixes.
  Always write tests for your changes.
on:
  error:
    new_issue: true
  manual: true
tools:
  - type: web_search
deliverable: pull_request
max_turns: 30
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.name, "Coding Assistant");
        assert_eq!(agent.resolved_provider(), Some("claude_cli"));
        assert!(agent
            .resolved_prompt()
            .unwrap()
            .contains("helpful coding agent"));
        assert_eq!(agent.tools.as_ref().unwrap().len(), 1);
        assert_eq!(agent.deliverable, "pull_request");
        assert_eq!(agent.max_turns, 30);
    }

    #[test]
    fn test_custom_tools_yaml_roundtrip() {
        let yaml = r#"
name: Tool Agent
on:
  manual: true
tools:
  - type: custom
    name: get_weather
    description: Get current weather for a location
    webhook_url: https://api.example.com/weather
    input_schema:
      type: object
      properties:
        city:
          type: string
          description: City name
      required:
        - city
    headers:
      Authorization: "Bearer ${TEMPS_SECRET:weather_key}"
  - type: custom
    name: query_db
    description: Run a database query
    webhook_url: https://api.example.com/query
    input_schema:
      type: object
      properties:
        query:
          type: string
      required:
        - query
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(agent.tools.as_ref().unwrap().len(), 2);

        let tools_json = agent.tools_config_json().unwrap();
        let arr = tools_json.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // First tool should have all custom fields preserved
        let t0 = &arr[0];
        assert_eq!(t0["type"], "custom");
        assert_eq!(t0["name"], "get_weather");
        assert_eq!(t0["webhook_url"], "https://api.example.com/weather");
        assert!(t0["input_schema"]["properties"]["city"].is_object());
        assert_eq!(
            t0["headers"]["Authorization"],
            "Bearer ${TEMPS_SECRET:weather_key}"
        );

        // Second tool
        let t1 = &arr[1];
        assert_eq!(t1["type"], "custom");
        assert_eq!(t1["name"], "query_db");
    }

    #[test]
    fn test_agent_yaml_with_skill_and_mcp_slugs() {
        let yaml = r#"
name: content-writer
skills: [blog-writer, seo-optimizer]
mcp_servers: [browser, github]
"#;
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            agent.skills.as_ref().unwrap(),
            &["blog-writer", "seo-optimizer"]
        );
        assert_eq!(agent.mcp_servers.as_ref().unwrap(), &["browser", "github"]);

        let skills_json = agent.skills_config_json().unwrap();
        assert_eq!(
            skills_json,
            serde_json::json!(["blog-writer", "seo-optimizer"])
        );

        let mcp_json = agent.mcp_servers_json().unwrap();
        assert_eq!(mcp_json, serde_json::json!(["browser", "github"]));
    }

    #[test]
    fn test_agent_yaml_no_skills_or_mcp_defaults_to_none() {
        let yaml = "name: minimal\n";
        let agent: AgentYamlConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(agent.skills.is_none());
        assert!(agent.mcp_servers.is_none());
        assert!(agent.skills_config_json().is_none());
        assert!(agent.mcp_servers_json().is_none());
    }
}
