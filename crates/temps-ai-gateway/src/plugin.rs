// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    handlers::{self, create_ai_gateway_app_state, AiGatewayAppState},
    services::{
        GatewayService, ProviderKeyService, ProviderModelService, ProviderPreferenceService,
        StructuredOutputService, UsageService,
    },
};

pub struct AiGatewayPlugin;

#[derive(serde::Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<CodexAuthTokens>,
}

#[derive(serde::Deserialize)]
struct CodexAuthTokens {
    access_token: String,
    account_id: String,
}

struct OpenCodeAuthEntries(std::collections::BTreeMap<String, serde_json::Value>);

impl<'de> serde::Deserialize<'de> for OpenCodeAuthEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AuthVisitor;
        impl<'de> serde::de::Visitor<'de> for AuthVisitor {
            type Value = OpenCodeAuthEntries;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an OpenCode provider credential object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = std::collections::BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom(
                            "duplicate OpenCode provider entry",
                        ));
                    }
                }
                Ok(OpenCodeAuthEntries(entries))
            }
        }
        deserializer.deserialize_map(AuthVisitor)
    }
}

fn parse_opencode_native_auth(
    credential: &str,
) -> Result<(Vec<u8>, Vec<String>), temps_ai::AiError> {
    let entries =
        serde_json::from_str::<OpenCodeAuthEntries>(credential)
        .map_err(|_error| temps_ai::AiError::Provider {
            purpose: "chat.application.credentials".to_string(),
            reason: "the saved OpenCode auth JSON is invalid, duplicated, or contains unsupported fields".to_string(),
        })?.0;
    if entries.is_empty() || entries.len() > 16 {
        return Err(temps_ai::AiError::Provider { purpose: "chat.application.credentials".to_string(), reason: "OpenCode sandbox execution requires between one and sixteen supported Anthropic or OpenAI auth entries".to_string() });
    }
    let mut providers = Vec::with_capacity(entries.len());
    for (provider, entry) in &entries {
        if !matches!(provider.as_str(), "anthropic" | "openai") {
            return Err(temps_ai::AiError::Provider { purpose: "chat.application.credentials".to_string(), reason: "OpenCode native sandbox authentication supports only Anthropic and OpenAI entries".to_string() });
        }
        let object = entry
            .as_object()
            .ok_or_else(|| temps_ai::AiError::Provider {
                purpose: "chat.application.credentials".to_string(),
                reason: "an OpenCode auth entry is not an object".to_string(),
            })?;
        if object.keys().any(|key| {
            key.to_ascii_lowercase().contains("url")
                || key.to_ascii_lowercase().contains("wellknown")
        }) {
            return Err(temps_ai::AiError::Provider {
                purpose: "chat.application.credentials".to_string(),
                reason:
                    "custom OpenCode authentication endpoints are not supported in sandbox mode"
                        .to_string(),
            });
        }
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("api") => {
                if object.len() != 2 || !object.contains_key("key") {
                    return Err(temps_ai::AiError::Provider {
                        purpose: "chat.application.credentials".to_string(),
                        reason: "an OpenCode API credential contains unsupported fields"
                            .to_string(),
                    });
                }
                let key = object
                    .get("key")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| temps_ai::AiError::Provider {
                        purpose: "chat.application.credentials".to_string(),
                        reason: "an OpenCode API credential is missing its key".to_string(),
                    })?;
                if key.is_empty()
                    || key != key.trim()
                    || key.len() > 16_384
                    || key.chars().any(char::is_control)
                {
                    return Err(temps_ai::AiError::Provider {
                        purpose: "chat.application.credentials".to_string(),
                        reason: "an OpenCode API credential key is empty or non-canonical"
                            .to_string(),
                    });
                }
            }
            Some("oauth") => {
                let allowed_fields = ["type", "access", "refresh", "expires", "accountId"];
                if object.len() < 4
                    || object.len() > 5
                    || object
                        .keys()
                        .any(|field| !allowed_fields.contains(&field.as_str()))
                    || !object.contains_key("expires")
                    || object
                        .get("expires")
                        .and_then(serde_json::Value::as_u64)
                        .is_none()
                {
                    return Err(temps_ai::AiError::Provider {
                        purpose: "chat.application.credentials".to_string(),
                        reason:
                            "an OpenCode OAuth credential contains unsupported or invalid fields"
                                .to_string(),
                    });
                }
                for field in ["access", "refresh"] {
                    let value = object
                        .get(field)
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| temps_ai::AiError::Provider {
                            purpose: "chat.application.credentials".to_string(),
                            reason: "an OpenCode OAuth credential is missing required token fields"
                                .to_string(),
                        })?;
                    if value.is_empty()
                        || value != value.trim()
                        || value.len() > 65_536
                        || value.chars().any(char::is_control)
                    {
                        return Err(temps_ai::AiError::Provider {
                            purpose: "chat.application.credentials".to_string(),
                            reason: "an OpenCode OAuth credential contains an invalid token field"
                                .to_string(),
                        });
                    }
                }
                if let Some(account_id) = object.get("accountId") {
                    if provider != "openai" {
                        return Err(temps_ai::AiError::Provider {
                            purpose: "chat.application.credentials".to_string(),
                            reason:
                                "an Anthropic OpenCode OAuth credential contains unsupported fields"
                                    .to_string(),
                        });
                    }
                    let account_id =
                        account_id
                            .as_str()
                            .ok_or_else(|| temps_ai::AiError::Provider {
                                purpose: "chat.application.credentials".to_string(),
                                reason:
                                    "an OpenCode OAuth credential contains an invalid account ID"
                                        .to_string(),
                            })?;
                    if account_id.is_empty()
                        || account_id != account_id.trim()
                        || account_id.len() > 4_096
                        || account_id.chars().any(char::is_control)
                    {
                        return Err(temps_ai::AiError::Provider {
                            purpose: "chat.application.credentials".to_string(),
                            reason: "an OpenCode OAuth credential contains an invalid account ID"
                                .to_string(),
                        });
                    }
                }
            }
            _ => return Err(temps_ai::AiError::Provider {
                purpose: "chat.application.credentials".to_string(),
                reason:
                    "OpenCode sandbox authentication supports only native API-key or OAuth entries"
                        .to_string(),
            }),
        }
        providers.push(provider.clone());
    }
    let canonical = serde_json::to_vec(&entries).map_err(|_| temps_ai::AiError::Provider {
        purpose: "chat.application.credentials".to_string(),
        reason: "the saved OpenCode auth JSON could not be encoded".to_string(),
    })?;
    Ok((canonical, providers))
}

