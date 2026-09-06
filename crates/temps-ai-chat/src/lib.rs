// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistent, resumable AI debugging conversations (ADR-023).
//!
//! A generic conversation store keyed by a polymorphic `(context_type,
//! context_id)` — one resumable chat per interaction. [`ConversationService`]
//! owns create/find/history + streaming `send_message`; each context type
//! supplies a [`ConversationContextProvider`] that seeds the chat (the
//! deployment provider seeds from a failure diagnosis). Built on the `temps-ai`
//! foundation; the AI is injected as `Arc<dyn AiService>`.

pub mod applications;
pub mod audit;
pub mod handlers;
pub mod pending_actions;
pub mod plugin;
pub mod provider;
pub mod providers;
mod sensitive;
pub mod service;

pub use applications::{ApplicationError, ApplicationService, ApplicationWorkspaceService};
pub use pending_actions::{PendingActionError, PendingActionService};
pub use plugin::AiChatPlugin;
pub use provider::{ConversationContextProvider, ConversationSeed};
pub use providers::alert::AlertChatProvider;
pub use providers::api_tools::ApiToolsProvider;
pub use providers::application::ApplicationChatProvider;
pub use providers::deployment::DeploymentChatProvider;
pub use providers::global::GlobalChatProvider;
pub use providers::project::ProjectChatProvider;
pub use service::{ChatStreamEvent, ConversationService, HarnessMcpError, PendingPermissionEntry};

