// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Provider-neutral runtime registry. It selects an adapter, then delegates the
//! complete normalized [`AiService`] contract without owning chat, tool,
//! persistence, permission, or transport behavior.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use temps_ai::{
    AiError, AiRequest, AiResponse, AiService, ChatTurnRequest, ChatTurnResponse, ChatTurnStream,
    ProviderCapabilities, RefreshPolicy, TokenStream, ToolExecutor, TurnServices,
};

/// Read seam for the instance-level provider preference (`ai_gateway_config`).
/// Defined here rather than depended on from `temps-ai-gateway` so this crate
/// stays free of a DB dependency — `temps-ai-gateway` implements this trait
/// for its `ProviderPreferenceService` and hands `DispatchingAiService` an
/// `Arc<dyn ActiveProviderReader>` at construction time.
#[async_trait]
pub trait ActiveProviderReader: Send + Sync {
    /// Returns the catalog id (e.g. `"claude_cli"`) of the active agent-CLI
    /// provider when the instance-scoped preference is `"agent_cli"`, or
    /// `None` when it's `"gateway"`, unset, or the row can't be read.
    async fn active_agent_cli_provider(&self) -> Option<String>;

    /// Instance-wide defaults for server-authored summary workloads. The
    /// provider id uses the normalized registry route (`gateway_key:{id}` or
    /// an agent CLI catalog id). Missing fields inherit the ordinary active
    /// provider and that provider's defaults.
    async fn summary_preference(&self) -> Result<AiSummaryPreference, AiError> {
        Ok(AiSummaryPreference::default())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSummaryPreference {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
}

/// A no-op reader that never selects an agent-CLI provider — used by
/// [`AiProviderRegistry::new`] so callers without provider selection
/// keep the original pure-passthrough-to-gateway behaviour unchanged.
struct NeverAgentCli;

#[async_trait]
impl ActiveProviderReader for NeverAgentCli {
    async fn active_agent_cli_provider(&self) -> Option<String> {
        None
    }
}

/// Routes each [`AiService`] call to whichever provider is currently active,
/// so a BYOK provider key and a subscription-backed agent CLI are usable the
/// same way — whichever the operator picked in the AI Provider Preference
/// card drives every AI feature that reads through this seam, not just AI
/// Workflows.
///
/// Multi-turn function calling follows the pinned/active provider too. Agent
/// CLI services receive a scoped executor and register it through their native
/// MCP configuration; gateway services retain their native tool-call loop.
pub struct AiProviderRegistry {
    gateway: Arc<dyn AiService>,
    preference: Arc<dyn ActiveProviderReader>,
    /// Agent-CLI-backed `AiService` per catalog id, built once at startup.
    agent_cli_services: HashMap<String, Arc<dyn AiService>>,
}

impl AiProviderRegistry {
    /// Phase 1 constructor: pure passthrough to `gateway`. Kept for callers
    /// (and tests) that don't need CLI routing.
    pub fn new(gateway: Arc<dyn AiService>) -> Self {
        Self {
            gateway,
            preference: Arc::new(NeverAgentCli),
            agent_cli_services: HashMap::new(),
        }
    }

    /// Phase 2/3 constructor: routes to `agent_cli_services[id]` when
    /// `preference` reports an active agent-CLI provider whose id is present
    /// in the map, else falls back to `gateway`.
    pub fn with_providers(
        gateway: Arc<dyn AiService>,
        preference: Arc<dyn ActiveProviderReader>,
        agent_cli_services: HashMap<String, Arc<dyn AiService>>,
    ) -> Self {
        Self {
            gateway,
            preference,
            agent_cli_services,
        }
    }

    /// Compatibility constructor for existing composition roots. New code
    /// should use [`Self::with_providers`]; the registry itself is not tied to
    /// CLI transports.
    pub fn with_agent_cli(
        gateway: Arc<dyn AiService>,
        preference: Arc<dyn ActiveProviderReader>,
        providers: HashMap<String, Arc<dyn AiService>>,
    ) -> Self {
        Self::with_providers(gateway, preference, providers)
    }

    /// The service to use for a workload an agent CLI can actually serve.
    /// An explicit conversation pin fails closed when its service is missing;
    /// only the instance-level preference may fall back to the gateway.
    async fn routed(&self, requested: Option<&str>) -> Option<Arc<dyn AiService>> {
        if let Some(id) = requested {
            if id == "gateway" || id.starts_with("gateway_key:") {
                return Some(self.gateway.clone());
            }
            return self.agent_cli_services.get(id).cloned();
        }
        if let Some(id) = self.preference.active_agent_cli_provider().await {
            if let Some(svc) = self.agent_cli_services.get(&id) {
                return Some(svc.clone());
            }
        }
        Some(self.gateway.clone())
    }

    async fn apply_summary_defaults(&self, request: &mut AiRequest) -> Result<(), AiError> {
        if !request.purpose.ends_with(".summary") {
            return Ok(());
        }
        let preference = self.preference.summary_preference().await?;
        if request.provider.is_none() {
            request.provider = preference.provider;
        }
        if request.model.is_none() {
            request.model = preference.model;
        }
        if request.thinking_level.is_none() {
            request.thinking_level = preference.thinking_level;
        }
        Ok(())
    }
}

#[async_trait]
impl AiService for AiProviderRegistry {
    async fn is_available(&self) -> bool {
        match self.routed(None).await {
            Some(service) => service.is_available().await,
            None => false,
        }
    }

    async fn is_available_for(&self, provider: Option<&str>) -> bool {
        match self.routed(provider).await {
            Some(service) => service.is_available_for(provider).await,
            None => false,
        }
    }

    async fn chat_capable(&self) -> bool {
        match self.routed(None).await {
            Some(service) => service.chat_capable().await,
            None => false,
        }
    }

    async fn chat_capable_for(&self, provider: Option<&str>) -> bool {
        match self.routed(provider).await {
            Some(service) => service.chat_capable_for(provider).await,
            None => false,
        }
    }

    async fn capabilities_for(
        &self,
        provider: Option<&str>,
        refresh: RefreshPolicy,
    ) -> Result<ProviderCapabilities, AiError> {
        let service = self
            .routed(provider)
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: "provider.capabilities".to_string(),
                reason: format!(
                    "pinned provider '{}' is unavailable",
                    provider.unwrap_or("unknown")
                ),
            })?;
        service.capabilities_for(provider, refresh).await
    }