fn sandbox_harness_credentials(
    provider_id: &str,
    format: temps_agents::ai_cli::catalog::CredentialFormat,
    credential: String,
    internal_api_url: String,
) -> Result<temps_ai_agent_cli::SandboxHarnessCredentials, temps_ai::AiError> {
    match (provider_id, format) {
        ("claude_cli", temps_agents::ai_cli::catalog::CredentialFormat::ApiKey) => Ok(
            temps_ai_agent_cli::SandboxHarnessCredentials::anthropic_api_key(
                credential,
                internal_api_url,
            ),
        ),
        ("claude_cli", temps_agents::ai_cli::catalog::CredentialFormat::OauthToken) => Ok(
            temps_ai_agent_cli::SandboxHarnessCredentials::claude_oauth_token(
                credential,
                internal_api_url,
            ),
        ),
        ("codex_cli", temps_agents::ai_cli::catalog::CredentialFormat::ApiKey) => Ok(
            temps_ai_agent_cli::SandboxHarnessCredentials::openai_api_key(
                credential,
                internal_api_url,
            ),
        ),
        ("codex_cli", temps_agents::ai_cli::catalog::CredentialFormat::ConfigFile) => {
            let auth = serde_json::from_str::<CodexAuthFile>(&credential).map_err(|error| {
                temps_ai::AiError::Provider {
                    purpose: "chat.application.credentials".to_string(),
                    reason: format!(
                        "the saved Codex credential is not a valid Codex auth file: {error}"
                    ),
                }
            })?;
            if let Some(api_key) = auth
                .openai_api_key
                .filter(|value| !value.trim().is_empty())
            {
                return Ok(temps_ai_agent_cli::SandboxHarnessCredentials::openai_api_key(
                    api_key,
                    internal_api_url,
                ));
            }
            let tokens = auth.tokens.ok_or_else(|| temps_ai::AiError::Provider {
                purpose: "chat.application.credentials".to_string(),
                reason: "the saved Codex auth file contains neither an API key nor ChatGPT tokens; import a current authenticated Codex login"
                    .to_string(),
            })?;
            if tokens.access_token.trim().is_empty() || tokens.account_id.trim().is_empty() {
                return Err(temps_ai::AiError::Provider {
                    purpose: "chat.application.credentials".to_string(),
                    reason: "the saved Codex auth file is missing its ChatGPT access token or account ID; import a current authenticated Codex login"
                        .to_string(),
                });
            }
            Ok(temps_ai_agent_cli::SandboxHarnessCredentials::codex_chatgpt(
                tokens.access_token,
                tokens.account_id,
                internal_api_url,
            ))
        }
        ("opencode", temps_agents::ai_cli::catalog::CredentialFormat::ConfigFile) => {
            let (contents, providers) = parse_opencode_native_auth(&credential)?;
            Ok(temps_ai_agent_cli::SandboxHarnessCredentials::opencode_auth_json(contents, providers, internal_api_url))
        }
        _ => Err(temps_ai::AiError::Provider {
            purpose: "chat.application.credentials".to_string(),
            reason: format!(
                "secure provider relay is not implemented for development harness '{provider_id}' yet"
            ),
        }),
    }
}