/// Errors from the conversation layer. All map cleanly to HTTP at the handler.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("conversation '{0}' not found")]
    NotFound(String),
    #[error("project {0} not found")]
    ProjectNotFound(i32),
    #[error("no AI context provider registered for type '{0}'")]
    NoProvider(String),
    #[error("AI is not configured for this project")]
    AiUnavailable,
    #[error("context not found or not accessible")]
    ContextUnavailable,
    #[error("failed to load project {project_id} for AI chat readiness: {source}")]
    ProjectLookup {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
    #[error("AI provider error: {0}")]
    Ai(String),
    #[error(transparent)]
    AuthorizationRefresh(#[from] ToolAuthorizationRefreshError),
    #[error("the assistant claimed a proposal was staged without a write-tool receipt")]
    ProposalNotStaged,
    #[error("conversation '{conversation_id}' already has an active AI turn")]
    TurnInProgress { conversation_id: String },
    #[error("turn '{turn_id}' was already submitted for conversation '{conversation_id}'")]
    DuplicateTurn {
        conversation_id: String,
        turn_id: String,
    },
    #[error("failed to prepare the application harness workspace: {0}")]
    ApplicationWorkspace(#[from] ApplicationError),
    /// Submitted `PermissionDecision` variant is incompatible with the kind the
    /// CLI requested.  E.g. sending `allow_tool` for a `question` permission.
    #[error(
        "permission decision type mismatch: expected a decision for kind \
         '{expected_kind}', received '{received}'"
    )]
    PermissionKindMismatch {
        expected_kind: String,
        received: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ToolAuthorizationRefreshError {
    #[error("the authenticated browser session has no durable identity")]
    MissingSessionIdentity,
    #[error("the authenticated principal or credential is no longer active")]
    PrincipalInactive,
    #[error("deployment credentials cannot execute conversation tools")]
    UnsupportedCredential,
    #[error("stored API-key role '{0}' is invalid")]
    InvalidRole(String),
    #[error("stored API-key permissions are invalid")]
    InvalidPermissions,
    #[error("permission to execute in the harness workspace has been revoked")]
    HarnessPermissionRevoked,
    #[error("could not verify current application project access")]
    ProjectAccessCheckFailed,
    #[error("failed to refresh tool authorization: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// A browser-safe explanation of a chat failure.
///
/// Provider and harness errors routinely contain host paths, subprocess
/// arguments, account identifiers, and occasionally credential-shaped values.
/// Those raw diagnostics belong in server logs, never in the conversation
/// wire. The UI receives this deliberately small, actionable contract instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublicChatFailure {
    pub code: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub retryable: bool,
}

impl ChatError {
    pub fn public_failure(&self) -> PublicChatFailure {
        match self {
            Self::Ai(reason) => classify_ai_failure(reason),
            Self::AiUnavailable => PublicChatFailure {
                code: "ai_not_configured",
                title: "AI harness unavailable",
                detail: "No usable AI harness or API provider is configured for this conversation. Configure or authenticate one, refresh its status, and retry.",
                retryable: false,
            },
            Self::ApplicationWorkspace(_) => PublicChatFailure {
                code: "sandbox_unavailable",
                title: "Application sandbox unavailable",
                detail: "Temps could not prepare the persistent application sandbox for this turn. Check the sandbox status and retry.",
                retryable: true,
            },
            Self::ProposalNotStaged => PublicChatFailure {
                code: "proposal_not_staged",
                title: "Proposal was not staged",
                detail: "The AI described a proposal but did not submit it to Temps, so no approval card exists and no change was made. Retry the request to create a fresh proposal.",
                retryable: true,
            },
            Self::Db(_) | Self::ProjectLookup { .. } => PublicChatFailure {
                code: "chat_storage_unavailable",
                title: "Conversation storage unavailable",
                detail: "Temps could not access the conversation store. Your existing messages remain saved; reconnect and retry.",
                retryable: true,
            },
            Self::TurnInProgress { .. } | Self::DuplicateTurn { .. } => PublicChatFailure {
                code: "turn_already_running",
                title: "A turn is already running",
                detail: "This conversation is already processing a message. Wait for it to finish or stop it before retrying.",
                retryable: true,
            },
            Self::AuthorizationRefresh(_) => PublicChatFailure {
                code: "authorization_changed",
                title: "Authorization changed",
                detail: "Temps stopped the tool call because the session, API key, role, or permissions changed after this turn started. Sign in again or retry with current access.",
                retryable: false,
            },
            _ => PublicChatFailure {
                code: "chat_request_failed",
                title: "AI request could not be completed",
                detail: "Temps could not complete this request. Retry once; if it happens again, check the server logs for the turn's internal diagnostic.",
                retryable: true,
            },
        }
    }
}

fn classify_ai_failure(reason: &str) -> PublicChatFailure {
    let reason = reason.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| reason.contains(needle));

    if reason.contains("mcp")
        && contains_any(&[
            "invalid mcp configuration",
            "permission denied",
            "eacces",
            "failed to read file",
        ])
    {
        return PublicChatFailure {
            code: "tool_configuration_unreadable",
            title: "Application tools could not start",
            detail: "The sandbox could not read Temps' temporary tool configuration, so the harness was stopped before it could reply. Restart the application sandbox and retry; if it persists, check the server logs.",
            retryable: true,
        };
    }

    if contains_any(&[
        "not logged in",
        "authentication failed",
        "authentication required",
        "unauthorized",
        "token refresh failed",
        "invalid api key",
        "bad api key",
        "status 401",
        " 401",
    ]) {
        return PublicChatFailure {
            code: "harness_authentication_required",
            title: "AI harness authentication required",
            detail: "The selected AI harness is not authenticated in the environment where Temps is running. Sign in to that harness, refresh its status, and retry.",
            retryable: false,
        };
    }

    if contains_any(&[
        "rate limit",
        "rate_limit",
        "status 429",
        " 429",
        "quota",
        "credits exhausted",
    ]) {
        return PublicChatFailure {
            code: "provider_rate_limited",
            title: "AI provider limit reached",
            detail: "The selected provider rejected the request because its rate or usage limit was reached. Wait briefly or review that provider account's limits before retrying.",
            retryable: true,
        };
    }

    if reason.contains("model")
        && contains_any(&[
            "not found",
            "not available",
            "unavailable",
            "unsupported",
            "invalid",
            "does not exist",
        ])
    {
        return PublicChatFailure {
            code: "model_unavailable",
            title: "Selected model unavailable",
            detail: "The selected model is not available to this harness or provider account. Refresh the model list, choose an available model, and retry.",
            retryable: false,
        };
    }

    if contains_any(&[
        "requires approval",
        "approval required",
        "permission prompt",
    ]) {
        return PublicChatFailure {
            code: "approval_bridge_unavailable",
            title: "The harness is waiting for approval",
            detail: "A command required approval, but this harness could not return the prompt to Temps. Choose Auto permissions for the sandbox or use a harness with an interactive approval bridge.",
            retryable: false,
        };
    }

    if contains_any(&["timed out", "timeout", "deadline exceeded"]) {
        return PublicChatFailure {
            code: "harness_timeout",
            title: "AI harness timed out",
            detail: "The selected harness did not respond before the turn timeout. Retry, or raise the chat timeout if this task legitimately needs longer.",
            retryable: true,
        };
    }

    if contains_any(&[
        "connection refused",
        "connection reset",
        "failed to connect",
        "network error",
        "dns error",
        "service unavailable",
        "status 502",
        "status 503",
        "status 504",
    ]) {
        return PublicChatFailure {
            code: "provider_unreachable",
            title: "AI provider unreachable",
            detail: "Temps could not reach the selected AI provider. Check network connectivity and provider availability, then retry.",
            retryable: true,
        };
    }

    if reason.contains("sandbox")
        && contains_any(&["unavailable", "could not", "failed", "container", "docker"])
    {
        return PublicChatFailure {
            code: "sandbox_unavailable",
            title: "Application sandbox unavailable",
            detail: "Temps could not prepare or reach the persistent application sandbox for this turn. Check its status and retry.",
            retryable: true,
        };
    }

    if contains_any(&["exited with code", "process exited", "subprocess exited"]) {
        return PublicChatFailure {
            code: "harness_exited",
            title: "AI harness exited unexpectedly",
            detail: "The selected AI harness exited before producing a reply. Verify that it is authenticated and that the selected model is available, then retry.",
            retryable: true,
        };
    }

    if contains_any(&[
        "returned no response",
        "no reply was produced",
        "empty response",
    ]) {
        return PublicChatFailure {
            code: "empty_provider_response",
            title: "AI harness returned no reply",
            detail: "The selected harness finished without producing a reply. Retry once; if it repeats, refresh the harness status and review the server logs.",
            retryable: true,
        };
    }

    PublicChatFailure {
        code: "harness_failed",
        title: "AI harness failed",
        detail: "The selected AI harness failed before it could reply. Retry once; if it repeats, check its authentication, selected model, and the server logs.",
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::ChatError;

    #[test]
    fn public_failure_explains_mcp_permissions_without_exposing_the_host_path() {
        let failure = ChatError::Ai(
            "sandboxed claude_cli exited with code 1: Invalid MCP configuration: EACCES: permission denied, open '/run/secrets/temps-chat-mcp-secret.json' token=secret"
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "tool_configuration_unreadable");
        assert!(failure.detail.contains("temporary tool configuration"));
        assert!(!failure.detail.contains("/run/secrets"));
        assert!(!failure.detail.contains("token=secret"));
        assert!(!failure.detail.contains("claude_cli"));
    }

    #[test]
    fn public_failure_distinguishes_authentication_and_model_errors() {
        let authentication =
            ChatError::Ai("Token refresh failed: 401 for user@example.com".to_string())
                .public_failure();
        let model =
            ChatError::Ai("model claude-private-model was not found".to_string()).public_failure();

        assert_eq!(authentication.code, "harness_authentication_required");
        assert!(!authentication.detail.contains("user@example.com"));
        assert_eq!(model.code, "model_unavailable");
        assert!(!model.detail.contains("claude-private-model"));
    }
}
