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
pub mod git_bindings;
pub mod git_bindings_handlers;
pub mod handlers;
pub mod pending_actions;
pub mod plugin;
pub mod provider;
pub mod providers;
mod sensitive;
pub mod service;

pub use applications::{ApplicationError, ApplicationService, ApplicationWorkspaceService};
pub use git_bindings::{GitBindingError, GitBindingService};
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
    #[error("retained AI harness diagnostic for '{purpose}': {reason}")]
    RetainedHarnessDiagnostic { purpose: String, reason: String },
    #[error(transparent)]
    AuthorizationRefresh(#[from] ToolAuthorizationRefreshError),
    #[error("the assistant claimed a proposal was staged without a write-tool receipt")]
    ProposalNotStaged,
    #[error("conversation '{conversation_id}' already has an active AI turn")]
    TurnInProgress { conversation_id: String },
    #[error(
        "application '{application_id}' has active chat turns or a runtime update in progress"
    )]
    WorkspaceRuntimeBusy { application_id: String },
    #[error(
        "application '{application_id}' requires a compatible runtime update before starting chat"
    )]
    WorkspaceRuntimeUpdateRequired { application_id: String },
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
/// Arbitrary raw errors stay in server logs. Only a retained-runtime error
/// marked at its exact-secret-redacted source may add a bounded, scrubbed
/// diagnostic to this browser-visible and persisted contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublicChatFailure {
    pub code: &'static str,
    pub title: &'static str,
    pub detail: String,
    pub retryable: bool,
}

impl ChatError {
    pub fn public_failure(&self) -> PublicChatFailure {
        match self {
            Self::Ai(reason) => classify_ai_failure(reason),
            Self::RetainedHarnessDiagnostic { reason, .. } => {
                let mut failure = classify_ai_failure(reason);
                let diagnostic = sensitive::redact_retained_diagnostic(reason);
                if !diagnostic.is_empty() {
                    let mut detail = format!("Underlying error: {diagnostic}. {}", failure.detail);
                    if detail.len() > 580 {
                        let mut end = 580;
                        while !detail.is_char_boundary(end) {
                            end -= 1;
                        }
                        detail.truncate(end);
                    }
                    failure.detail = detail;
                }
                failure
            }
            Self::AiUnavailable => PublicChatFailure {
                code: "ai_not_configured",
                title: "AI harness unavailable",
                detail: "No usable AI harness or API provider is configured for this conversation. Configure or authenticate one, refresh its status, and retry.".to_string(),
                retryable: false,
            },
            Self::ApplicationWorkspace(_) => PublicChatFailure {
                code: "sandbox_unavailable",
                title: "Application sandbox unavailable",
                detail: "Temps could not prepare the persistent application sandbox for this turn. Check the sandbox status and retry.".to_string(),
                retryable: true,
            },
            Self::ProposalNotStaged => PublicChatFailure {
                code: "proposal_not_staged",
                title: "Proposal was not staged",
                detail: "The AI described a proposal but did not submit it to Temps, so no approval card exists and no change was made. Retry the request to create a fresh proposal.".to_string(),
                retryable: true,
            },
            Self::Db(_) | Self::ProjectLookup { .. } => PublicChatFailure {
                code: "chat_storage_unavailable",
                title: "Conversation storage unavailable",
                detail: "Temps could not access the conversation store. Your existing messages remain saved; reconnect and retry.".to_string(),
                retryable: true,
            },
            Self::WorkspaceRuntimeBusy { .. } => PublicChatFailure {
                code: "workspace_runtime_busy",
                title: "Workspace runtime is busy",
                detail: "Wait for active threads or the runtime update to finish, then retry. Workspace files are preserved.".to_string(),
                retryable: true,
            },
            Self::WorkspaceRuntimeUpdateRequired { .. } => PublicChatFailure {
                code: "workspace_runtime_update_required",
                title: "Runtime update required",
                detail: "This sandbox runtime is incompatible with Temps. Open Workspace settings and choose Update runtime. Restarting the same image will not fix this.".to_string(),
                retryable: false,
            },
            Self::TurnInProgress { .. } | Self::DuplicateTurn { .. } => PublicChatFailure {
                code: "turn_already_running",
                title: "A turn is already running",
                detail: "This conversation is already processing a message. Wait for it to finish or stop it before retrying.".to_string(),
                retryable: true,
            },
            Self::AuthorizationRefresh(_) => PublicChatFailure {
                code: "authorization_changed",
                title: "Authorization changed",
                detail: "Temps stopped the tool call because the session, API key, role, or permissions changed after this turn started. Sign in again or retry with current access.".to_string(),
                retryable: false,
            },
            _ => PublicChatFailure {
                code: "chat_request_failed",
                title: "AI request could not be completed",
                detail: "Temps could not complete this request. Retry once; if it happens again, check the server logs for the turn's internal diagnostic.".to_string(),
                retryable: true,
            },
        }
    }
}