impl AiGatewayPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AiGatewayPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod opencode_auth_tests {
    use super::*;

    #[test]
    fn accepts_native_api_and_multi_provider_oauth_without_echoing_errors() {
        let (contents, providers) = parse_opencode_native_auth(
            r#"{"anthropic":{"type":"oauth","access":"access-a","refresh":"refresh-a","expires":999},"openai":{"type":"oauth","access":"access-b","refresh":"refresh-b","expires":999,"accountId":"account-b"}}"#,
        )
        .unwrap();
        assert_eq!(providers, ["anthropic", "openai"]);
        assert!(!contents.is_empty());
        for invalid in [
            r#"{}"#,
            r#""real-secret""#,
            r#"{"anthropic":{"type":"oauth","access":"real-secret"}}"#,
            r#"{"custom":{"type":"api","key":"real-secret"}}"#,
            r#"{"anthropic":{"type":"api","key":"real-secret","endpoint":"https://invalid.test"}}"#,
            r#"{"anthropic":{"type":"oauth","access":"real-secret","refresh":"refresh","expires":"tomorrow"}}"#,
            r#"{"anthropic":{"type":"oauth","access":"real-secret","refresh":"refresh","expires":999,"extra":true}}"#,
            r#"{"openai":{"type":"oauth","access":"real-secret","refresh":"refresh","expires":999,"accountId":" account"}}"#,
            r#"{"openai":{"type":"oauth","access":"real-secret","refresh":"refresh","expires":999,"accountId":7}}"#,
            r#"{"anthropic":{"type":"oauth","access":"real-secret","refresh":"refresh","expires":999,"accountId":"account"}}"#,
            r#"{"anthropic":{"type":"api","key":"a"},"anthropic":{"type":"api","key":"real-secret"}}"#,
            "{\"anthropic\":{\"type\":\"api\",\"key\":\" real-secret\"}}",
            "{\"anthropic\":{\"type\":\"api\",\"key\":\"real-secret \"}}",
            "{\"anthropic\":{\"type\":\"api\",\"key\":\"   \"}}",
            "{\"anthropic\":{\"type\":\"api\",\"key\":\"real-secret\\nnext\"}}",
            "{\"anthropic\":{\"type\":\"api\",\"key\":\"real-secret\\rnext\"}}",
            "{\"anthropic\":{\"type\":\"api\",\"key\":\"real-secret\\u0000next\"}}",
        ] {
            let error = parse_opencode_native_auth(invalid).unwrap_err().to_string();
            assert!(!error.contains("real-secret"));
        }
    }
}

impl TempsPlugin for AiGatewayPlugin {
    fn name(&self) -> &'static str {
        "ai_gateway"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            let provider_key_service = Arc::new(ProviderKeyService::new(
                db.clone(),
                encryption_service.clone(),
            ));
            context.register_service(provider_key_service.clone());

