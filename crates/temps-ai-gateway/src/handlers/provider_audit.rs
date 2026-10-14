// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audit records for AI provider key changes. Never carries key material:
//! only identifiers, the provider type, the destination host, and the
//! per-key private-address trust, which an operator reviewing SSRF exposure
//! needs to see.

use anyhow::Result as AnyhowResult;
use serde::Serialize;
use temps_auth::AuthContext;
use temps_core::{AuditOperation, RequestMetadata};
use temps_entities::ai_provider_keys;
use tracing::error;

use crate::handlers::types::AiGatewayAppState;

pub(crate) const PROVIDER_KEY_CREATED: &str = "AI_PROVIDER_KEY_CREATED";
pub(crate) const PROVIDER_KEY_UPDATED: &str = "AI_PROVIDER_KEY_UPDATED";
pub(crate) const PROVIDER_KEY_DELETED: &str = "AI_PROVIDER_KEY_DELETED";
pub(crate) const PROVIDER_MODELS_REFRESHED: &str = "AI_PROVIDER_MODELS_REFRESHED";
pub(crate) const PROVIDER_MODEL_ADDED: &str = "AI_PROVIDER_MODEL_ADDED";
pub(crate) const PROVIDER_MODEL_UPDATED: &str = "AI_PROVIDER_MODEL_UPDATED";
pub(crate) const PROVIDER_MODEL_DELETED: &str = "AI_PROVIDER_MODEL_DELETED";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderKeyAudit {
    #[serde(skip)]
    operation: &'static str,
    user_id: Option<i32>,
    #[serde(skip)]
    ip_address: Option<String>,
    #[serde(skip)]
    user_agent: String,
    provider_key_id: i32,
    provider: String,
    display_name: String,
    /// `host[:port]` of the base URL; never the path, query or user info.
    base_url_host: Option<String>,
    is_active: bool,
    /// Exact private `ip:port` this key trusts for itself, after the change.
    trusted_private_address: Option<String>,
    /// `"set"` or `"cleared"` when the per-key trust changed.
    trusted_private_address_change: Option<&'static str>,
    /// Fields the request changed (`api_key` is reported by name only).
    changed_fields: Vec<&'static str>,
}

impl AuditOperation for ProviderKeyAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> AnyhowResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize AI provider key audit: {e}"))
    }
}

/// A change to a provider key's model catalog. Enabling or disabling a model
/// decides which key -- self-hosted or vendor -- may serve it, so these
/// changes are security-relevant and audited like key changes.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderModelAudit {
    #[serde(skip)]
    operation: &'static str,
    user_id: Option<i32>,
    #[serde(skip)]
    ip_address: Option<String>,
    #[serde(skip)]
    user_agent: String,
    provider_key_id: i32,
    /// Affected model ID; `None` for a whole-catalog refresh.
    model_id: Option<String>,
    /// New enabled state, for enable/disable.
    is_enabled: Option<bool>,
    /// Number of catalog rows after a refresh.
    model_count: Option<usize>,
}

impl AuditOperation for ProviderModelAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn serialize(&self) -> AnyhowResult<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize AI provider model audit: {e}"))
    }
}

/// Record a model catalog change. Failures are logged, never surfaced.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn audit_provider_model(
    app_state: &AiGatewayAppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    operation: &'static str,
    provider_key_id: i32,
    model_id: Option<String>,
    is_enabled: Option<bool>,
    model_count: Option<usize>,
) {
    let audit = ProviderModelAudit {
        operation,
        user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        provider_key_id,
        model_id,
        is_enabled,
        model_count,
    };
    if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            operation,
            provider_key_id,
            error = %audit_error,
            "Failed to create audit log for AI provider model"
        );
    }
}

fn base_url_host(base_url: Option<&str>) -> Option<String> {
    let parsed = url::Url::parse(base_url?).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Record a provider key change. `before` is the row prior to an update
/// (`None` for create/delete). Failures are logged, never surfaced: the
/// change itself already happened.
pub(crate) async fn audit_provider_key(
    app_state: &AiGatewayAppState,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    operation: &'static str,
    key: &ai_provider_keys::Model,
    before: Option<&ai_provider_keys::Model>,
    changed_fields: Vec<&'static str>,
) {
    let previous_trust = before.and_then(|before| before.trusted_private_address.clone());
    let trusted_private_address_change = match operation {
        PROVIDER_KEY_CREATED => key.trusted_private_address.as_ref().map(|_| "set"),
        PROVIDER_KEY_UPDATED if previous_trust != key.trusted_private_address => {
            Some(if key.trusted_private_address.is_some() {
                "set"
            } else {
                "cleared"
            })
        }
        PROVIDER_KEY_UPDATED
            if changed_fields.contains(&"trusted_private_address")
                && key.trusted_private_address.is_some() =>
        {
            Some("set")
        }
        _ => None,
    };
    let audit = ProviderKeyAudit {
        operation,
        user_id: auth.user_id_opt(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
        provider_key_id: key.id,
        provider: key.provider.clone(),
        display_name: key.display_name.clone(),
        base_url_host: base_url_host(key.base_url.as_deref()),
        is_active: key.is_active,
        trusted_private_address: key.trusted_private_address.clone(),
        trusted_private_address_change,
        changed_fields,
    };
    if let Err(audit_error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            operation,
            provider_key_id = key.id,
            error = %audit_error,
            "Failed to create audit log for AI provider key"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_host_never_includes_path_query_or_credentials() {
        assert_eq!(
            base_url_host(Some("http://user:secret@10.0.0.5:8000/v1?token=x")).as_deref(),
            Some("10.0.0.5:8000")
        );
        assert_eq!(
            base_url_host(Some("https://api.openai.com/v1")).as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(base_url_host(None), None);
        assert_eq!(base_url_host(Some("not a url")), None);
    }
}
