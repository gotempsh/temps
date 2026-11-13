// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Marker for requests originating from the AI agent's `call_api` tool.
//!
//! The agent does not call the HTTP API over the network — `InternalApiCaller`
//! replays a synthetic request through the real Axum router with the caller's
//! `AuthContext` injected, so `permission_guard!` and project scoping stay the
//! single enforcement point. That is exactly what we want for authorization,
//! but it also makes agent traffic indistinguishable from console traffic.
//!
//! Some endpoints need to treat the two differently. Reading table rows is the
//! motivating case: a human who already has `ExternalServicesRead` may browse
//! their own data freely, but the *agent* doing the same thing ships those rows
//! to a third-party LLM provider, which the operator must opt into per service.
//!
//! The caller inserts [`AiToolCall`] into the request extensions; handlers that
//! care extract it with `Option<Extension<AiToolCall>>`. Absence means a normal
//! console/CLI request — the marker can only be set in-process by the tool
//! caller, never by a remote client, because extensions are not derived from
//! anything on the wire.

/// Present in request extensions when the request was issued by the AI agent's
/// API tool rather than by a human client.
///
/// ```ignore
/// async fn handler(
///     ai_call: Option<Extension<AiToolCall>>,
///     // …
/// ) -> Result<impl IntoResponse, Problem> {
///     if ai_call.is_some() {
///         // apply the agent-specific gate
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AiToolCall;

/// Refuse a request the AI assistant issued itself, for an operation whose
/// whole protection is that a *human* performs it.
///
/// Approving or rejecting a proposed action, and choosing how much the
/// assistant may do without asking, are the controls that bound the assistant.
/// If the assistant could call them it would be approving its own proposals.
/// Authorization (`permission_guard!`) cannot catch this, because the replayed
/// request carries the human's own `AuthContext`; the marker is the only thing
/// that tells the two apart.
///
/// Call this from the handler, next to `permission_guard!`, rather than relying
/// on the tool allowlist: the allowlist is a convenience for discovery, while a
/// handler check holds however the operation is reached.
pub fn require_human_caller(
    ai_call: Option<&AiToolCall>,
    operation: &str,
) -> Result<(), crate::problemdetails::Problem> {
    if ai_call.is_none() {
        return Ok(());
    }
    Err(crate::error_builder::forbidden()
        .title("Human Confirmation Required")
        .detail(format!(
            "'{operation}' must be performed by a person in the console; the AI assistant cannot do it on their behalf"
        ))
        .value("error_code", "HUMAN_CALLER_REQUIRED")
        .value("operation", operation)
        .build())
}

/// Where an agent-issued request came from: the human client behind it.
///
/// The replayed request has no socket and no `User-Agent` of its own, so
/// without this every audit row written by an assistant action would record
/// `unknown` for both. The caller that holds the real approver's metadata
/// inserts it next to [`AiToolCall`]; [`crate::request_metadata`] folds it into
/// the [`crate::RequestMetadata`] the handlers hand to the audit log, together
/// with a visible "via Temps AI assistant" label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiToolOrigin {
    pub ip_address: String,
    pub user_agent: String,
}

/// Suffix appended to the recorded `user_agent` of every request issued by the
/// AI assistant, so an audit entry reads as "done from the assistant".
pub const AI_ASSISTANT_USER_AGENT_LABEL: &str = "via Temps AI assistant";