            let provider_preference_service = Arc::new(ProviderPreferenceService::new(db.clone()));
            context.register_service(provider_preference_service.clone());

            let gateway_service = Arc::new(
                GatewayService::new(provider_key_service.clone())
                    .with_cloud_link(context.get_service::<temps_cloud_client::CloudLink>()),
            );
            context.register_service(gateway_service.clone());
            let provider_model_service = Arc::new(ProviderModelService::new(
                db.clone(),
                provider_key_service.clone(),
                gateway_service.clone(),
            ));
            context.register_service(provider_model_service.clone());

            // Upgrade-safe catalog bootstrap: keys created before the model
            // inventory migration would otherwise advertise zero models until
            // an administrator manually pressed Refresh. Static adapter models
            // are inserted synchronously and live discovery replaces their
            // availability metadata in the background without delaying boot.
            match provider_key_service.list_active().await {
                Ok(keys) => {
                    for key in keys {
                        if let Err(error) = provider_model_service.seed_bootstrap(&key).await {
                            tracing::warn!(
                                provider_key_id = key.id,
                                %error,
                                "Could not seed the upgraded AI model catalog"
                            );
                        }
                        let models = provider_model_service.clone();
                        tokio::spawn(async move {
                            if let Err(error) = models.refresh(key.id).await {
                                tracing::warn!(
                                    provider_key_id = key.id,
                                    %error,
                                    "Background AI model catalog refresh failed; bootstrap models remain available"
                                );
                            }
                        });
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    "Could not enumerate provider keys for model catalog bootstrap"
                ),
            }

            // ADR-022: register the general AI foundation so any feature can get
            // text or typed/structured output from the configured model through
            // one governed seam. Best-effort + self-gating, safe to always register.
            //
            // The provider registry routes every normalized AI capability to
            // the selected adapter. Gateway keys use native function calling;
            // host harnesses receive the same scoped tools through the
            // turn-local MCP bridge. Chat and feature code never branches on
            // provider transport.
            let gateway_ai_service = Arc::new(crate::services::GatewayAiService::new(
                gateway_service.clone(),
                db.clone(),
            ));

            // One AgentCliAiService per catalog entry (Claude Code, Codex,
            // OpenCode), built once here rather than per-request — each is
            // just a thin handle around a zero-arg `AiCliProvider` plus a
            // shared scratch dir/semaphore, so building the whole catalog
            // upfront is cheap and keeps DispatchingAiService's routing a
            // synchronous HashMap lookup.
            let config_service = context.require_service::<temps_config::ConfigService>();
            let ai_cli_scratch_dir = config_service.get_data_subdir("ai-cli-scratch");
            let application_workspace_root = config_service.get_data_subdir("ai-applications");
            let sandbox_provider =
                context.get_service::<dyn temps_agents::sandbox::SandboxProvider>();
            let sandbox_model_relay = Arc::new(
                temps_ai_agent_cli::SandboxModelRelayService::new().map_err(|error| {
                    PluginError::InitializationFailed(format!(
                        "failed to initialize sandbox model relay: {error}"
                    ))
                })?,
            );
            context.register_service(sandbox_model_relay.clone());
            let sandbox_workspace_resolver =
                Arc::new(temps_ai_agent_cli::SandboxWorkspaceResolverSlot::new());
            context.register_service(sandbox_workspace_resolver.clone());
            let sandbox_credentials: temps_ai_agent_cli::SandboxCredentialResolver = {
                let config_service = config_service.clone();
                let encryption_service = encryption_service.clone();
                Arc::new(move |provider_id| {
                    let config_service = config_service.clone();
                    let encryption_service = encryption_service.clone();
                    let provider_id = provider_id.to_string();
                    Box::pin(async move {
                        let settings = config_service.get_settings().await.map_err(|error| {
                            temps_ai::AiError::Provider {
                                purpose: "chat.application.credentials".to_string(),
                                reason: format!(
                                    "could not load the Agent Sandbox credential configuration: {error}"
                                ),
                            }
                        })?;
                        let provider_config = settings.agent_sandbox.provider_config(&provider_id);
                        let encrypted = provider_config
                            .credentials_encrypted
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| temps_ai::AiError::Provider {
                                purpose: "chat.application.credentials".to_string(),
                                reason: format!(
                                    "{} is not authenticated in Agent Sandbox settings",
                                    provider_id
                                ),
                            })?;
                        let credential =
                            encryption_service
                                .decrypt_string(&encrypted)
                                .map_err(|error| temps_ai::AiError::Provider {
                                    purpose: "chat.application.credentials".to_string(),
                                    reason: format!(
                                    "could not unlock the Agent Sandbox credential for {}: {error}",
                                    provider_id
                                ),
                                })?;
                        let provider = temps_agents::ai_cli::find_provider(&provider_id)
                            .ok_or_else(|| temps_ai::AiError::Provider {
                                purpose: "chat.application.credentials".to_string(),
                                reason: format!("unknown development harness '{}'", provider_id),
                            })?;
                        let auth_type = if provider_config.auth_type.is_empty() {
                            provider.default_flavor().id
                        } else {
                            &provider_config.auth_type
                        };
                        let flavor = provider.flavor(auth_type).ok_or_else(|| {
                            temps_ai::AiError::Provider {
                                purpose: "chat.application.credentials".to_string(),
                                reason: format!(
                                    "Agent Sandbox authentication type '{}' is not valid for {}",
                                    auth_type, provider_id
                                ),
                            }
                        })?;
                        let internal_api_url = config_service.resolve_internal_url().await;
                        let credentials = sandbox_harness_credentials(
                            &provider_id,
                            flavor.format,
                            credential,
                            internal_api_url,
                        )?;
                        Ok(credentials)
                    })
                })
            };
            if let Err(e) = tokio::fs::create_dir_all(&ai_cli_scratch_dir).await {
                tracing::error!(
                    scratch_dir = %ai_cli_scratch_dir.display(),
                    error = %e,
                    "Failed to create AI CLI scratch directory — agent-CLI-backed AI requests will fail until this is fixed"
                );
            }
            // ADR-037 §5 recommended defaults: 30s per-call timeout, 2
            // concurrent CLI subprocesses on the host.
            const AI_CLI_TIMEOUT: Duration = Duration::from_secs(30);
            const AI_CLI_CONCURRENCY: usize = 2;

