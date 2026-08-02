use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Autopilot config not found for project {project_id}")]
    ConfigNotFound { project_id: i32 },

    #[error("Agent '{slug}' not found in project {project_id}")]
    AgentNotFound { project_id: i32, slug: String },

    #[error("Autopilot run {run_id} not found")]
    RunNotFound { run_id: i32 },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Daily budget exceeded for project {project_id}: spent {spent_cents} of {daily_limit_cents} cents")]
    BudgetExceeded {
        project_id: i32,
        daily_limit_cents: i32,
        spent_cents: i32,
    },

    #[error("Cooldown active for project {project_id}: {minutes_remaining} minutes remaining")]
    CooldownActive {
        project_id: i32,
        minutes_remaining: i32,
    },

    #[error("AI CLI '{provider}' is not installed or not found in PATH")]
    AiCliNotInstalled { provider: String },

    #[error("AI CLI '{provider}' failed with exit code {exit_code}: {stderr}")]
    AiCliFailed {
        provider: String,
        exit_code: i32,
        stderr: String,
    },

    #[error("AI CLI '{provider}' timed out after {timeout_secs} seconds")]
    AiCliTimeout { provider: String, timeout_secs: u64 },

    #[error("Git operation failed: {message}")]
    GitError { message: String },

    #[error("Encryption error: {message}")]
    EncryptionError { message: String },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Sandbox creation failed for run {run_id} ({provider}): {reason}")]
    SandboxCreationFailed {
        run_id: i32,
        provider: String,
        reason: String,
    },

    #[error("Sandbox not found for run {run_id}")]
    SandboxNotFound { run_id: i32 },

    #[error("Sandbox exec failed for run {run_id} in sandbox {sandbox_id}: {reason}")]
    SandboxExecFailed {
        run_id: i32,
        sandbox_id: String,
        reason: String,
    },

    #[error("Sandbox provider '{provider}' unavailable: {reason}")]
    SandboxProviderUnavailable { provider: String, reason: String },

    #[error("Secret '{name}' not found")]
    SecretNotFound { name: String },

    #[error("Skill definition '{slug}' not found in project {project_id}")]
    SkillDefinitionNotFound { project_id: i32, slug: String },

    #[error("MCP definition '{slug}' not found in project {project_id}")]
    McpDefinitionNotFound { project_id: i32, slug: String },

    #[error("MCP config field '{field}' was not found for server '{slug}'")]
    McpConfigFieldNotFound { slug: String, field: String },

    #[error("A skill with slug '{slug}' already exists{}", scope_label(*project_id))]
    SkillDefinitionAlreadyExists {
        project_id: Option<i32>,
        slug: String,
    },

    #[error("An MCP server with slug '{slug}' already exists{}", scope_label(*project_id))]
    McpDefinitionAlreadyExists {
        project_id: Option<i32>,
        slug: String,
    },
}

fn scope_label(project_id: Option<i32>) -> String {
    match project_id {
        Some(id) => format!(" in project {}", id),
        None => " (global)".to_string(),
    }
}

impl From<sea_orm::DbErr> for AgentError {
    fn from(error: sea_orm::DbErr) -> Self {
        // Postgres unique-constraint violations surface as `DbErr::Exec` /
        // `DbErr::Query` carrying the raw libpq error string. Detect them so
        // race-condition duplicates (two concurrent inserts slip past the
        // service-layer existence check) don't leak as opaque 500s.
        let msg = error.to_string();
        if msg.contains("duplicate key value violates unique constraint") {
            if let Some(slug) = extract_dup_slug(&msg) {
                if msg.contains("skill_definitions") {
                    return AgentError::SkillDefinitionAlreadyExists {
                        project_id: None,
                        slug,
                    };
                }
                if msg.contains("mcp_definitions") {
                    return AgentError::McpDefinitionAlreadyExists {
                        project_id: None,
                        slug,
                    };
                }
            }
        }
        match &error {
            sea_orm::DbErr::RecordNotFound(_) => AgentError::RunNotFound { run_id: 0 },
            sea_orm::DbErr::RecordNotInserted => AgentError::Validation {
                message: format!("Duplicate record or constraint violation: {}", error),
            },
            _ => AgentError::Database(error),
        }
    }
}

/// Try to pull the slug out of a Postgres unique-violation message.
/// Postgres includes `DETAIL: Key (slug)=(foo) already exists.` in its
/// error body. Missing DETAIL just means we fall back to `None`.
fn extract_dup_slug(msg: &str) -> Option<String> {
    let key_idx = msg.find("Key (")?;
    let rest = &msg[key_idx + 5..];
    let val_start = rest.find(")=(")? + 3;
    let val_end = rest[val_start..].find(')')?;
    Some(rest[val_start..val_start + val_end].to_string())
}