pub(crate) fn classify_ai_failure(reason: &str) -> PublicChatFailure {
    let reason = reason.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| reason.contains(needle));

    // These are server-owned runtime diagnostics, not arbitrary provider text.
    // Match exact causes but never echo the surrounding error, which may carry
    // credentials, host paths, or request bodies into persisted chat metadata.
    if contains_any(&[
        "unsupported retained sandbox permission mode full-access",
        "unsupported retained sandbox permission mode 'full-access'",
    ]) {
        return PublicChatFailure {
            code: "sandbox_permission_mode_unsupported",
            title: "Sandbox permission mode unsupported",
            detail: "Underlying error: unsupported retained sandbox permission mode full-access. Select a supported permission mode and retry.".to_string(),
            retryable: false,
        };
    }
    if contains_any(&[
        "another harness turn already running",
        "another harness turn is already running",
    ]) {
        return PublicChatFailure {
            code: "harness_turn_already_running",
            title: "Another harness turn is running",
            detail: "Underlying error: another harness turn already running. Wait for it to finish or stop it before retrying.".to_string(),
            retryable: true,
        };
    }
    if reason.contains("remote runtime carrier disconnected") {
        return PublicChatFailure {
            code: "sandbox_runtime_disconnected",
            title: "Sandbox runtime disconnected",
            detail: "Underlying error: remote runtime carrier disconnected. Restart the application sandbox and retry.".to_string(),
            retryable: true,
        };
    }

    // This is a Temps relay rejection, not a silent exit or an upstream
    // account-access failure. Preserve that cause even when the turn finalizer
    // wraps it with "no reply". Never echo the raw diagnostic: it may also
    // contain credentials, host paths, or untrusted model strings.
    if reason.contains("not an allowed concrete claude model for sandbox execution") {
        return PublicChatFailure {
            code: "model_unavailable",
            title: "Claude model rejected by workspace relay",
            detail: "Underlying error: Temps' workspace model relay rejected the selected Claude model selector before Claude could start. Refresh the model list and select a supported model, then retry. This is a Temps model-selection error, not an empty Claude response.".to_string(),
            retryable: false,
        };
    }

    // Docker includes internal network names and allocator diagnostics in these
    // errors. Preserve the actionable cause while keeping those details out of
    // the conversation wire. This must precede the generic provider-network
    // and sandbox classifiers below.
    if contains_any(&["fully subnetted", "non-overlapping ipv4"])
        || (reason.contains("address pool") && contains_any(&["sandbox", "docker", "network"]))
    {
        return PublicChatFailure {
            code: "sandbox_network_capacity_exhausted",
            title: "Application sandbox network capacity exhausted",
            detail: "This Temps host has no private sandbox network capacity available. Contact your Temps administrator, then try again.".to_string(),
            retryable: true,
        };
    }

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
            detail: "The sandbox could not read Temps' temporary tool configuration, so the harness was stopped before it could reply. Restart the application sandbox and retry; if it persists, check the server logs.".to_string(),
            retryable: true,
        };
    }

    if contains_any(&[
        "oauth credentials are not supported",
        "oauth credential is not supported",
        "oauth is not supported",
        "does not support oauth",
        "unsupported oauth",
        "unsupported auth type",
        "unsupported authentication type",
        "unsupported authentication format",
        "unsupported credential type",
        "unsupported credential format",
    ]) {
        return PublicChatFailure {
            code: "unsupported_workspace_credential",
            title: "AI credential format unsupported",
            detail: "The selected harness cannot use this provider credential format. Update the saved credential to a supported API credential or choose a harness that supports this login type, then retry.".to_string(),
            retryable: false,
        };
    }

    if contains_any(&[
        "invalid workspace credential",
        "invalid credential format",
        "invalid credential file",
        "multiple credential entries",
        "multiple credentials are not supported",
        "exactly one direct",
        "unsupported fields",
        "unsupported or invalid fields",
        "duplicate credential",
        "duplicated credential",
    ]) {
        return PublicChatFailure {
            code: "invalid_workspace_credential",
            title: "Saved AI credential is invalid",
            detail: "The saved workspace credential is not in the format this harness accepts. Update it with one supported provider credential, then retry.".to_string(),
            retryable: false,
        };
    }

    if contains_any(&[
        "authentication expired",
        "authentication revoked",
        "credential expired",
        "credential revoked",
        "token expired",
        "token revoked",
        "expired token",
        "revoked token",
        "refresh token expired",
        "refresh token revoked",
    ]) {
        return PublicChatFailure {
            code: "harness_authentication_required",
            title: "AI harness authentication expired",
            detail: "The selected harness login expired or was revoked. Update the saved credential, or refresh a supported local login on the machine running Temps, then retry.".to_string(),
            retryable: false,
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
            detail: "The selected AI harness is not authenticated in the environment where Temps is running. Update the saved credential, or refresh a supported local login on the machine running Temps, then retry.".to_string(),
            retryable: false,
        };
    }

    if contains_any(&[
        "quota exhausted",
        "quota exceeded",
        "insufficient quota",
        "usage limit reached",
        "credits exhausted",
        "out of credits",
        "billing limit",
    ]) {
        return PublicChatFailure {
            code: "provider_quota_exhausted",
            title: "AI provider quota exhausted",
            detail: "The selected provider account has no remaining usage quota. Review its billing, credits, or usage limits, then retry after quota is available.".to_string(),
            retryable: false,
        };
    }

    if contains_any(&[
        "rate limit",
        "rate_limit",
        "status 429",
        " 429",
        "too many requests",
    ]) {
        return PublicChatFailure {
            code: "provider_rate_limited",
            title: "AI provider rate limited",
            detail: "The selected provider is temporarily rate limiting requests. Wait briefly, then retry this message.".to_string(),
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
            detail: "The selected model is not available to this harness or provider account. Refresh the model list, choose an available model, and retry.".to_string(),
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
            detail: "A command required approval, but this harness could not return the prompt to Temps. Choose Auto permissions for the sandbox or use a harness with an interactive approval bridge.".to_string(),
            retryable: false,
        };
    }

    if contains_any(&["timed out", "timeout", "deadline exceeded"]) {
        return PublicChatFailure {
            code: "harness_timeout",
            title: "AI harness timed out",
            detail: "The selected harness did not respond before the turn timeout. Retry, or raise the chat timeout if this task legitimately needs longer.".to_string(),
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
            detail: "Temps could not reach the selected AI provider. Check network connectivity and provider availability, then retry.".to_string(),
            retryable: true,
        };
    }

    if reason.contains("sandbox")
        && contains_any(&["unavailable", "could not", "failed", "container", "docker"])
    {
        return PublicChatFailure {
            code: "sandbox_unavailable",
            title: "Application sandbox unavailable",
            detail: "Temps could not prepare or reach the persistent application sandbox for this turn. Check its status and retry.".to_string(),
            retryable: true,
        };
    }

    if contains_any(&["exited with code", "process exited", "subprocess exited"]) {
        return PublicChatFailure {
            code: "harness_exited",
            title: "AI harness exited unexpectedly",
            detail: "The selected AI harness exited before producing a reply. Verify that it is authenticated and that the selected model is available, then retry.".to_string(),
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
            detail: "The selected harness finished without producing a reply. Retry once; if it repeats, refresh the harness status and review the server logs.".to_string(),
            retryable: true,
        };
    }

    PublicChatFailure {
        code: "harness_failed",
        title: "AI harness failed",
        detail: "The selected AI harness failed before it could reply. Retry once; if it repeats, check its authentication, selected model, and the server logs.".to_string(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::ChatError;

    #[test]
    fn public_failure_keeps_runtime_cause_without_echoing_untrusted_suffix() {
        for (cause, code) in [
            (
                "unsupported retained sandbox permission mode full-access",
                "sandbox_permission_mode_unsupported",
            ),
            (
                "another harness turn already running",
                "harness_turn_already_running",
            ),
            (
                "remote runtime carrier disconnected",
                "sandbox_runtime_disconnected",
            ),
        ] {
            let error = ChatError::Ai(format!(
                "no reply was produced: {cause}; token=private-secret /run/secrets/private"
            ));
            let failure = error.public_failure();
            assert_eq!(failure.code, code);
            assert!(failure.detail.contains(cause));
            assert!(!failure.detail.contains("private-secret"));
            assert!(!failure.detail.contains("/run/secrets"));
        }
    }

    #[test]
    fn retained_diagnostic_shows_source_error_only_for_typed_provenance() {
        let reason = "remote engine rejected workspace handshake (code E712); token=private-secret; https://user:pass@example.test/api?key=secret; /run/secrets/private; user@example.test; Bearer hidden-token";
        let trusted = ChatError::RetainedHarnessDiagnostic {
            purpose: "chat.application.tools".to_string(),
            reason: reason.to_string(),
        }
        .public_failure();
        let untrusted = ChatError::Ai(reason.to_string()).public_failure();

        assert!(trusted
            .detail
            .contains("remote engine rejected workspace handshake"));
        assert!(trusted.detail.contains("code E712"));
        for secret in [
            "private-secret",
            "user:pass",
            "example.test",
            "/run/secrets",
            "Bearer ",
            "hidden-token",
        ] {
            assert!(!trusted.detail.contains(secret));
        }
        assert!(trusted.detail.chars().count() <= 580);
        assert!(!untrusted.detail.contains("code E712"));
    }

    #[test]
    fn runtime_failures_match_actual_adapter_wording() {
        for (reason, code) in [
            (
                "unsupported retained sandbox permission mode 'full-access'",
                "sandbox_permission_mode_unsupported",
            ),
            (
                "another harness turn is already running in this application sandbox",
                "harness_turn_already_running",
            ),
        ] {
            assert_eq!(
                ChatError::Ai(format!("no reply was produced: {reason}"))
                    .public_failure()
                    .code,
                code
            );
        }
    }

    #[test]
    fn public_failure_explains_exhausted_sandbox_network_capacity_safely() {
        let failure = ChatError::Ai(
            "could not prepare the application execution sandbox: create network temps-sandbox-private-secret: all predefined address pools have been fully subnetted; token=secret"
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "sandbox_network_capacity_exhausted");
        assert_eq!(
            failure.detail,
            "This Temps host has no private sandbox network capacity available. Contact your Temps administrator, then try again."
        );
        assert!(failure.retryable);
        assert!(!failure.detail.contains("temps-sandbox-private-secret"));
        assert!(!failure.detail.contains("fully subnetted"));
        assert!(!failure.detail.contains("token=secret"));
    }

    #[test]
    fn public_failure_preserves_claude_relay_rejection_before_no_reply() {
        for model in ["sonnet[1m]", "opus[1m]"] {
            let failure = ChatError::Ai(format!(
                "no reply was produced. AI provider error for 'chat.application.model_relay': model '{model}' is not an allowed concrete Claude model for sandbox execution; token=private-secret; /private/credential.json"
            ))
            .public_failure();
            assert_eq!(failure.code, "model_unavailable");
            assert_eq!(failure.title, "Claude model rejected by workspace relay");
            assert!(failure.detail.contains("before Claude could start"));
            assert!(!failure.detail.contains("private-secret"));
            assert!(!failure.detail.contains("/private/"));
            assert!(!failure.retryable);
        }
    }

    #[test]
    fn public_failure_recognizes_non_overlapping_sandbox_pool_exhaustion() {
        let failure = ChatError::Ai(
            "could not prepare the global operator sandbox: could not find an available, non-overlapping IPv4 address pool among the defaults"
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "sandbox_network_capacity_exhausted");
        assert!(failure
            .detail
            .contains("no private sandbox network capacity"));
    }

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

    #[test]
    fn public_failure_prioritizes_unsupported_oauth_over_empty_response_wrapper() {
        let failure = ChatError::Ai(
            r#"OpenCode failed: {"error":"OAuth credentials are not supported for this provider","token":"secret"}; provider returned no response"#
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "unsupported_workspace_credential");
        assert!(!failure.retryable);
        assert!(failure.detail.contains("supported API credential"));
        assert!(!failure.detail.contains("OpenCode"));
        assert!(!failure.detail.contains("secret"));
    }

    #[test]
    fn public_failure_distinguishes_expired_auth_quota_and_rate_limits() {
        let expired = ChatError::Ai("refresh token revoked for account=user@example.com".into())
            .public_failure();
        let quota =
            ChatError::Ai("429 insufficient_quota: credits exhausted".into()).public_failure();
        let rate = ChatError::Ai("status 429: too many requests".into()).public_failure();

        assert_eq!(expired.code, "harness_authentication_required");
        assert!(!expired.retryable);
        assert!(!expired.detail.contains("user@example.com"));
        assert_eq!(quota.code, "provider_quota_exhausted");
        assert!(!quota.retryable);
        assert_eq!(rate.code, "provider_rate_limited");
        assert!(rate.retryable);
    }

    #[test]
    fn public_failure_reports_invalid_opencode_auth_json_before_empty_response() {
        let failure = ChatError::Ai(
            "no reply was produced. AI provider error for 'chat.application.credentials': the saved OpenCode auth JSON is invalid, duplicated, or contains unsupported fields. the provider returned no response"
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "invalid_workspace_credential");
        assert!(!failure.retryable);
        assert!(failure.detail.contains("one supported provider credential"));
        assert!(!failure.detail.contains("auth.json"));
    }

    #[test]
    fn public_failure_recognizes_native_opencode_oauth_schema_error() {
        let failure = ChatError::Ai(
            "no reply was produced. AI provider error for chat.application.credentials: an OpenCode OAuth credential contains unsupported or invalid fields. the provider returned no response"
                .to_string(),
        )
        .public_failure();

        assert_eq!(failure.code, "invalid_workspace_credential");
        assert!(!failure.retryable);
        assert!(!failure.detail.contains("OpenCode"));
        assert!(!failure.detail.contains("OAuth"));
    }
}