            let mut agent_cli_services: HashMap<String, Arc<dyn temps_ai::AiService>> =
                HashMap::new();
            for registration in temps_agents::ai_cli::PROVIDER_CATALOG {
                if let Some(provider) = temps_agents::ai_cli::create_provider(registration.id) {
                    let provider: Arc<dyn temps_agents::ai_cli::AiCliProvider> = provider.into();
                    let mut svc = temps_ai_agent_cli::AgentCliAiService::new(
                        provider,
                        ai_cli_scratch_dir.clone(),
                        AI_CLI_TIMEOUT,
                        AI_CLI_CONCURRENCY,
                    );
                    if let Some(sandbox_provider) = &sandbox_provider {
                        svc = svc.with_temps_sandbox(
                            sandbox_provider.clone(),
                            application_workspace_root.clone(),
                            sandbox_credentials.clone(),
                            sandbox_model_relay.clone(),
                            sandbox_workspace_resolver.as_ref().clone(),
                        );
                    } else {
                        tracing::warn!(
                            "Temps sandbox provider is unavailable; application harness turns will fail closed"
                        );
                    }
                    let svc = Arc::new(svc);
                    agent_cli_services.insert(
                        registration.id.to_string(),
                        svc as Arc<dyn temps_ai::AiService>,
                    );
                }
            }

            let preference_reader: Arc<dyn temps_ai_agent_cli::ActiveProviderReader> =
                provider_preference_service.clone();
            let ai_service = Arc::new(temps_ai_agent_cli::AiProviderRegistry::with_providers(
                gateway_ai_service.clone() as Arc<dyn temps_ai::AiService>,
                preference_reader,
                agent_cli_services,
            ));
            let ai_service: Arc<dyn temps_ai::AiService> = ai_service;
            context.register_service(ai_service.clone());

