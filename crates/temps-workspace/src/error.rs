use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorkspaceError {
    #[error("Workspace session {session_id} not found")]
    SessionNotFound { session_id: i32 },

    #[error("Workspace session {session_id} is not active (status: {status})")]
    SessionNotActive { session_id: i32, status: String },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Sandbox creation failed for session {session_id}: {reason}")]
    SandboxCreationFailed { session_id: i32, reason: String },

    #[error("Sandbox not available for session {session_id}")]
    SandboxNotAvailable { session_id: i32 },

    #[error("AI CLI execution failed for session {session_id}: {reason}")]
    AiCliFailed { session_id: i32, reason: String },

    #[error("AI CLI timed out for session {session_id} after {timeout_secs}s")]
    AiCliTimeout { session_id: i32, timeout_secs: u64 },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Failed to hash preview password for session {session_id}: {reason}")]
    PasswordHashFailed { session_id: i32, reason: String },

    #[error("Memory fact {fact_id} not found in workflow {agent_id} (project {project_id})")]
    MemoryNotFound {
        project_id: i32,
        agent_id: i32,
        fact_id: i64,
    },

    #[error("Workflow {slug} not found in project {project_id}")]
    WorkflowNotFound { project_id: i32, slug: String },

    /// Caller asked for a credential whose `(host, owner, repo)` doesn't
    /// match the project's configured repository. Used by the sandbox
    /// credential daemon's mint endpoint to refuse cross-project token
    /// requests — the daemon can only ever ask for tokens for its own
    /// project's repo, regardless of what user code might try to inject.
    #[error("Project {project_id} cannot request credentials for {requested_host}/{requested_owner}/{requested_repo}: project is configured for {project_repo}")]
    GitCredentialRepoMismatch {
        project_id: i32,
        requested_host: String,
        requested_owner: String,
        requested_repo: String,
        project_repo: String,
    },

    /// Project has no `git_provider_connection_id` set, so we have nothing
    /// to mint a token from. Distinct from `ProjectNotFound` — the project
    /// exists, it just isn't connected to any git provider.
    #[error("Project {project_id} has no git provider connection configured")]
    GitCredentialNoConnection { project_id: i32 },

    /// Underlying provider couldn't mint a scoped token. Covers GitHub API
    /// failures, PAT-backed connections (which can't narrow), and
    /// installation_id mismatches.
    #[error(
        "Failed to mint scoped credential for project {project_id} on {owner}/{repo}: {reason}"
    )]
    GitCredentialMintFailed {
        project_id: i32,
        owner: String,
        repo: String,
        reason: String,
    },

    #[error("Database error: {0}")]
    Database(sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Agent error: {0}")]
    Agent(#[from] temps_agents::error::AgentError),
}

impl From<sea_orm::DbErr> for WorkspaceError {
    /// Generic `From` impl for the `?` operator — preserves the original
    /// error message. Callers that know which resource was missing (session,
    /// project, workflow, memory fact, etc.) MUST construct the correct
    /// typed `NotFound` variant themselves with the real ID, instead of
    /// relying on this fallback. Mapping every `RecordNotFound` to
    /// `SessionNotFound{session_id: 0}` here would mislabel missing
    /// projects, workflows, and memory rows as "session 0 not found".
    fn from(error: sea_orm::DbErr) -> Self {
        WorkspaceError::Database(error)
    }
}