    async fn complete(&self, mut request: AiRequest) -> Result<AiResponse, AiError> {
        self.apply_summary_defaults(&mut request).await?;
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!(
                    "pinned provider '{}' is unavailable",
                    request.provider.as_deref().unwrap_or("unknown")
                ),
            })?;
        service.complete(request).await
    }

    async fn complete_stream(&self, mut request: AiRequest) -> Result<TokenStream, AiError> {
        self.apply_summary_defaults(&mut request).await?;
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!(
                    "pinned provider '{}' is unavailable",
                    request.provider.as_deref().unwrap_or("unknown")
                ),
            })?;
        service.complete_stream(request).await
    }

    async fn chat_stream(&self, request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: "chat.stream".to_string(),
                reason: format!(
                    "pinned provider '{}' is unavailable",
                    request.provider.as_deref().unwrap_or("unknown")
                ),
            })?;
        service.chat_stream(request).await
    }

    async fn chat(&self, request: ChatTurnRequest) -> Result<ChatTurnResponse, AiError> {
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "pinned chat provider is unavailable".to_string(),
            })?;
        service.chat(request).await
    }

    async fn chat_stream_turn(&self, request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "pinned chat provider is unavailable".to_string(),
            })?;
        service.chat_stream_turn(request).await
    }

    async fn chat_stream_turn_with_executor(
        &self,
        request: ChatTurnRequest,
        executor: Option<ToolExecutor>,
    ) -> Result<ChatTurnStream, AiError> {
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "pinned chat provider is unavailable".to_string(),
            })?;
        service
            .chat_stream_turn_with_executor(request, executor)
            .await
    }

    async fn chat_stream_turn_with_services(
        &self,
        request: ChatTurnRequest,
        services: TurnServices,
    ) -> Result<ChatTurnStream, AiError> {
        let service = self
            .routed(request.provider.as_deref())
            .await
            .ok_or_else(|| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "pinned chat provider is unavailable".to_string(),
            })?;
        service
            .chat_stream_turn_with_services(request, services)
            .await
    }
}