            // Browser-supplied generic prompts are intentionally restricted to
            // tool-less gateway APIs. Host subscription CLIs can inspect the
            // host and are reserved for server-authored, purpose-specific jobs.
            let structured_output_service = Arc::new(StructuredOutputService::new(
                gateway_ai_service as Arc<dyn temps_ai::AiService>,
            ));
            context.register_service(structured_output_service.clone());

            let usage_service = Arc::new(UsageService::new(db.clone()));
            context.register_service(usage_service.clone());

            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            let project_access_checker =
                context.get_service::<dyn temps_core::ProjectAccessChecker>();

            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                });

            let app_state = create_ai_gateway_app_state(
                db,
                gateway_service,
                provider_key_service,
                provider_model_service,
                provider_preference_service,
                usage_service,
                audit_service,
                telemetry,
                structured_output_service,
                project_access_checker,
            )
            .await;
            context.register_service(app_state);

            tracing::debug!("AI Gateway plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let app_state = context.require_service::<AiGatewayAppState>();

        // All AI Gateway routes live on the authenticated surface. The
        // OpenAI-compatible gateway endpoints use `RequireAuth`, which depends
        // on the `AuthContext` injected by `auth_middleware` — that middleware
        // only runs on this router, not the public one.
        let routes = handlers::configure_admin_routes()
            .merge(handlers::configure_usage_routes())
            .merge(handlers::configure_pricing_routes())
            .merge(handlers::configure_gateway_routes())
            .merge(handlers::configure_provider_status_routes())
            .merge(handlers::configure_structured_output_routes())
            .with_state(app_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut schema = <handlers::gateway::AiGatewayApiDoc as OpenApiTrait>::openapi();
        let admin_schema = <handlers::providers::AiGatewayAdminApiDoc as OpenApiTrait>::openapi();
        schema.merge(admin_schema);
        let usage_schema = <handlers::usage::AiGatewayUsageApiDoc as OpenApiTrait>::openapi();
        schema.merge(usage_schema);
        let pricing_schema = <handlers::pricing::AiGatewayPricingApiDoc as OpenApiTrait>::openapi();
        schema.merge(pricing_schema);
        let provider_status_schema =
            <handlers::provider_status::AiProviderStatusApiDoc as OpenApiTrait>::openapi();
        schema.merge(provider_status_schema);
        let structured_output_schema =
            <handlers::structured_output::StructuredOutputApiDoc as OpenApiTrait>::openapi();
        schema.merge(structured_output_schema);
        Some(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ai_gateway_plugin_name() {
        let plugin = AiGatewayPlugin::new();
        assert_eq!(plugin.name(), "ai_gateway");
    }

    #[tokio::test]
    async fn test_ai_gateway_plugin_default() {
        let plugin = AiGatewayPlugin;
        assert_eq!(plugin.name(), "ai_gateway");
    }

    #[test]
    fn codex_subscription_auth_file_resolves_for_the_host_relay() {
        let credential = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "access-token",
                "account_id": "account-id",
                "refresh_token": "refresh-token"
            }
        })
        .to_string();

        assert!(sandbox_harness_credentials(
            "codex_cli",
            temps_agents::ai_cli::catalog::CredentialFormat::ConfigFile,
            credential,
            "http://temps.internal".to_string(),
        )
        .is_ok());
    }

    #[test]
    fn codex_subscription_auth_file_rejects_missing_account_context() {
        let credential = serde_json::json!({
            "tokens": {"access_token": "access-token", "account_id": ""}
        })
        .to_string();

        assert!(sandbox_harness_credentials(
            "codex_cli",
            temps_agents::ai_cli::catalog::CredentialFormat::ConfigFile,
            credential,
            "http://temps.internal".to_string(),
        )
        .is_err());
    }

    #[test]
    fn codex_api_key_auth_file_uses_the_api_key_path() {
        let credential = serde_json::json!({
            "OPENAI_API_KEY": "sk-test-key",
            "tokens": null
        })
        .to_string();

        assert!(sandbox_harness_credentials(
            "codex_cli",
            temps_agents::ai_cli::catalog::CredentialFormat::ConfigFile,
            credential,
            "http://temps.internal".to_string(),
        )
        .is_ok());
    }
}
