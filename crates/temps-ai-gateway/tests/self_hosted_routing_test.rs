// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Catalog-first routing and self-hosted OpenAI-compatible providers.
//!
//! Two recording upstreams stand in for a self-hosted model server (MLX-style
//! `org/model` IDs, no authentication) and OpenRouter. Provider keys and
//! their catalogs live in a real PostgreSQL database, so these tests exercise
//! the same queries as production and prove which upstream every request
//! reached:
//!
//! * a `vendor/model` ID listed only by the self-hosted key is served by it,
//!   never by OpenRouter -- even though OpenRouter's name heuristic matches;
//! * an ID served by two upstreams is refused as ambiguous until a key is
//!   picked explicitly;
//! * a model the self-hosted key hosts but cannot serve right now is refused,
//!   not forwarded to OpenRouter;
//! * built-in routing (`openai/...` through OpenRouter) is unchanged.
//!
//! Both mocks listen on loopback, which the instance-wide policy blocks, so
//! each key trusts its own exact `127.0.0.1:<port>` (#1411). The global
//! trusted-networks list is never touched here.
//!
//! Database tests skip gracefully when Docker/PostgreSQL is unavailable.

// Shared with provider_integration_test.rs; this binary uses only part of it.
#[allow(dead_code)]
mod mock_provider_servers;

use std::sync::Arc;

use mock_provider_servers::MockRecordingServer;
use temps_ai_gateway::error::AiGatewayError;
use temps_ai_gateway::provider_url::KeyTrust;
use temps_ai_gateway::providers::openai_compat::OpenAiCompatProvider;
use temps_ai_gateway::providers::AiProvider;
use temps_ai_gateway::self_hosted::{SelfHostedCapabilities, OPENAI_COMPATIBLE};
use temps_ai_gateway::services::{
    ByokOverride, GatewayService, NewProviderKey, ProviderKeyService, ProviderModelService,
    TrustRequest,
};
use temps_ai_gateway::types::*;
use temps_core::EncryptionService;
use temps_database::test_utils::TestDatabase;
use temps_entities::ai_provider_keys;
use tokio_stream::StreamExt;

const SELF_HOSTED_MODEL: &str = "mlx-community/Some-Model-4bit";
const SELF_HOSTED_EMBEDDING_MODEL: &str = "mlx-community/embed-small";
const OPENROUTER_MODEL: &str = "openai/gpt-4o-mini";
/// Listed only by the tools-off server in the AI chat capability test.
const TOOLS_OFF_ONLY_MODEL: &str = "mlx-community/tools-off-only";

struct Harness {
    _db: TestDatabase,
    keys: Arc<ProviderKeyService>,
    gateway: Arc<GatewayService>,
    models: ProviderModelService,
}

async fn harness() -> Option<Harness> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error) => {
            println!("PostgreSQL test database unavailable, skipping: {error}");
            return None;
        }
    };
    let connection = db.connection_arc();
    let encryption = Arc::new(
        EncryptionService::new("01234567890123456789012345678901").expect("test encryption key"),
    );
    let keys = Arc::new(ProviderKeyService::new(connection.clone(), encryption));
    let gateway = Arc::new(GatewayService::new(keys.clone()));
    let models = ProviderModelService::new(connection, keys.clone(), gateway.clone());
    Some(Harness {
        _db: db,
        keys,
        gateway,
        models,
    })
}

fn chat(model: &str, stream: bool) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("Say ok".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream,
        temperature: None,
        max_tokens: Some(16),
        top_p: None,
        stop: None,
        n: None,
        tools: None,
        tool_choice: None,
        response_format: None,
        frequency_penalty: None,
        presence_penalty: None,
        seed: None,
        user: None,
        extra: None,
    }
}

fn embedding(model: &str) -> EmbeddingRequest {
    serde_json::from_value(serde_json::json!({"model": model, "input": "hello"}))
        .expect("valid embedding request")
}

fn trust() -> Option<TrustRequest> {
    Some(TrustRequest { set_by: None })
}

