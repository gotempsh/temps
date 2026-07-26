//! Typed errors for the standalone sandbox API.
//!
//! Every variant includes the identifiers needed to understand the error
//! in isolation (sandbox_id, job_id, path, reason). This follows the
//! codebase-wide rule: error messages must be greppable and include the
//! IDs of the resources involved.

use thiserror::Error;

use temps_agents::error::AgentError;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SandboxError {
    /// The requested sandbox does not exist or has been destroyed.
    #[error("Sandbox {sandbox_id} not found")]
    NotFound { sandbox_id: String },

    /// A background job tracked by [`JobTracker`] was not found in the
    /// requested sandbox. Separate from `NotFound` so callers can tell the
    /// difference between "wrong sandbox id" and "wrong job id".
    #[error("Job {job_id} not found in sandbox {sandbox_id}")]
    JobNotFound { sandbox_id: String, job_id: String },

    /// The underlying provider failed to create the container.
    #[error("Failed to create sandbox for user {user_id}: {reason}")]
    CreateFailed { user_id: i32, reason: String },

    /// Command execution failed inside the sandbox (spawn/attach failure,
    /// not a non-zero exit — a non-zero exit returns `ExecResult` with the
    /// code, it is not an error).
    #[error("Exec failed in sandbox {sandbox_id}: {reason}")]
    ExecFailed { sandbox_id: String, reason: String },

    /// A filesystem operation failed against a path inside the sandbox.
    /// `op` is one of "read" | "write" | "stat" | "mkdir".
    #[error("FS {op} failed for '{path}' in sandbox {sandbox_id}: {reason}")]
    FileOp {
        sandbox_id: String,
        op: String,
        path: String,
        reason: String,
    },

    /// Input validation failed (empty name, invalid timeout, non-absolute
    /// path, etc.). Mapped to HTTP 400.
    #[error("Validation error: {message}")]
    Validation { message: String },

    /// The sandbox exists but is not in a state that allows the requested
    /// operation (e.g. exec on a stopped sandbox). Mapped to HTTP 409.
    #[error("Sandbox {sandbox_id} is in state '{state}' — cannot {operation}")]
    InvalidState {
        sandbox_id: String,
        state: String,
        operation: String,
    },

    /// The sandbox row is attributed to an agent run (`agent_run_id` is
    /// set). Its container is named `temps-sandbox-<run_id>` — not by the
    /// row's public id — and the run owns the lifecycle, so standalone
    /// lifecycle ops (pause/resume/restart/resize, and destroy while the
    /// run is active) must not act on it: the registry would miss the real
    /// container and the DB row would drift from reality. Mapped to
    /// HTTP 409 telling the user to stop/cancel the run instead.
    #[error(
        "Sandbox {sandbox_id} belongs to agent run {run_id} and its lifecycle is managed by that run — stop or cancel agent run {run_id} instead"
    )]
    ManagedByAgentRun { sandbox_id: String, run_id: i32 },

    /// An operation exceeded the per-sandbox timeout.
    #[error("Operation timed out in sandbox {sandbox_id} after {timeout_secs}s")]
    Timeout {
        sandbox_id: String,
        timeout_secs: u64,
    },

    /// Required plumbing is missing at runtime. Indicates a deployment
    /// misconfiguration (e.g. no Docker, no SandboxProvider registered).
    /// Mapped to HTTP 503.
    #[error("Sandbox subsystem unavailable: {reason}")]
    Unavailable { reason: String },

    /// Hashing the preview password failed. Argon2 returns errors only for
    /// catastrophic platform issues (OS RNG failure etc.), so this maps to
    /// HTTP 500.
    #[error("Failed to hash preview password for sandbox {sandbox_id}: {reason}")]
    PasswordHashFailed { sandbox_id: String, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Translate a lower-level `AgentError` from the shared `SandboxProvider`
/// into a `SandboxError` with the standalone sandbox ID attached. The
/// provider's errors are keyed by `run_id` (an internal numeric id) — the
/// caller is responsible for passing the public `sandbox_id` so users see
/// the opaque ID, not the internal integer.
pub fn from_agent_error(sandbox_id: &str, err: AgentError) -> SandboxError {
    match err {
        AgentError::SandboxNotFound { .. } => SandboxError::NotFound {
            sandbox_id: sandbox_id.to_string(),
        },
        AgentError::SandboxExecFailed { reason, .. } => SandboxError::ExecFailed {
            sandbox_id: sandbox_id.to_string(),
            reason,
        },
        AgentError::Io(e) => SandboxError::Io(e),
        other => SandboxError::ExecFailed {
            sandbox_id: sandbox_id.to_string(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_message_includes_id() {
        let err = SandboxError::NotFound {
            sandbox_id: "sbx_abc123".into(),
        };
        assert_eq!(err.to_string(), "Sandbox sbx_abc123 not found");
    }

    #[test]
    fn job_not_found_distinguishes_sandbox_from_job() {
        let err = SandboxError::JobNotFound {
            sandbox_id: "sbx_abc".into(),
            job_id: "job_xyz".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("sbx_abc"), "msg: {}", msg);
        assert!(msg.contains("job_xyz"), "msg: {}", msg);
    }

    #[test]
    fn file_op_message_includes_op_and_path() {
        let err = SandboxError::FileOp {
            sandbox_id: "sbx_a".into(),
            op: "read".into(),
            path: "/etc/hosts".into(),
            reason: "no such file".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("read"), "missing op: {}", msg);
        assert!(msg.contains("/etc/hosts"), "missing path: {}", msg);
        assert!(msg.contains("no such file"), "missing reason: {}", msg);
    }

    #[test]
    fn invalid_state_names_state_and_operation() {
        let err = SandboxError::InvalidState {
            sandbox_id: "sbx_a".into(),
            state: "stopped".into(),
            operation: "exec".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("stopped"));
        assert!(msg.contains("exec"));
    }

    #[test]
    fn managed_by_agent_run_names_sandbox_and_run() {
        let err = SandboxError::ManagedByAgentRun {
            sandbox_id: "sbx_abc".into(),
            run_id: 42,
        };
        let msg = err.to_string();
        assert!(msg.contains("sbx_abc"), "msg: {}", msg);
        assert!(msg.contains("42"), "msg: {}", msg);
        // The whole point of this variant: tell the user what to do instead.
        assert!(msg.contains("stop or cancel"), "msg: {}", msg);
    }

    #[test]
    fn from_agent_error_preserves_not_found() {
        let agent = AgentError::SandboxNotFound { run_id: 42 };
        let err = from_agent_error("sbx_public", agent);
        assert!(matches!(err, SandboxError::NotFound { .. }));
        // The public ID propagates, not the internal run_id
        assert_eq!(err.to_string(), "Sandbox sbx_public not found");
    }

    #[test]
    fn from_agent_error_preserves_exec_failure() {
        let agent = AgentError::SandboxExecFailed {
            run_id: 1,
            sandbox_id: "internal".into(),
            reason: "container died".into(),
        };
        let err = from_agent_error("sbx_pub", agent);
        match err {
            SandboxError::ExecFailed {
                sandbox_id, reason, ..
            } => {
                assert_eq!(sandbox_id, "sbx_pub");
                assert!(reason.contains("container died"));
            }
            other => panic!("expected ExecFailed, got {:?}", other),
        }
    }

    #[test]
    fn from_agent_error_catchall_becomes_exec_failed() {
        let agent = AgentError::AiCliNotInstalled {
            provider: "claude".into(),
        };
        let err = from_agent_error("sbx_x", agent);
        assert!(matches!(err, SandboxError::ExecFailed { .. }));
    }
}
