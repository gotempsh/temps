//! Provider-neutral structured-output streaming with bounded backend caching.
//!
//! Screens provide a prompt and JSON Schema; this service owns provider
//! routing, final schema validation, partial top-level object snapshots, and
//! cache semantics. It deliberately has no knowledge of traffic analytics (or
//! any other feature), so the same endpoint can power cards throughout Temps.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};
use moka::future::Cache;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use temps_ai::{AiRequest, AiService};

const CACHE_CAPACITY: u64 = 512;
const DEFAULT_CACHE_TTL_SECONDS: u64 = 60;
const MAX_CACHE_TTL_SECONDS: u64 = 300;
const MAX_STREAM_BYTES: usize = 128 * 1024;
const STREAM_TIMEOUT: Duration = Duration::from_secs(35);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(10);
const REQUEST_WINDOW: Duration = Duration::from_secs(60);
const MAX_REQUESTS_PER_WINDOW: usize = 12;
const MAX_CONCURRENT_PER_CALLER: usize = 2;
const MAX_REQUESTS_PER_PROJECT_WINDOW: usize = 30;
const MAX_CONCURRENT_PER_PROJECT: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum StructuredOutputError {
    #[error("invalid structured AI request field '{field}': {reason}")]
    InvalidRequest { field: &'static str, reason: String },
    #[error("no active AI provider is available")]
    NotAvailable,
    #[error("AI provider could not start the structured stream: {0}")]
    Provider(#[from] temps_ai::AiError),
    #[error("structured AI request timed out after {seconds} seconds")]
    Timeout { seconds: u64 },
    #[error("structured AI request is rate limited: {reason}")]
    RateLimited { reason: &'static str },
}

#[derive(Debug, Clone)]
pub struct StructuredOutputRequest {
    pub project_id: i32,
    pub purpose: String,
    pub prompt: String,
    pub schema: Value,
    pub provider_key_id: Option<i32>,
    pub model: Option<String>,
    pub thinking_level: Option<String>,
    pub requester_id: Option<i32>,
    pub cache_key: Option<String>,
    pub cache_ttl_seconds: Option<u64>,
    pub refresh: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StructuredOutputEvent {
    Started {
        cached: bool,
        provider: String,
        model: String,
        thinking_level: Option<String>,
    },
    Partial {
        value: Value,
    },
    Complete {
        value: Value,
        cached: bool,
    },
    Error {
        code: &'static str,
        message: String,
    },
}

pub type StructuredOutputStream =
    Pin<Box<dyn Stream<Item = StructuredOutputEvent> + Send + 'static>>;

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct StructuredCacheKey {
    project_id: i32,
    purpose: String,
    caller_key: Option<String>,
    provider: String,
    model: String,
    thinking_level: Option<String>,
    content_digest: String,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct CallerLimitKey {
    project_id: i32,
    requester_id: Option<i32>,
}

#[derive(Default)]
struct CallerLimitState {
    recent_requests: VecDeque<Instant>,
    in_flight: usize,
    last_refresh_by_purpose: HashMap<String, Instant>,
}

struct CallerPermit {
    states: Vec<Arc<StdMutex<CallerLimitState>>>,
}

impl Drop for CallerPermit {
    fn drop(&mut self) {
        for state in &self.states {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Clone)]
struct CachedOutput {
    value: Value,
    expires_at: Instant,
    provider: String,
    model: String,
    thinking_level: Option<String>,
}

pub struct StructuredOutputService {
    ai: Arc<dyn AiService>,
    cache: Cache<StructuredCacheKey, CachedOutput>,
    singleflight: Cache<StructuredCacheKey, Arc<tokio::sync::Mutex<()>>>,
    caller_limits: Cache<CallerLimitKey, Arc<StdMutex<CallerLimitState>>>,
    project_limits: Cache<i32, Arc<StdMutex<CallerLimitState>>>,
}

impl StructuredOutputService {
    pub fn new(ai: Arc<dyn AiService>) -> Self {
        Self {
            ai,
            cache: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_live(Duration::from_secs(MAX_CACHE_TTL_SECONDS))
                .build(),
            singleflight: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_idle(Duration::from_secs(MAX_CACHE_TTL_SECONDS))
                .build(),
            caller_limits: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_idle(Duration::from_secs(MAX_CACHE_TTL_SECONDS))
                .build(),
            project_limits: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_idle(Duration::from_secs(MAX_CACHE_TTL_SECONDS))
                .build(),
        }
    }

    pub async fn stream(
        &self,
        request: StructuredOutputRequest,
    ) -> Result<StructuredOutputStream, StructuredOutputError> {
        validate_request(&request)?;
        let validator = jsonschema::validator_for(&request.schema).map_err(|error| {
            StructuredOutputError::InvalidRequest {
                field: "schema",
                reason: error.to_string(),
            }
        })?;

        let requested_route = request
            .provider_key_id
            .map(|key_id| format!("gateway_key:{key_id}"));
        if !self.ai.is_available_for(requested_route.as_deref()).await {
            return Err(StructuredOutputError::NotAvailable);
        }
        let capabilities = self
            .ai
            .capabilities_for(requested_route.as_deref(), temps_ai::RefreshPolicy::Cached)
            .await
            .map_err(|error| match error {
                temps_ai::AiError::NotAvailable => StructuredOutputError::NotAvailable,
                other => StructuredOutputError::Provider(other),
            })?;
        let provider = capabilities.id.clone();
        let model = request
            .model
            .clone()
            .or_else(|| capabilities.default_model_id.clone())
            .ok_or_else(|| StructuredOutputError::InvalidRequest {
                field: "model",
                reason: "the selected provider has no usable default model".to_string(),
            })?;
        let model_capability =
            capabilities
                .model(&model)
                .ok_or_else(|| StructuredOutputError::InvalidRequest {
                    field: "model",
                    reason: format!(
                        "model '{model}' is not enabled and available for provider '{}'",
                        capabilities.name
                    ),
                })?;
        if let Some(thinking) = request.thinking_level.as_deref() {
            if !model_capability
                .thinking_modes
                .iter()
                .any(|option| option.id == thinking)
            {
                return Err(StructuredOutputError::InvalidRequest {
                    field: "thinking_level",
                    reason: format!(
                        "thinking level '{thinking}' is not supported by model '{model}'"
                    ),
                });
            }
        }

        let cache_key = cache_key_for(&request, &provider, &model);
        self.reserve_refresh(&request).await?;
        if !request.refresh {
            if let Some(cached) = self.cache.get(&cache_key).await {
                if cached.expires_at > Instant::now() {
                    return Ok(cached_stream(cached));
                }
                self.cache.invalidate(&cache_key).await;
            }
        }

        // Per-content mutexes coalesce identical cache misses. Followers wait
        // for the leader's stream to complete and then reuse its cached value.
        let content_lock = self
            .singleflight
            .get_with(cache_key.clone(), async {
                Arc::new(tokio::sync::Mutex::new(()))
            })
            .await;
        let content_guard = content_lock.lock_owned().await;
        if !request.refresh {
            if let Some(cached) = self.cache.get(&cache_key).await {
                if cached.expires_at > Instant::now() {
                    return Ok(cached_stream(cached));
                }
                self.cache.invalidate(&cache_key).await;
            }
        }

        let caller_permit = self.acquire_caller_permit(&request).await?;

        let deadline = tokio::time::Instant::now() + STREAM_TIMEOUT;
        let token_stream = tokio::time::timeout_at(
            deadline,
            self.ai.complete_stream(AiRequest {
                purpose: request.purpose.clone(),
                project_id: Some(request.project_id),
                provider: Some(provider.clone()),
                system: Some(
                    "Return only one JSON object matching the supplied schema. Put concise fields before longer arrays so the interface can render useful content immediately. Never wrap the object in Markdown."
                        .to_string(),
                ),
                prompt: request.prompt,
                model: Some(model.clone()),
                thinking_level: request.thinking_level.clone(),
                response_schema: Some(request.schema),
                max_tokens: Some(2_048),
                // New reasoning models across OpenAI, Anthropic, and Gemini
                // reject non-default sampling controls. Structured output is
                // constrained by schema and prompt, so omit sampling entirely.
                temperature: None,
            }),
        )
        .await
        .map_err(|_| StructuredOutputError::Timeout {
            seconds: STREAM_TIMEOUT.as_secs(),
        })??;

        let cache = self.cache.clone();
        let ttl = Duration::from_secs(
            request
                .cache_ttl_seconds
                .unwrap_or(DEFAULT_CACHE_TTL_SECONDS)
                .clamp(1, MAX_CACHE_TTL_SECONDS),
        );
        let thinking_level = request.thinking_level.clone();
        let stream = async_stream::stream! {
            // Hold both guards for the entire provider stream. Dropping the
            // client response also releases capacity and lets one waiter retry.
            let _content_guard = content_guard;
            let _caller_permit = caller_permit;
            yield StructuredOutputEvent::Started {
                cached: false,
                provider: provider.clone(),
                model: model.clone(),
                thinking_level: thinking_level.clone(),
            };
            let mut tokens = token_stream;
            let mut raw = String::new();
            let mut last_partial = Value::Object(Map::new());
            loop {
                let next = match tokio::time::timeout_at(deadline, tokens.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        yield StructuredOutputEvent::Error {
                            code: "timeout",
                            message: format!(
                                "Structured AI response timed out after {} seconds",
                                STREAM_TIMEOUT.as_secs()
                            ),
                        };
                        return;
                    }
                };
                let Some(item) = next else { break };
                match item {
                    Ok(delta) => {
                        raw.push_str(&delta);
                        if raw.len() > MAX_STREAM_BYTES {
                            yield StructuredOutputEvent::Error {
                                code: "response_too_large",
                                message: format!(
                                    "Structured AI response exceeded the {} byte limit",
                                    MAX_STREAM_BYTES
                                ),
                            };
                            return;
                        }
                        let partial = Value::Object(parse_complete_top_level_fields(&raw));
                        if partial != last_partial {
                            last_partial = partial.clone();
                            yield StructuredOutputEvent::Partial { value: partial };
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            project_id = request.project_id,
                            purpose = request.purpose,
                            %error,
                            "Structured AI provider stream failed"
                        );
                        yield StructuredOutputEvent::Error {
                            code: "provider_error",
                            message: "The AI provider stream ended unexpectedly".to_string(),
                        };
                        return;
                    }
                }
            }

            let Some(value) = temps_ai::extract_json_block(&raw) else {
                yield StructuredOutputEvent::Error {
                    code: "invalid_json",
                    message: "AI provider returned no complete JSON object".to_string(),
                };
                return;
            };
            if let Err(error) = validator.validate(&value) {
                yield StructuredOutputEvent::Error {
                    code: "schema_validation_failed",
                    message: error.to_string(),
                };
                return;
            }

            cache
                .insert(
                    cache_key,
                    CachedOutput {
                        value: value.clone(),
                        expires_at: Instant::now() + ttl,
                        provider: provider.clone(),
                        model: model.clone(),
                        thinking_level: thinking_level.clone(),
                    },
                )
                .await;
            if value != last_partial {
                yield StructuredOutputEvent::Partial {
                    value: value.clone(),
                };
            }
            yield StructuredOutputEvent::Complete {
                value,
                cached: false,
            };
        };
        Ok(Box::pin(stream))
    }

    async fn acquire_caller_permit(
        &self,
        request: &StructuredOutputRequest,
    ) -> Result<CallerPermit, StructuredOutputError> {
        let key = CallerLimitKey {
            project_id: request.project_id,
            requester_id: request.requester_id,
        };
        let state = self
            .caller_limits
            .get_with(key, async {
                Arc::new(StdMutex::new(CallerLimitState::default()))
            })
            .await;
        reserve_limit(
            &state,
            MAX_REQUESTS_PER_WINDOW,
            MAX_CONCURRENT_PER_CALLER,
            "provider-backed requests are limited to 12 per minute per caller and project",
            "at most two provider-backed requests may run concurrently per caller and project",
        )?;

        let project_state = self
            .project_limits
            .get_with(request.project_id, async {
                Arc::new(StdMutex::new(CallerLimitState::default()))
            })
            .await;
        if let Err(error) = reserve_limit(
            &project_state,
            MAX_REQUESTS_PER_PROJECT_WINDOW,
            MAX_CONCURRENT_PER_PROJECT,
            "provider-backed requests are limited to 30 per minute per project",
            "at most four provider-backed requests may run concurrently per project",
        ) {
            let mut caller = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            caller.in_flight = caller.in_flight.saturating_sub(1);
            return Err(error);
        }
        Ok(CallerPermit {
            states: vec![state, project_state],
        })
    }

    async fn reserve_refresh(
        &self,
        request: &StructuredOutputRequest,
    ) -> Result<(), StructuredOutputError> {
        if !request.refresh {
            return Ok(());
        }
        let key = CallerLimitKey {
            project_id: request.project_id,
            requester_id: request.requester_id,
        };
        let state = self
            .caller_limits
            .get_with(key, async {
                Arc::new(StdMutex::new(CallerLimitState::default()))
            })
            .await;
        let now = Instant::now();
        let mut limits = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        limits
            .last_refresh_by_purpose
            .retain(|_, refreshed| now.duration_since(*refreshed) < REQUEST_WINDOW);
        if limits
            .last_refresh_by_purpose
            .get(&request.purpose)
            .is_some_and(|last| now.duration_since(*last) < REFRESH_COOLDOWN)
        {
            return Err(StructuredOutputError::RateLimited {
                reason:
                    "refresh is limited to once every 10 seconds per caller, project, and purpose",
            });
        }
        limits
            .last_refresh_by_purpose
            .insert(request.purpose.clone(), now);
        Ok(())
    }
}

fn reserve_limit(
    state: &Arc<StdMutex<CallerLimitState>>,
    max_requests: usize,
    max_concurrent: usize,
    request_reason: &'static str,
    concurrent_reason: &'static str,
) -> Result<(), StructuredOutputError> {
    let now = Instant::now();
    let mut limits = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while limits
        .recent_requests
        .front()
        .is_some_and(|started| now.duration_since(*started) >= REQUEST_WINDOW)
    {
        limits.recent_requests.pop_front();
    }
    if limits.recent_requests.len() >= max_requests {
        return Err(StructuredOutputError::RateLimited {
            reason: request_reason,
        });
    }
    if limits.in_flight >= max_concurrent {
        return Err(StructuredOutputError::RateLimited {
            reason: concurrent_reason,
        });
    }
    limits.recent_requests.push_back(now);
    limits.in_flight += 1;
    Ok(())
}

fn cached_stream(cached: CachedOutput) -> StructuredOutputStream {
    let value = cached.value;
    Box::pin(futures_util::stream::iter([
        StructuredOutputEvent::Started {
            cached: true,
            provider: cached.provider,
            model: cached.model,
            thinking_level: cached.thinking_level,
        },
        StructuredOutputEvent::Partial {
            value: value.clone(),
        },
        StructuredOutputEvent::Complete {
            value,
            cached: true,
        },
    ]))
}

fn validate_request(request: &StructuredOutputRequest) -> Result<(), StructuredOutputError> {
    let purpose_is_valid = !request.purpose.is_empty()
        && request.purpose.len() <= 64
        && request
            .purpose
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !purpose_is_valid {
        return Err(StructuredOutputError::InvalidRequest {
            field: "purpose",
            reason: "must be 1-64 ASCII letters, digits, dots, underscores, or hyphens".to_string(),
        });
    }
    if request.prompt.trim().is_empty() || request.prompt.len() > 32 * 1024 {
        return Err(StructuredOutputError::InvalidRequest {
            field: "prompt",
            reason: "must contain 1-32768 bytes".to_string(),
        });
    }
    let schema_size = serde_json::to_vec(&request.schema)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if schema_size > 32 * 1024 {
        return Err(StructuredOutputError::InvalidRequest {
            field: "schema",
            reason: "must not exceed 32768 serialized bytes".to_string(),
        });
    }
    if !request.schema.is_object() {
        return Err(StructuredOutputError::InvalidRequest {
            field: "schema",
            reason: "must be a JSON Schema object".to_string(),
        });
    }
    if request.provider_key_id.is_some_and(|id| id <= 0) {
        return Err(StructuredOutputError::InvalidRequest {
            field: "provider_key_id",
            reason: "must be a positive provider key id".to_string(),
        });
    }
    for (field, value) in [
        ("model", request.model.as_deref()),
        ("thinking_level", request.thinking_level.as_deref()),
    ] {
        if let Some(value) = value {
            let valid = !value.is_empty()
                && value.len() <= 128
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '_' | '/' | '-' | ':')
                });
            if !valid {
                return Err(StructuredOutputError::InvalidRequest {
                    field,
                    reason: "must contain 1-128 letters, digits, dots, underscores, slashes, hyphens, or colons"
                        .to_string(),
                });
            }
        }
    }
    if request
        .cache_key
        .as_ref()
        .is_some_and(|key| key.is_empty() || key.len() > 128)
    {
        return Err(StructuredOutputError::InvalidRequest {
            field: "cache_key",
            reason: "must contain 1-128 bytes when supplied".to_string(),
        });
    }
    Ok(())
}

fn cache_key_for(
    request: &StructuredOutputRequest,
    provider: &str,
    model: &str,
) -> StructuredCacheKey {
    let mut hasher = Sha256::new();
    // An explicit cache key is an intentional stable identity supplied by a
    // screen (for example, a rolling "last 24 hours" summary). Hashing the
    // full prompt as well would defeat that contract because timestamps and
    // aggregate values naturally move on every load. Without an explicit key,
    // remain fully content-addressed.
    if request.cache_key.is_none() {
        hasher.update(request.prompt.as_bytes());
    }
    if let Ok(schema) = serde_json::to_vec(&request.schema) {
        hasher.update(schema);
    }
    StructuredCacheKey {
        project_id: request.project_id,
        purpose: request.purpose.clone(),
        caller_key: request.cache_key.clone(),
        provider: provider.to_string(),
        model: model.to_string(),
        thinking_level: request.thinking_level.clone(),
        content_digest: hex::encode(hasher.finalize()),
    }
}

/// Parse every fully-closed top-level property from a possibly incomplete JSON
/// object. Nested objects and arrays are emitted only when their closing token
/// arrives, which makes each snapshot valid JSON without fabricating values.
fn parse_complete_top_level_fields(input: &str) -> Map<String, Value> {
    let Some(object_start) = input.find('{') else {
        return Map::new();
    };
    let bytes = input.as_bytes();
    let mut cursor = object_start + 1;
    let mut fields = BTreeMap::new();

    loop {
        cursor = skip_whitespace(bytes, cursor);
        if bytes.get(cursor) == Some(&b',') {
            cursor += 1;
            continue;
        }
        if bytes.get(cursor).is_none() || bytes.get(cursor) == Some(&b'}') {
            break;
        }
        if bytes.get(cursor) != Some(&b'"') {
            break;
        }
        let Some(key_end) = string_end(bytes, cursor) else {
            break;
        };
        let Ok(key) = serde_json::from_str::<String>(&input[cursor..key_end]) else {
            break;
        };
        cursor = skip_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            break;
        }
        cursor = skip_whitespace(bytes, cursor + 1);
        let Some(value_end) = complete_value_end(bytes, cursor) else {
            break;
        };
        let Ok(value) = serde_json::from_str::<Value>(&input[cursor..value_end]) else {
            break;
        };
        fields.insert(key, value);
        cursor = value_end;
    }

    fields.into_iter().collect()
}

fn skip_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, byte) in bytes.get(start + 1..)?.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'"' => return Some(start + offset + 2),
            _ => {}
        }
    }
    None
}