/// Create a key the way the console does: discover first, then persist the
/// listing as the key's catalog.
async fn add_key(
    harness: &Harness,
    provider: &str,
    api_key: &str,
    server: &MockRecordingServer,
    capabilities: Option<SelfHostedCapabilities>,
) -> ai_provider_keys::Model {
    let key_trust = KeyTrust::for_base_url(&server.base_url).expect("loopback is trustable");
    let discovered = harness
        .gateway
        .discover_models(provider, api_key, Some(&server.base_url), Some(key_trust))
        .await
        .expect("discovery succeeds with per-key trust");
    let key = harness
        .keys
        .create(NewProviderKey {
            provider,
            display_name: provider,
            api_key,
            base_url: Some(&server.base_url),
            default_model: None,
            trust: trust(),
            capabilities,
        })
        .await
        .expect("create provider key");
    harness
        .models
        .apply_discovered(key.id, discovered)
        .await
        .expect("store discovered catalog");
    key
}

#[tokio::test]
async fn vendor_model_hosted_only_by_self_hosted_key_never_reaches_openrouter() {
    let Some(harness) = harness().await else {
        return;
    };
    let self_hosted =
        MockRecordingServer::start(&[SELF_HOSTED_MODEL, SELF_HOSTED_EMBEDDING_MODEL], false).await;
    let openrouter = MockRecordingServer::start(&[OPENROUTER_MODEL], true).await;

    let openrouter_key = add_key(&harness, "openrouter", "sk-or-fake", &openrouter, None).await;
    let self_hosted_key = add_key(
        &harness,
        OPENAI_COMPATIBLE,
        "",
        &self_hosted,
        Some(SelfHostedCapabilities {
            embeddings: true,
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        self_hosted_key.trusted_private_address.as_deref(),
        Some(self_hosted.addr.to_string().as_str())
    );

    // The self-hosted catalog kept the server's own IDs verbatim.
    let catalog = harness
        .models
        .list_for_key(self_hosted_key.id)
        .await
        .unwrap();
    let ids: Vec<&str> = catalog
        .iter()
        .map(|model| model.model_id.as_str())
        .collect();
    assert!(ids.contains(&SELF_HOSTED_MODEL), "{ids:?}");
    assert!(ids.contains(&SELF_HOSTED_EMBEDDING_MODEL), "{ids:?}");

    // Non-streaming chat goes to the self-hosted server, without an
    // Authorization header (the key has no API key).
    let (response, served) = harness
        .gateway
        .chat_completion(&chat(SELF_HOSTED_MODEL, false), &ByokOverride::default())
        .await
        .expect("self-hosted completion");
    assert_eq!(served.provider_id, OPENAI_COMPATIBLE);
    assert_eq!(served.system_key_id, Some(self_hosted_key.id));
    assert_eq!(response.model, SELF_HOSTED_MODEL);

    // Streaming chat too.
    let (mut stream, served) = harness
        .gateway
        .chat_completion_stream(&chat(SELF_HOSTED_MODEL, true), &ByokOverride::default())
        .await
        .expect("self-hosted stream");
    assert_eq!(served.provider_id, OPENAI_COMPATIBLE);
    let mut payload = String::new();
    while let Some(chunk) = stream.next().await {
        payload.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
    }
    assert!(payload.contains("from self-hosted"), "{payload}");

    // Embeddings, declared on the key.
    let (embeddings, served) = harness
        .gateway
        .embeddings(
            &embedding(SELF_HOSTED_EMBEDDING_MODEL),
            &ByokOverride::default(),
        )
        .await
        .expect("self-hosted embeddings");
    assert_eq!(served.system_key_id, Some(self_hosted_key.id));
    assert_eq!(embeddings.data.len(), 1);

    let reached_self_hosted = self_hosted.inference_requests();
    assert_eq!(reached_self_hosted.len(), 3, "{reached_self_hosted:?}");
    assert!(reached_self_hosted
        .iter()
        .all(|request| request.authorization.is_none()));
    assert!(
        openrouter.inference_requests().is_empty(),
        "a self-hosted model must never reach OpenRouter: {:?}",
        openrouter.inference_requests()
    );

    // Built-in routing is unchanged: an OpenRouter model goes to OpenRouter
    // with its key.
    let (_, served) = harness
        .gateway
        .chat_completion(&chat(OPENROUTER_MODEL, false), &ByokOverride::default())
        .await
        .expect("openrouter completion");
    assert_eq!(served.provider_id, "openrouter");
    assert_eq!(served.system_key_id, Some(openrouter_key.id));
    let reached_openrouter = openrouter.inference_requests();
    assert_eq!(reached_openrouter.len(), 1);
    assert_eq!(
        reached_openrouter[0].model.as_deref(),
        Some(OPENROUTER_MODEL)
    );
    assert_eq!(
        reached_openrouter[0].authorization.as_deref(),
        Some("Bearer sk-or-fake")
    );

    // A different capitalisation of a self-hosted ID never falls through to
    // the name heuristic (which would pick OpenRouter).
    let error = harness
        .gateway
        .chat_completion(
            &chat(&SELF_HOSTED_MODEL.to_uppercase(), false),
            &ByokOverride::default(),
        )
        .await
        .expect_err("case variant of a self-hosted model must be refused");
    assert!(
        matches!(&error, AiGatewayError::SelfHostedModelUnavailable { key_id, .. } if *key_id == self_hosted_key.id),
        "{error:?}"
    );

    // An ID served by two upstreams is refused, naming both keys ...
    harness
        .models
        .add_manual(openrouter_key.id, SELF_HOSTED_MODEL, None)
        .await
        .expect("list the model on OpenRouter too");
    let error = harness
        .gateway
        .chat_completion(&chat(SELF_HOSTED_MODEL, false), &ByokOverride::default())
        .await
        .expect_err("ambiguous model must be refused");
    match &error {
        AiGatewayError::AmbiguousModel { model, candidates } => {
            assert_eq!(model, SELF_HOSTED_MODEL);
            assert!(candidates.contains(&format!("key {}", self_hosted_key.id)));
            assert!(candidates.contains(&format!("key {}", openrouter_key.id)));
        }
        other => panic!("expected AmbiguousModel, got {other:?}"),
    }
    // ... until the caller picks one explicitly.
    let (_, served) = harness
        .gateway
        .chat_completion(
            &chat(SELF_HOSTED_MODEL, false),
            &ByokOverride {
                system_key_id: Some(self_hosted_key.id),
                ..Default::default()
            },
        )
        .await
        .expect("explicitly selected key");
    assert_eq!(served.system_key_id, Some(self_hosted_key.id));

    // With OpenRouter also listing the ID, deactivating the self-hosted key
    // must not hand the model to OpenRouter.
    harness
        .keys
        .update(
            self_hosted_key.id,
            temps_ai_gateway::services::ProviderKeyUpdate {
                is_active: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("deactivate self-hosted key");
    let error = harness
        .gateway
        .chat_completion(&chat(SELF_HOSTED_MODEL, false), &ByokOverride::default())
        .await
        .expect_err("inactive self-hosted key must not fall back to OpenRouter");
    assert!(
        matches!(&error, AiGatewayError::SelfHostedModelUnavailable { key_id, .. } if *key_id == self_hosted_key.id),
        "{error:?}"
    );
    harness
        .keys
        .update(
            self_hosted_key.id,
            temps_ai_gateway::services::ProviderKeyUpdate {
                is_active: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("reactivate self-hosted key");

    // Remove the OpenRouter listing again and disable the model on the
    // self-hosted key: the request is refused, never sent to OpenRouter.
    let openrouter_row = harness
        .models
        .list_for_key(openrouter_key.id)
        .await
        .unwrap()
        .into_iter()
        .find(|model| model.model_id == SELF_HOSTED_MODEL)
        .unwrap();
    harness
        .models
        .delete_manual(openrouter_key.id, openrouter_row.id)
        .await
        .unwrap();
    let self_hosted_row = catalog
        .iter()
        .find(|model| model.model_id == SELF_HOSTED_MODEL)
        .unwrap();
    harness
        .models
        .set_enabled(self_hosted_key.id, self_hosted_row.id, false)
        .await
        .unwrap();
    let error = harness
        .gateway
        .chat_completion(&chat(SELF_HOSTED_MODEL, false), &ByokOverride::default())
        .await
        .expect_err("disabled self-hosted model must not fall back");
    assert!(
        matches!(
            &error,
            AiGatewayError::SelfHostedModelUnavailable { key_id, .. } if *key_id == self_hosted_key.id
        ),
        "{error:?}"
    );
    // Pinning OpenRouter explicitly cannot route it there either: its
    // catalog does not list the model.
    let error = harness
        .gateway
        .chat_completion(
            &chat(SELF_HOSTED_MODEL, false),
            &ByokOverride {
                system_key_id: Some(openrouter_key.id),
                ..Default::default()
            },
        )
        .await
        .expect_err("OpenRouter does not serve the self-hosted model");
    assert!(
        matches!(error, AiGatewayError::Validation { .. }),
        "{error:?}"
    );

    assert_eq!(
        openrouter.inference_requests().len(),
        1,
        "only the explicit OpenRouter model may have reached OpenRouter: {:?}",
        openrouter.inference_requests()
    );
}

#[tokio::test]
async fn undeclared_self_hosted_capabilities_are_refused_before_sending() {
    let Some(harness) = harness().await else {
        return;
    };
    let server =
        MockRecordingServer::start(&[SELF_HOSTED_MODEL, SELF_HOSTED_EMBEDDING_MODEL], false).await;
    let key = add_key(
        &harness,
        OPENAI_COMPATIBLE,
        "",
        &server,
        Some(SelfHostedCapabilities {
            streaming: false,
            tools: true,
            embeddings: false,
            responses_api: false,
        }),
    )
    .await;

    let error = harness
        .gateway
        .embeddings(
            &embedding(SELF_HOSTED_EMBEDDING_MODEL),
            &ByokOverride::default(),
        )
        .await
        .expect_err("embeddings were not declared");
    assert!(
        matches!(&error, AiGatewayError::CapabilityNotDeclared { key_id, capability, .. }
            if *key_id == key.id && capability == "embeddings"),
        "{error:?}"
    );
    let error = match harness
        .gateway
        .chat_completion_stream(&chat(SELF_HOSTED_MODEL, true), &ByokOverride::default())
        .await
    {
        Ok(_) => panic!("streaming was not declared"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, AiGatewayError::CapabilityNotDeclared { .. }),
        "{error:?}"
    );
    assert!(server.inference_requests().is_empty());

    // Non-streaming chat still works.
    harness
        .gateway
        .chat_completion(&chat(SELF_HOSTED_MODEL, false), &ByokOverride::default())
        .await
        .expect("declared chat completion");
    assert_eq!(server.inference_requests().len(), 1);
}

#[tokio::test]
async fn self_hosted_key_verification_uses_the_chosen_model_not_a_vendor_default() {
    let Some(harness) = harness().await else {
        return;
    };
    let server = MockRecordingServer::start(&[SELF_HOSTED_MODEL], false).await;
    let key_trust = KeyTrust::for_base_url(&server.base_url).unwrap();

    // Without a chosen model there is nothing to verify with: no request is
    // sent, and gpt-5-nano is never tried.
    let error = harness
        .gateway
        .test_provider(
            OPENAI_COMPATIBLE,
            "",
            Some(&server.base_url),
            None,
            Some(key_trust),
        )
        .await
        .expect_err("a model must be chosen");
    assert!(
        matches!(error, AiGatewayError::Validation { .. }),
        "{error:?}"
    );
    assert!(server.requests().is_empty());

    harness
        .gateway
        .test_provider(
            OPENAI_COMPATIBLE,
            "",
            Some(&server.base_url),
            Some(SELF_HOSTED_MODEL),
            Some(key_trust),
        )
        .await
        .expect("verification with the chosen model");
    let requests = server.inference_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model.as_deref(), Some(SELF_HOSTED_MODEL));
}

#[tokio::test]
async fn ai_chat_follows_self_hosted_capability_declarations() {
    use temps_ai::{
        AiService, ChatMessage as AiChatMessage, ChatStreamDelta, ChatTool, ChatTurnRequest,
        RefreshPolicy,
    };
    use temps_ai_gateway::services::GatewayAiService;

    let Some(harness) = harness().await else {
        return;
    };
    // One server that cannot stream, one that streams but has no tools.
    let no_streaming_server = MockRecordingServer::start(&[SELF_HOSTED_MODEL], false).await;
    let no_tools_server =
        MockRecordingServer::start(&[SELF_HOSTED_MODEL, TOOLS_OFF_ONLY_MODEL], false).await;
    let no_streaming = add_key(
        &harness,
        OPENAI_COMPATIBLE,
        "",
        &no_streaming_server,
        Some(SelfHostedCapabilities {
            streaming: false,
            tools: false,
            embeddings: false,
            responses_api: false,
        }),
    )
    .await;
    let no_tools = add_key(
        &harness,
        OPENAI_COMPATIBLE,
        "",
        &no_tools_server,
        Some(SelfHostedCapabilities {
            streaming: true,
            tools: false,
            embeddings: false,
            responses_api: false,
        }),
    )
    .await;
    let ai = GatewayAiService::new(harness.gateway.clone(), harness._db.connection_arc());

    // AI chat streams every turn: a key that cannot stream is not offered,
    // and the reason names the key and the missing declaration.
    let no_streaming_route = format!("gateway_key:{}", no_streaming.id);
    assert!(!ai.chat_capable_for(Some(&no_streaming_route)).await);
    let reason = ai
        .chat_unavailable_reason(Some(&no_streaming_route))
        .await
        .expect("a specific reason for the non-streaming key");
    assert!(
        reason.contains(&format!("key {}", no_streaming.id)),
        "{reason}"
    );
    assert!(reason.contains("does not declare streaming"), "{reason}");

    let no_tools_route = format!("gateway_key:{}", no_tools.id);
    assert!(ai.chat_capable_for(Some(&no_tools_route)).await);
    assert!(ai
        .chat_unavailable_reason(Some(&no_tools_route))
        .await
        .is_none());
    // Chat asks before adding tools, so it never sends them to this key.
    assert!(
        !ai.tool_calling_for(Some(&no_tools_route), Some(SELF_HOSTED_MODEL))
            .await
    );
    // An unpinned conversation is checked against the key serving its saved
    // model, not the instance default model.
    assert!(
        !ai.tool_calling_for(Some("gateway"), Some(TOOLS_OFF_ONLY_MODEL))
            .await
    );

    // New chats default to the first key that can stream, skipping the
    // lower-id key that cannot -- picked with one query.
    assert_eq!(
        ai.default_chat_provider().await.as_deref(),
        Some(no_tools_route.as_str())
    );

    // The capability report reflects the declarations, not a vendor's.
    let capabilities = ai
        .capabilities_for(Some(&no_tools_route), RefreshPolicy::Cached)
        .await
        .expect("capabilities of the tools-off key");
    assert!(capabilities.realtime.text_streaming);
    assert!(!capabilities.realtime.tool_events);

    let turn = |tools: Vec<ChatTool>| ChatTurnRequest {
        purpose: "test.self_hosted_chat".to_string(),
        provider: Some(no_tools_route.clone()),
        model: Some(SELF_HOSTED_MODEL.to_string()),
        messages: vec![AiChatMessage {
            role: "user".to_string(),
            content: "Say hello".to_string(),
            ..Default::default()
        }],
        tools,
        ..Default::default()
    };

    // A plain turn streams a reply.
    let mut stream = ai
        .chat_stream_turn(turn(Vec::new()))
        .await
        .expect("chat turn on a tools-off key");
    let mut text = String::new();
    while let Some(delta) = stream.next().await {
        if let ChatStreamDelta::Text(chunk) = delta.expect("stream delta") {
            text.push_str(&chunk);
        }
    }
    assert!(text.contains("from self-hosted"), "{text}");

    // A caller that sends tools anyway gets an explicit refusal, never a
    // silent drop, and nothing reaches the server.
    let refused = ai
        .chat_stream_turn(turn(vec![ChatTool {
            name: "lookup".to_string(),
            description: "Look something up".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }]))
        .await;
    let error = match refused {
        Ok(_) => panic!("tools must be refused on a key without tool calling"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("tool calling"), "{error}");
    assert_eq!(no_tools_server.inference_requests().len(), 1);
    assert!(no_streaming_server.inference_requests().is_empty());
}

// ----------------------------------------------------------------------------
// #1410: blocked destinations are reported clearly, before anything is sent
// ----------------------------------------------------------------------------

fn gateway_without_database() -> GatewayService {
    let db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let encryption = Arc::new(EncryptionService::new("01234567890123456789012345678901").unwrap());
    GatewayService::new(Arc::new(ProviderKeyService::new(db, encryption)))
}

fn invalid_url_reason(error: AiGatewayError) -> String {
    match error {
        AiGatewayError::InvalidProviderUrl { reason } => reason,
        other => panic!("expected InvalidProviderUrl, got {other:?}"),
    }
}

#[tokio::test]
async fn untrusted_private_ip_is_refused_with_the_trusted_networks_hint() {
    let server = MockRecordingServer::start(&[SELF_HOSTED_MODEL], false).await;
    let error = gateway_without_database()
        .test_provider("openai", "sk-test", Some(&server.base_url), None, None)
        .await
        .expect_err("loopback is not trusted");
    let reason = invalid_url_reason(error);
    assert!(reason.contains("Loopback"), "{reason}");
    assert!(reason.contains("Trusted private networks"), "{reason}");
    assert!(server.requests().is_empty(), "nothing may be sent");
}

#[tokio::test]
async fn hostname_resolving_to_untrusted_loopback_is_reported_not_wrapped() {
    // Bypass the save-time check and call the adapter directly, as a request
    // would after a hostname started resolving to a private address: the
    // connect-time resolver must refuse it with the same clear reason, not
    // reqwest's opaque "error sending request".
    let server = MockRecordingServer::start(&[SELF_HOSTED_MODEL], false).await;
    let base_url = format!("http://localhost:{}/v1", server.addr.port());
    let error = OpenAiCompatProvider::openai_compatible()
        .chat_completion("", Some(&base_url), &chat(SELF_HOSTED_MODEL, false))
        .await
        .expect_err("localhost resolves to untrusted loopback");
    let reason = invalid_url_reason(error);
    assert!(reason.contains("'localhost'"), "{reason}");
    assert!(reason.contains("Trusted private networks"), "{reason}");
    assert!(!reason.contains("error sending request"), "{reason}");
    assert!(server.requests().is_empty(), "nothing may be sent");
}

#[tokio::test]
async fn plain_http_to_a_public_address_is_refused_before_sending() {
    let error = gateway_without_database()
        .test_provider(
            OPENAI_COMPATIBLE,
            "sk-secret",
            Some("http://8.8.8.8:8000/v1"),
            Some(SELF_HOSTED_MODEL),
            None,
        )
        .await
        .expect_err("plaintext to a public host");
    let reason = invalid_url_reason(error);
    assert!(reason.contains("plain http://"), "{reason}");
    assert!(!reason.contains("sk-secret"), "{reason}");
}

#[tokio::test]
async fn byok_base_url_over_plain_http_to_a_public_host_is_refused() {
    let error = gateway_without_database()
        .resolve_credentials(
            "gpt-4o",
            &ByokOverride {
                api_key: Some("sk-caller".to_string()),
                base_url: Some("http://8.8.8.8/v1".to_string()),
                system_key_id: None,
            },
        )
        .await
        .expect_err("plaintext BYOK to a public host");
    assert!(invalid_url_reason(error).contains("plain http://"));
}