/// The `user_agent` an audit entry records for an assistant-issued request.
pub fn assistant_user_agent(origin: Option<&AiToolOrigin>) -> String {
    match origin {
        Some(origin) => format!("{} ({})", origin.user_agent, AI_ASSISTANT_USER_AGENT_LABEL),
        None => format!("Temps AI assistant ({AI_ASSISTANT_USER_AGENT_LABEL})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_tool_call_round_trips_through_http_extensions() {
        // The gate is only sound if the marker survives the extension
        // insert/extract cycle the caller and handler rely on.
        let mut extensions = axum::http::Extensions::new();
        assert!(extensions.get::<AiToolCall>().is_none());

        extensions.insert(AiToolCall);
        assert_eq!(extensions.get::<AiToolCall>(), Some(&AiToolCall));
    }

    #[test]
    fn human_only_operations_refuse_the_assistant_and_admit_people() {
        assert!(require_human_caller(None, "confirm_pending_action").is_ok());

        let problem = require_human_caller(Some(&AiToolCall), "confirm_pending_action")
            .expect_err("the assistant must not pass a human-only gate");
        assert_eq!(problem.status_code, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(
            problem.body.get("error_code").and_then(|v| v.as_str()),
            Some("HUMAN_CALLER_REQUIRED")
        );
        assert_eq!(
            problem.body.get("operation").and_then(|v| v.as_str()),
            Some("confirm_pending_action"),
            "the refusal names the operation"
        );
    }

    #[test]
    fn assistant_user_agent_keeps_the_real_client_and_names_the_assistant() {
        let origin = AiToolOrigin {
            ip_address: "203.0.113.7".to_string(),
            user_agent: "Mozilla/5.0".to_string(),
        };
        assert_eq!(
            assistant_user_agent(Some(&origin)),
            "Mozilla/5.0 (via Temps AI assistant)"
        );
        assert!(assistant_user_agent(None).contains("via Temps AI assistant"));
    }
}

/// In-process evidence that a pending write was confirmed by its human owner.
/// Never populated from HTTP headers or model parameters.
#[derive(Clone, Copy, Debug)]
pub struct ConfirmedAiWrite;

/// Assistant writes that widen what a credential or a sandbox can reach: a
/// vault session, link or bundle, and an internal access grant from another
/// project or a sandbox (ADR-101, ADR-105 D8).
///
/// Only the owner's confirmation of that exact pending action in the console
/// may run one. A permission mode that approves writes on its own, an MCP
/// `confirm_action`, or anything else the model can produce never stands in
/// for it: a prompt-injected agent could otherwise widen its own reach, such
/// as a sandbox agent granting its own sandbox access to a project. Each
/// handler also checks [`require_human_confirmation`], so the rule holds
/// however the operation is reached.
pub const HUMAN_CONSENT_OPERATIONS: &[&str] = &[
    "create_access_session_general",
    "create_vault_link",
    "create_access_bundle_general",
    "create_internal_access_grant",
    "create_internal_access_sandbox_grant",
];

/// Whether `operation_id` is one of [`HUMAN_CONSENT_OPERATIONS`].
pub fn requires_human_consent(operation_id: &str) -> bool {
    HUMAN_CONSENT_OPERATIONS.contains(&operation_id)
}

pub fn require_human_confirmation(
    ai_call: Option<&AiToolCall>,
    confirmation: Option<&ConfirmedAiWrite>,
    operation: &str,
) -> Result<(), crate::problemdetails::Problem> {
    if confirmation.is_some() {
        return Ok(());
    }
    require_human_caller(ai_call, operation)
}

#[cfg(test)]
mod confirmation_tests {
    use super::*;
    #[test]
    fn assistant_access_requires_confirmed_pending_write() {
        assert!(require_human_confirmation(None, None, "create_access_session_general").is_ok());
        assert!(require_human_confirmation(
            Some(&AiToolCall),
            None,
            "create_access_session_general"
        )
        .is_err());
        assert!(require_human_confirmation(
            Some(&AiToolCall),
            Some(&ConfirmedAiWrite),
            "create_access_session_general"
        )
        .is_ok());
        assert!(require_human_caller(Some(&AiToolCall), "confirm_pending_action").is_err());
    }

    #[test]
    fn granting_internal_access_needs_human_consent_and_revoking_does_not() {
        for operation in [
            "create_internal_access_grant",
            "create_internal_access_sandbox_grant",
            "create_vault_link",
        ] {
            assert!(requires_human_consent(operation), "{operation}");
        }
        for operation in [
            "delete_internal_access_grant",
            "delete_internal_access_sandbox_grant",
            "list_internal_access_sandbox_grants",
        ] {
            assert!(!requires_human_consent(operation), "{operation}");
        }
    }
}