fn complete_value_end(bytes: &[u8], start: usize) -> Option<usize> {
    match *bytes.get(start)? {
        b'"' => string_end(bytes, start),
        b'{' | b'[' => {
            let open = bytes[start];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0usize;
            let mut cursor = start;
            let mut in_string = false;
            let mut escaped = false;
            while let Some(byte) = bytes.get(cursor) {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if *byte == b'\\' {
                        escaped = true;
                    } else if *byte == b'"' {
                        in_string = false;
                    }
                } else if *byte == b'"' {
                    in_string = true;
                } else if *byte == open {
                    depth += 1;
                } else if *byte == close {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(cursor + 1);
                    }
                }
                cursor += 1;
            }
            None
        }
        _ => {
            let mut cursor = start;
            while let Some(byte) = bytes.get(cursor) {
                if matches!(byte, b',' | b'}') {
                    let end = skip_trailing_whitespace(bytes, start, cursor);
                    return (end > start).then_some(end);
                }
                cursor += 1;
            }
            None
        }
    }
}

fn skip_trailing_whitespace(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StreamingAi {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AiService for StreamingAi {
        async fn is_available(&self) -> bool {
            true
        }

        async fn capabilities_for(
            &self,
            _provider: Option<&str>,
            _refresh: temps_ai::RefreshPolicy,
        ) -> Result<temps_ai::ProviderCapabilities, temps_ai::AiError> {
            Ok(temps_ai::ProviderCapabilities {
                id: "gateway_key:1".to_string(),
                name: "Test gateway".to_string(),
                auth_source: temps_ai::ProviderAuthSource::ConfiguredKey,
                models: vec![temps_ai::ModelCapability {
                    id: "test-model".to_string(),
                    name: "Test model".to_string(),
                    thinking_modes: vec![temps_ai::SelectOption {
                        id: "high".to_string(),
                        name: "High".to_string(),
                        description: None,
                    }],
                    tool_thinking_modes: None,
                    default_thinking_mode_id: None,
                }],
                default_model_id: Some("test-model".to_string()),
                permission_modes: Vec::new(),
                default_permission_mode_id: None,
                realtime: temps_ai::RealtimeCapabilities {
                    text_streaming: true,
                    reasoning_streaming: false,
                    tool_events: false,
                    user_interactions: false,
                    cancellation: true,
                },
            })
        }

        async fn complete(
            &self,
            _request: AiRequest,
        ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn complete_stream(
            &self,
            _request: AiRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(futures_util::stream::iter([
                Ok("{\"headline\":\"Ready\",".to_string()),
                Ok("\"findings\":[\"One\"]}".to_string()),
            ])))
        }

        async fn chat_stream(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }
    }

    fn request(refresh: bool) -> StructuredOutputRequest {
        StructuredOutputRequest {
            project_id: 7,
            purpose: "test.summary".to_string(),
            prompt: "Summarize this".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "headline": {"type": "string"},
                    "findings": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["headline", "findings"],
                "additionalProperties": false
            }),
            provider_key_id: None,
            model: None,
            thinking_level: None,
            requester_id: Some(11),
            cache_key: Some("window:1".to_string()),
            cache_ttl_seconds: Some(60),
            refresh,
        }
    }

    #[test]
    fn partial_parser_emits_only_complete_top_level_fields() {
        let partial =
            parse_complete_top_level_fields(r#"prefix {"headline":"Ready","findings":["one", "tw"#);
        assert_eq!(
            partial.get("headline"),
            Some(&Value::String("Ready".into()))
        );
        assert!(!partial.contains_key("findings"));

        let complete =
            parse_complete_top_level_fields(r#"{"headline":"Ready","findings":["one", "two"]}"#);
        assert_eq!(complete.len(), 2);
    }

    #[test]
    fn explicit_cache_key_is_stable_across_moving_prompt_content() {
        let first = request(false);
        let mut second = request(false);
        second.prompt = "Summarize this one minute later".to_string();

        assert_eq!(
            cache_key_for(&first, "gateway_key:1", "model-a"),
            cache_key_for(&second, "gateway_key:1", "model-a")
        );
    }

    #[test]
    fn prompt_remains_part_of_implicit_cache_identity() {
        let mut first = request(false);
        first.cache_key = None;
        let mut second = first.clone();
        second.prompt = "Different content".to_string();

        assert_ne!(
            cache_key_for(&first, "gateway_key:1", "model-a"),
            cache_key_for(&second, "gateway_key:1", "model-a")
        );
    }

    #[tokio::test]
    async fn cache_hit_skips_provider_and_refresh_bypasses_cache() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = StructuredOutputService::new(Arc::new(StreamingAi {
            calls: calls.clone(),
        }));

        for refresh in [false, false, true] {
            let events: Vec<_> = service
                .stream(request(refresh))
                .await
                .expect("stream should start")
                .collect()
                .await;
            assert!(matches!(
                events.last(),
                Some(StructuredOutputEvent::Complete { .. })
            ));
        }

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn repeated_force_refresh_is_rate_limited() {
        let service = StructuredOutputService::new(Arc::new(StreamingAi {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
        let _first_stream = service
            .stream(request(true))
            .await
            .expect("first refresh should start");
        assert!(matches!(
            service.stream(request(true)).await,
            Err(StructuredOutputError::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn project_concurrency_limit_applies_across_distinct_callers() {
        let service = StructuredOutputService::new(Arc::new(StreamingAi {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
        let mut permits = Vec::new();
        for requester_id in 1..=MAX_CONCURRENT_PER_PROJECT {
            let mut candidate = request(false);
            candidate.requester_id = Some(requester_id as i32);
            permits.push(
                service
                    .acquire_caller_permit(&candidate)
                    .await
                    .expect("project capacity should remain"),
            );
        }
        let mut blocked = request(false);
        blocked.requester_id = Some(99);
        assert!(matches!(
            service.acquire_caller_permit(&blocked).await,
            Err(StructuredOutputError::RateLimited { .. })
        ));
        drop(permits);
    }

    #[tokio::test]
    async fn identical_cache_misses_are_singleflight() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(StructuredOutputService::new(Arc::new(StreamingAi {
            calls: calls.clone(),
        })));

        let first = service
            .stream(request(false))
            .await
            .expect("first stream should start");
        let follower_service = service.clone();
        let follower = tokio::spawn(async move {
            follower_service
                .stream(request(false))
                .await
                .expect("follower should reuse the completed result")
                .collect::<Vec<_>>()
                .await
        });
        tokio::task::yield_now().await;

        let first_events = first.collect::<Vec<_>>().await;
        let follower_events = follower.await.expect("follower task should finish");

        assert!(matches!(
            first_events.first(),
            Some(StructuredOutputEvent::Started { cached: false, .. })
        ));
        assert!(matches!(
            follower_events.first(),
            Some(StructuredOutputEvent::Started { cached: true, .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejects_models_and_thinking_modes_outside_capabilities() {
        let service = StructuredOutputService::new(Arc::new(StreamingAi {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
        let mut bad_model = request(false);
        bad_model.model = Some("unknown".to_string());
        assert!(matches!(
            service.stream(bad_model).await,
            Err(StructuredOutputError::InvalidRequest { field: "model", .. })
        ));

        let mut bad_thinking = request(false);
        bad_thinking.thinking_level = Some("ultra".to_string());
        assert!(matches!(
            service.stream(bad_thinking).await,
            Err(StructuredOutputError::InvalidRequest {
                field: "thinking_level",
                ..
            })
        ));
    }
}