/// Backwards-compatible name retained while downstream plugins migrate to the
/// provider-neutral registry terminology.
pub type DispatchingAiService = AiProviderRegistry;

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use temps_ai::{AiError, AiRequest, AiResponse, ChatTurnRequest, TokenStream};

    struct AlwaysOkService {
        tag: &'static str,
    }

    #[async_trait]
    impl AiService for AlwaysOkService {
        async fn is_available(&self) -> bool {
            true
        }
        async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
            Ok(AiResponse {
                text: format!("{}:{}", self.tag, request.purpose),
                json: None,
                model: "test-model".into(),
            })
        }
        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            let s = futures::stream::once(async { Ok::<String, AiError>("chunk".into()) });
            Ok(Box::pin(s))
        }

        async fn chat_stream_turn_with_executor(
            &self,
            request: ChatTurnRequest,
            executor: Option<ToolExecutor>,
        ) -> Result<ChatTurnStream, AiError> {
            let event = temps_ai::ChatStreamDelta::Text(format!(
                "{}:{}:{}",
                self.tag,
                request.purpose,
                executor.is_some()
            ));
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
        }

        async fn chat_stream_turn_with_services(
            &self,
            request: ChatTurnRequest,
            services: TurnServices,
        ) -> Result<ChatTurnStream, AiError> {
            let event = temps_ai::ChatStreamDelta::Text(format!(
                "{}:{}:{}:{}",
                self.tag,
                request.purpose,
                services.tools.is_some(),
                services.interactions.is_some()
            ));
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
        }
    }

    #[tokio::test]
    async fn test_dispatching_delegates_complete() {
        let gateway: Arc<dyn AiService> = Arc::new(AlwaysOkService { tag: "gateway" });
        let dispatching = DispatchingAiService::new(gateway);

        let result = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                prompt: "hello".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.text, "gateway:test");
        assert_eq!(result.model, "test-model");
    }

    #[tokio::test]
    async fn test_dispatching_is_available_delegates() {
        let gateway: Arc<dyn AiService> = Arc::new(AlwaysOkService { tag: "gw" });
        let dispatching = DispatchingAiService::new(gateway);
        assert!(dispatching.is_available().await);
    }

    #[tokio::test]
    async fn test_dispatching_chat_stream_delegates() {
        use futures::StreamExt;

        let gateway: Arc<dyn AiService> = Arc::new(AlwaysOkService { tag: "gw" });
        let dispatching = DispatchingAiService::new(gateway);

        let mut stream = dispatching
            .chat_stream(ChatTurnRequest::default())
            .await
            .unwrap();

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk, "chunk");
    }

    // -----------------------------------------------------------------------
    // Phase 2/3 routing tests
    // -----------------------------------------------------------------------

    struct FixedPreference(Option<&'static str>);

    #[async_trait]
    impl ActiveProviderReader for FixedPreference {
        async fn active_agent_cli_provider(&self) -> Option<String> {
            self.0.map(str::to_string)
        }
    }

    struct SummaryPreferenceReader;

    #[async_trait]
    impl ActiveProviderReader for SummaryPreferenceReader {
        async fn active_agent_cli_provider(&self) -> Option<String> {
            None
        }

        async fn summary_preference(&self) -> Result<AiSummaryPreference, AiError> {
            Ok(AiSummaryPreference {
                provider: Some("codex_cli".to_string()),
                model: Some("gpt-5.6-codex".to_string()),
                thinking_level: Some("high".to_string()),
            })
        }
    }

    fn tagged_service(tag: &'static str) -> Arc<dyn AiService> {
        Arc::new(AlwaysOkService { tag })
    }

    #[tokio::test]
    async fn test_with_agent_cli_routes_complete_to_active_cli_provider() {
        let gateway = tagged_service("gateway");
        let mut agent_cli_services = HashMap::new();
        agent_cli_services.insert("claude_cli".to_string(), tagged_service("claude_cli"));

        let dispatching = DispatchingAiService::with_agent_cli(
            gateway,
            Arc::new(FixedPreference(Some("claude_cli"))),
            agent_cli_services,
        );

        let result = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.text, "claude_cli:test");
    }

    #[tokio::test]
    async fn summary_defaults_route_and_configure_every_summary_request() {
        struct EchoOptions;
        #[async_trait]
        impl AiService for EchoOptions {
            async fn is_available(&self) -> bool {
                true
            }
            async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
                Ok(AiResponse {
                    text: format!(
                        "{}:{}",
                        request.model.as_deref().unwrap_or("default"),
                        request.thinking_level.as_deref().unwrap_or("default")
                    ),
                    json: None,
                    model: request.model.unwrap_or_default(),
                })
            }
            async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
                Err(AiError::NotAvailable)
            }
        }

        let mut providers = HashMap::new();
        providers.insert(
            "codex_cli".to_string(),
            Arc::new(EchoOptions) as Arc<dyn AiService>,
        );
        let registry = AiProviderRegistry::with_providers(
            tagged_service("gateway"),
            Arc::new(SummaryPreferenceReader),
            providers,
        );

        let response = registry
            .complete(AiRequest {
                purpose: "api_traffic.summary".to_string(),
                ..Default::default()
            })
            .await
            .expect("summary profile should route through Codex");
        assert_eq!(response.text, "gpt-5.6-codex:high");
    }

    #[tokio::test]
    async fn explicit_summary_options_override_instance_defaults() {
        let mut providers = HashMap::new();
        providers.insert("codex_cli".to_string(), tagged_service("codex_cli"));
        let registry = AiProviderRegistry::with_providers(
            tagged_service("gateway"),
            Arc::new(SummaryPreferenceReader),
            providers,
        );
        let response = registry
            .complete(AiRequest {
                purpose: "alert.summary".to_string(),
                provider: Some("gateway".to_string()),
                model: Some("explicit-model".to_string()),
                thinking_level: Some("low".to_string()),
                ..Default::default()
            })
            .await
            .expect("explicit route remains authoritative");
        assert_eq!(response.text, "gateway:alert.summary");
    }

    #[tokio::test]
    async fn test_explicit_provider_pin_overrides_changed_instance_preference() {
        let gateway = tagged_service("gateway");
        let mut agent_cli_services = HashMap::new();
        agent_cli_services.insert("claude_cli".to_string(), tagged_service("claude_cli"));
        agent_cli_services.insert("codex_cli".to_string(), tagged_service("codex_cli"));

        let dispatching = DispatchingAiService::with_agent_cli(
            gateway,
            Arc::new(FixedPreference(Some("claude_cli"))),
            agent_cli_services,
        );

        let result = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                provider: Some("codex_cli".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.text, "codex_cli:test");
    }

    #[tokio::test]
    async fn adding_a_provider_requires_no_registry_or_chat_code_change() {
        let mut providers = HashMap::new();
        providers.insert("future_provider".to_string(), tagged_service("future"));
        let registry = AiProviderRegistry::with_providers(
            tagged_service("gateway"),
            Arc::new(FixedPreference(None)),
            providers,
        );

        let result = registry
            .complete(AiRequest {
                purpose: "contract".to_string(),
                provider: Some("future_provider".to_string()),
                ..Default::default()
            })
            .await
            .expect("registered provider routes through the common contract");

        assert_eq!(result.text, "future:contract");
    }

    #[tokio::test]
    async fn future_provider_receives_the_common_turn_services_contract() {
        use futures::StreamExt;

        let mut providers = HashMap::new();
        providers.insert("future_provider".to_string(), tagged_service("future"));
        let registry = AiProviderRegistry::with_providers(
            tagged_service("gateway"),
            Arc::new(FixedPreference(None)),
            providers,
        );
        let interactions: temps_ai::InteractionExecutor =
            Arc::new(|_| Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) }));
        let tools: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("ok".to_string()) }));
        let mut stream = registry
            .chat_stream_turn_with_services(
                ChatTurnRequest {
                    purpose: "turn".to_string(),
                    provider: Some("future_provider".to_string()),
                    ..Default::default()
                },
                TurnServices {
                    tools: Some(tools),
                    interactions: Some(interactions),
                },
            )
            .await
            .expect("future provider receives normalized turn services");

        assert!(matches!(
            stream.next().await,
            Some(Ok(temps_ai::ChatStreamDelta::Text(text))) if text == "future:turn:true:true"
        ));
    }

    #[tokio::test]
    async fn test_with_agent_cli_falls_back_to_gateway_for_unknown_id() {
        let gateway = tagged_service("gateway");
        // Preference names a provider that has no registered service (e.g. a
        // stale/removed catalog id) — must fall back to the gateway rather
        // than error.
        let dispatching = DispatchingAiService::with_agent_cli(
            gateway,
            Arc::new(FixedPreference(Some("does_not_exist"))),
            HashMap::new(),
        );

        let result = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.text, "gateway:test");
    }

    #[tokio::test]
    async fn test_explicit_unknown_provider_pin_fails_closed() {
        let dispatching = DispatchingAiService::with_agent_cli(
            tagged_service("gateway"),
            Arc::new(FixedPreference(None)),
            HashMap::new(),
        );

        let error = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                provider: Some("removed_cli".into()),
                ..Default::default()
            })
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            AiError::Provider { reason, .. } if reason.contains("removed_cli")
        ));
    }

    #[tokio::test]
    async fn test_with_agent_cli_uses_gateway_when_preference_is_gateway() {
        let gateway = tagged_service("gateway");
        let mut agent_cli_services = HashMap::new();
        agent_cli_services.insert("claude_cli".to_string(), tagged_service("claude_cli"));

        let dispatching = DispatchingAiService::with_agent_cli(
            gateway,
            Arc::new(FixedPreference(None)),
            agent_cli_services,
        );

        let result = dispatching
            .complete(AiRequest {
                purpose: "test".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.text, "gateway:test");
    }

    #[tokio::test]
    async fn test_with_agent_cli_routes_scoped_tool_executor_to_active_cli() {
        use futures::StreamExt;

        let gateway = tagged_service("gateway");
        let mut agent_cli_services = HashMap::new();
        agent_cli_services.insert("claude_cli".to_string(), tagged_service("claude_cli"));

        let dispatching = DispatchingAiService::with_agent_cli(
            gateway,
            Arc::new(FixedPreference(Some("claude_cli"))),
            agent_cli_services,
        );

        let executor: ToolExecutor = Arc::new(|_call| Box::pin(async { Ok("ok".to_string()) }));
        let mut stream = dispatching
            .chat_stream_turn_with_executor(
                ChatTurnRequest {
                    purpose: "test".into(),
                    ..Default::default()
                },
                Some(executor),
            )
            .await
            .unwrap();

        assert!(matches!(
            stream.next().await,
            Some(Ok(temps_ai::ChatStreamDelta::Text(text))) if text == "claude_cli:test:true"
        ));
    }
}
