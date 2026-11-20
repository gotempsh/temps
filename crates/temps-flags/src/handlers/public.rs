// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Evaluation endpoints: the same-origin endpoint browsers call, and OFREP.
//!
//! - `POST /api/_temps/flags`: unauthenticated, tenant resolved from `Host`
//!   through the route table exactly like `/api/_temps/event`. Serves only
//!   `client_visible` flags, and only their evaluated results: rules, salts and
//!   other flags never reach the browser (ADR 034 R5.1, R5.3).
//! - `POST /api/_temps/ofrep/v1/evaluate/flags[/{key}]`: the same, in the
//!   OpenFeature Remote Evaluation Protocol, for OpenFeature web SDKs.
//! - `POST /api/ofrep/v1/evaluate/flags[/{key}]`: OFREP for server-side
//!   OpenFeature SDKs in any language, authenticated by the deployment token
//!   Temps injects (`TEMPS_API_TOKEN`), scoped to its project and environment,
//!   and serving every flag.
//!
//! All of them read flags from the in-memory delivery cache (no database
//! query per request), evaluate with precompiled patterns, and are bounded:
//! a body cap, an attribute cap, and on the unauthenticated ones a lock-free
//! per-project and per-address rate limit (R5.2).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_authz::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use temps_geo::GeoIpService;
use temps_routes::CachedPeerTable;
use tracing::{debug, warn};
use utoipa::ToSchema;

use super::rate_limit::SlotRateLimiter;
use crate::error::FlagError;
use crate::eval::{
    evaluate_compiled, suffixed_bucket, AttributeValue, EvalContext, EvalReason, Evaluation,
};
use crate::services::{DeliveredFlag, EnvironmentFlags, FlagService};

/// Request-body cap for every evaluation endpoint. A context is a key and a
/// few dozen short attributes; 16 KiB is generous.
pub const EVALUATION_BODY_LIMIT: usize = 16 * 1024;
/// Most attributes one request may send.
pub const MAX_CONTEXT_ATTRIBUTES: usize = 50;
/// Longest attribute name, string attribute value and bucketing key.
pub const MAX_ATTRIBUTE_NAME_LEN: usize = 64;
pub const MAX_ATTRIBUTE_VALUE_LEN: usize = 512;
pub const MAX_SUBJECT_KEY_LEN: usize = 256;
/// Most keys a caller may restrict a bulk evaluation to.
pub const MAX_REQUESTED_KEYS: usize = 200;

/// Per-project ceiling on the unauthenticated endpoints. A browser calls once
/// per page load, so this admits ~500 page loads per second per project
/// while bounding what a flood costs. The packed counter caps at 65 535.
pub const PROJECT_LIMIT_PER_MINUTE: u32 = 30_000;
/// Per-address ceiling on the unauthenticated endpoints.
pub const ADDRESS_LIMIT_PER_MINUTE: u32 = 600;

pub struct PublicFlagsState {
    pub flag_service: Arc<FlagService>,
    pub route_table: Arc<CachedPeerTable>,
    /// In-memory GeoIP lookup for the implicit `country` attribute. `None`
    /// when the geo plugin is not registered; the attribute is then absent.
    pub geoip: Option<Arc<GeoIpService>>,
    pub project_limiter: SlotRateLimiter,
    pub address_limiter: SlotRateLimiter,
}

pub fn configure_public_routes() -> Router<Arc<PublicFlagsState>> {
    Router::new()
        .route("/_temps/flags", post(evaluate_public_flags))
        .route("/_temps/ofrep/v1/evaluate/flags", post(ofrep_public_bulk))
        .route(
            "/_temps/ofrep/v1/evaluate/flags/{key}",
            post(ofrep_public_single),
        )
        .layer(axum::extract::DefaultBodyLimit::max(EVALUATION_BODY_LIMIT))
}

// =============================================================================
// Wire types
// =============================================================================

/// Body of `POST /api/_temps/flags`. Every field is optional; an empty body
/// evaluates every client-visible flag for the visitor the request carries.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PublicFlagsRequest {
    /// Bucketing subject, e.g. a signed-in user id. Defaults to the Temps
    /// visitor cookie when the request carries one.
    #[serde(default)]
    pub key: Option<String>,
    /// Targeting attributes. Override the implicit ones of the same name.
    #[serde(default)]
    pub attributes: HashMap<String, AttributeValue>,
    /// Evaluate only these flags. Omit for all client-visible flags.
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}

/// One evaluated flag, as a browser sees it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicFlagEvaluation {
    pub key: String,
    pub value: serde_json::Value,
    pub variant: Option<String>,
    pub reason: EvalReason,
    /// Whether the app should record a `flag_exposure` analytics event when
    /// it reads this flag. Decided here, per subject, from the flag's
    /// exposure sample rate, so sampling is stable for a visitor and costs
    /// the browser nothing.
    pub record_exposure: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicFlagsResponse {
    pub flags: Vec<PublicFlagEvaluation>,
}

/// OFREP evaluation request: `{ "context": { "targetingKey": "...", ... } }`.
///
/// `context` is an OpenFeature evaluation context, arbitrary JSON by
/// definition. Scalar entries become targeting attributes; nested objects and
/// arrays are ignored.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct OfrepEvaluationRequest {
    #[serde(default)]
    pub context: Option<serde_json::Map<String, serde_json::Value>>,
}

/// OFREP metadata. Always empty today; present because the protocol defines
/// it.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct OfrepMetadata {}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OfrepEvaluationSuccess {
    pub key: String,
    pub value: serde_json::Value,
    /// `STATIC`, `TARGETING_MATCH`, `SPLIT`, `DISABLED`, `DEFAULT` or `ERROR`.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub metadata: OfrepMetadata,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OfrepEvaluationFailure {
    pub key: String,
    /// `FLAG_NOT_FOUND`, `INVALID_CONTEXT`, `PARSE_ERROR` or `GENERAL`.
    #[serde(rename = "errorCode")]
    pub error_code: String,
    #[serde(rename = "errorDetails")]
    pub error_details: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OfrepBulkResponse {
    pub flags: Vec<OfrepEvaluationSuccess>,
}

// =============================================================================
// Shared evaluation
// =============================================================================

/// Reject a context the endpoints will not evaluate. Bounded so a public
/// caller cannot make one request expensive.
pub fn check_context(ctx: &EvalContext) -> Result<(), String> {
    if ctx.attributes.len() > MAX_CONTEXT_ATTRIBUTES {
        return Err(format!(
            "{} attributes sent, maximum is {MAX_CONTEXT_ATTRIBUTES}",
            ctx.attributes.len()
        ));
    }
    if let Some(key) = &ctx.key {
        if key.len() > MAX_SUBJECT_KEY_LEN {
            return Err(format!(
                "key is {} characters, maximum is {MAX_SUBJECT_KEY_LEN}",
                key.len()
            ));
        }
    }
    for (name, value) in &ctx.attributes {
        if name.is_empty() || name.len() > MAX_ATTRIBUTE_NAME_LEN {
            return Err(format!(
                "attribute names must be 1-{MAX_ATTRIBUTE_NAME_LEN} characters"
            ));
        }
        if let AttributeValue::String(text) = value {
            if text.len() > MAX_ATTRIBUTE_VALUE_LEN {
                return Err(format!(
                    "attribute '{name}' is {} characters, maximum is {MAX_ATTRIBUTE_VALUE_LEN}",
                    text.len()
                ));
            }
        }
    }
    Ok(())
}

/// Whether this subject's reads of this flag should be recorded as exposures.
/// Deterministic per subject so a visitor is either sampled or not, on every
/// page; independent of the rollout and split buckets.
pub fn should_record_exposure(flag: &DeliveredFlag, subject: Option<&str>) -> bool {
    let rate = flag.exposure_sample_rate;
    // NaN never passes: the column has a CHECK constraint, but a cached
    // snapshot is not the place to trust it.
    if rate.is_nan() || rate <= 0.0 {
        return false;
    }
    let Some(subject) = subject.filter(|s| !s.is_empty()) else {
        return false;
    };
    if rate >= 1.0 {
        return true;
    }
    suffixed_bucket(
        &flag.snapshot.key,
        "#exposure",
        &flag.snapshot.salt,
        subject,
    ) < rate * 100.0
}

/// Evaluate the flags of an environment a caller may see.
pub fn evaluate_visible<'a>(
    environment: &'a EnvironmentFlags,
    client_only: bool,
    keys: Option<&[String]>,
    ctx: &EvalContext,
) -> Vec<(&'a DeliveredFlag, Evaluation)> {
    let visible = |flag: &DeliveredFlag| !client_only || flag.client_visible;
    let chosen: Vec<&DeliveredFlag> = match keys {
        Some(keys) => keys
            .iter()
            .filter_map(|key| environment.get(key))
            .filter(|flag| visible(flag))
            .collect(),
        None => environment
            .flags
            .iter()
            .filter(|flag| visible(flag))
            .collect(),
    };
    chosen
        .into_iter()
        .map(|flag| (flag, evaluate_compiled(&flag.snapshot, &flag.patterns, ctx)))
        .collect()
}

fn ofrep_reason(reason: &EvalReason) -> &'static str {
    match reason {
        EvalReason::RuleMatch { .. } => "TARGETING_MATCH",
        EvalReason::PercentageRollout { .. } => "SPLIT",
        EvalReason::Disabled => "DISABLED",
        EvalReason::EnvironmentValue => "STATIC",
        EvalReason::Default => "DEFAULT",
        EvalReason::Error => "ERROR",
        EvalReason::FlagNotFound => "ERROR",
    }
}

fn ofrep_success(key: &str, evaluation: Evaluation) -> OfrepEvaluationSuccess {
    OfrepEvaluationSuccess {
        key: key.to_string(),
        reason: ofrep_reason(&evaluation.reason).to_string(),
        value: evaluation.value,
        variant: evaluation.variant,
        metadata: OfrepMetadata::default(),
    }
}

fn ofrep_failure(status: StatusCode, key: &str, code: &str, details: String) -> Response {
    (
        status,
        Json(OfrepEvaluationFailure {
            key: key.to_string(),
            error_code: code.to_string(),
            error_details: details,
        }),
    )
        .into_response()
}

/// Turn an OFREP context into an evaluation context. `targetingKey` is the
/// bucketing subject; other scalar entries are attributes.
fn ofrep_context(request: &OfrepEvaluationRequest) -> EvalContext {
    let mut ctx = EvalContext::default();
    let Some(context) = &request.context else {
        return ctx;
    };
    for (name, value) in context {
        if name == "targetingKey" {
            if let Some(key) = value.as_str() {
                ctx.key = Some(key.to_string());
            }
            continue;
        }
        let attribute = match value {
            serde_json::Value::Bool(b) => AttributeValue::Bool(*b),
            serde_json::Value::Number(n) => match n.as_f64() {
                Some(n) => AttributeValue::Number(n),
                None => continue,
            },
            serde_json::Value::String(s) => AttributeValue::String(s.clone()),
            serde_json::Value::Null
            | serde_json::Value::Array(_)
            | serde_json::Value::Object(_) => continue,
        };
        ctx.attributes.insert(name.clone(), attribute);
    }
    ctx
}

fn parse_body<T: for<'de> Deserialize<'de> + Default>(body: &Bytes) -> Result<T, String> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| e.to_string())
}

/// Bulk OFREP results depend on the context as much as on the definitions,
/// so the tag covers both: the same tag means the same answer.
///
/// Every field is length-prefixed, so no subject key can produce the same
/// bytes as a shorter key followed by an attribute, and an absent key is
/// distinct from an empty one.
fn bulk_etag(definitions: &str, ctx: &EvalContext) -> String {
    fn field(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    let mut hasher = Sha256::new();
    field(&mut hasher, definitions.as_bytes());
    match ctx.key.as_deref() {
        Some(key) => {
            hasher.update([1u8]);
            field(&mut hasher, key.as_bytes());
        }
        None => hasher.update([0u8]),
    }
    let mut names: Vec<&String> = ctx.attributes.keys().collect();
    names.sort();
    hasher.update((names.len() as u64).to_be_bytes());
    for name in names {
        field(&mut hasher, name.as_bytes());
        let value = serde_json::to_vec(&ctx.attributes[name]).unwrap_or_default();
        field(&mut hasher, &value);
    }
    format!("\"{}\"", hex::encode(&hasher.finalize()[..16]))
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|raw| {
            raw.trim() == "*"
                || raw
                    .split(',')
                    .map(|c| c.trim())
                    .map(|c| c.strip_prefix("W/").unwrap_or(c))
                    .any(|c| c == etag)
        })
}

fn bulk_response(
    headers: &HeaderMap,
    etag: String,
    flags: Vec<OfrepEvaluationSuccess>,
) -> Response {
    if if_none_match(headers, &etag) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = Json(OfrepBulkResponse { flags }).into_response();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// Results are per visitor: never let a shared cache store them.
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// =============================================================================
// Same-origin (unauthenticated) endpoints
// =============================================================================

/// The tenant and implicit context of a same-origin request.
struct PublicScope {
    environment: Arc<EnvironmentFlags>,
    implicit: EvalContext,
}

fn public_problem(status: StatusCode, title: &str, detail: String) -> Response {
    no_store(
        problemdetails::new(status)
            .with_title(title)
            .with_detail(detail)
            .into_response(),
    )
}

/// Resolve the tenant from `Host`, apply the rate limits, load the
/// environment's flags from memory and build the implicit context.
///
/// `Err` is the response to send, boxed because an `http::Response` dwarfs
/// the `Ok` variant (`clippy::result_large_err`).
async fn resolve_public_scope(
    state: &PublicFlagsState,
    metadata: &RequestMetadata,
) -> Result<PublicScope, Box<Response>> {
    let host = metadata.host.as_str();
    let route = state.route_table.get_route(host);
    let (Some(project), Some(environment)) = (
        route.as_ref().and_then(|r| r.project.clone()),
        route.as_ref().and_then(|r| r.environment.clone()),
    ) else {
        debug!(host = %host, "Flag evaluation for a host no environment serves");
        return Err(Box::new(public_problem(
            StatusCode::NOT_FOUND,
            "Unknown Host",
            format!("No Temps environment serves host '{host}'."),
        )));
    };
    let deployment_id = route
        .as_ref()
        .and_then(|r| r.deployment.as_ref())
        .map(|d| d.id);

    if !state.address_limiter.check(
        SlotRateLimiter::owner_for_address(&metadata.ip_address),
        ADDRESS_LIMIT_PER_MINUTE,
    ) || !state
        .project_limiter
        .check(project.id as u32, PROJECT_LIMIT_PER_MINUTE)
    {
        warn!(
            project_id = project.id,
            host = %host,
            "Feature flag evaluation rate limit exceeded"
        );
        let mut response = public_problem(
            StatusCode::TOO_MANY_REQUESTS,
            "Flag Evaluation Rate Limit Exceeded",
            format!(
                "Flag evaluation for host '{host}' exceeded {ADDRESS_LIMIT_PER_MINUTE} requests \
                 per minute per address or {PROJECT_LIMIT_PER_MINUTE} per project."
            ),
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return Err(Box::new(response));
    }

    let flags = state
        .flag_service
        .environment_flags(project.id, environment.id)
        .await
        .map_err(|error| {
            warn!(
                project_id = project.id,
                environment_id = environment.id,
                %error,
                "Failed to load feature flags for same-origin evaluation"
            );
            public_problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "Flags Unavailable",
                format!(
                    "Feature flags for host '{host}' could not be loaded; serve your fallbacks."
                ),
            )
        })?;

    let implicit = implicit_context(
        metadata,
        host,
        environment.id,
        &environment.slug,
        deployment_id,
        state.geoip.as_deref(),
    )
    .await;

    Ok(PublicScope {
        environment: flags,
        implicit,
    })
}

/// The attributes a same-origin request carries without the page sending
/// them: environment, deployment, host, visitor and session cookies, browser,
/// OS and device from the user agent, and (with GeoIP) country. Also used by
/// the surveys endpoint, so a survey's flag condition evaluates exactly as
/// the flag does on `/api/_temps/flags`.
pub async fn implicit_context(
    metadata: &RequestMetadata,
    host: &str,
    environment_id: i32,
    environment_slug: &str,
    deployment_id: Option<i32>,
    geoip: Option<&GeoIpService>,
) -> EvalContext {
    let mut implicit = EvalContext {
        key: metadata.visitor_id_cookie.clone(),
        attributes: HashMap::new(),
    };
    let attributes = &mut implicit.attributes;
    attributes.insert(
        "environment".into(),
        AttributeValue::String(environment_slug.to_string()),
    );
    attributes.insert(
        "environment_id".into(),
        AttributeValue::Number(f64::from(environment_id)),
    );
    if let Some(id) = deployment_id {
        attributes.insert(
            "deployment_id".into(),
            AttributeValue::Number(f64::from(id)),
        );
    }
    attributes.insert("host".into(), AttributeValue::String(host.to_string()));
    if let Some(visitor) = &metadata.visitor_id_cookie {
        attributes.insert("visitor_id".into(), AttributeValue::String(visitor.clone()));
    }
    if let Some(session) = &metadata.session_id_cookie {
        attributes.insert("session_id".into(), AttributeValue::String(session.clone()));
    }
    if !metadata.user_agent.is_empty() {
        if let Some(parsed) = woothee::parser::Parser::new().parse(&metadata.user_agent) {
            if parsed.name != woothee::woothee::VALUE_UNKNOWN {
                attributes.insert(
                    "browser".into(),
                    AttributeValue::String(parsed.name.to_string()),
                );
            }
            if parsed.os != woothee::woothee::VALUE_UNKNOWN {
                attributes.insert("os".into(), AttributeValue::String(parsed.os.to_string()));
            }
            if parsed.category != woothee::woothee::VALUE_UNKNOWN {
                attributes.insert(
                    "device_type".into(),
                    AttributeValue::String(parsed.category.to_string()),
                );
            }
        }
    }
    if let (Some(geoip), Ok(address)) = (geoip, metadata.ip_address.parse::<IpAddr>()) {
        if let Ok(location) = geoip.geolocate(address).await {
            if let Some(code) = location.country_code {
                attributes.insert("country".into(), AttributeValue::String(code));
            }
        }
    }
    implicit
}

/// Explicit context over implicit (ADR 034 R2.5).
pub fn merge(mut implicit: EvalContext, explicit: EvalContext) -> EvalContext {
    if let Some(key) = explicit.key.filter(|k| !k.is_empty()) {
        implicit.key = Some(key);
    }
    implicit.attributes.extend(explicit.attributes);
    implicit
}

/// Evaluate every client-visible flag for the visitor making the request.
///
/// Unauthenticated: the project and environment come from the `Host` the
/// browser called, never from the body. Only flags marked `client_visible`
/// are evaluated or mentioned; a server-only flag is indistinguishable from
/// one that does not exist.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/_temps/flags",
    request_body = PublicFlagsRequest,
    responses(
        (status = 200, description = "Evaluated client-visible flags", body = PublicFlagsResponse),
        (status = 400, description = "Malformed or oversized context"),
        (status = 404, description = "No environment serves this host"),
        (status = 429, description = "Rate limit exceeded"),
        (status = 503, description = "Flags could not be loaded")
    )
)]
pub async fn evaluate_public_flags(
    State(state): State<Arc<PublicFlagsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    body: Bytes,
) -> Response {
    let request: PublicFlagsRequest = match parse_body(&body) {
        Ok(request) => request,
        Err(reason) => {
            return public_problem(
                StatusCode::BAD_REQUEST,
                "Malformed Flag Request",
                format!("The flag evaluation body is not valid: {reason}"),
            )
        }
    };
    if request
        .keys
        .as_ref()
        .is_some_and(|k| k.len() > MAX_REQUESTED_KEYS)
    {
        return public_problem(
            StatusCode::BAD_REQUEST,
            "Too Many Keys",
            format!("At most {MAX_REQUESTED_KEYS} flag keys may be requested at once."),
        );
    }
    let explicit = EvalContext {
        key: request.key,
        attributes: request.attributes,
    };
    if let Err(reason) = check_context(&explicit) {
        return public_problem(StatusCode::BAD_REQUEST, "Invalid Flag Context", reason);
    }

    let scope = match resolve_public_scope(&state, &metadata).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let ctx = merge(scope.implicit, explicit);
    let subject = ctx.key.clone();

    let flags = evaluate_visible(&scope.environment, true, request.keys.as_deref(), &ctx)
        .into_iter()
        .map(|(flag, evaluation)| PublicFlagEvaluation {
            key: flag.snapshot.key.clone(),
            record_exposure: should_record_exposure(flag, subject.as_deref()),
            value: evaluation.value,
            variant: evaluation.variant,
            reason: evaluation.reason,
        })
        .collect();

    no_store(Json(PublicFlagsResponse { flags }).into_response())
}

/// OFREP bulk evaluation for OpenFeature web SDKs, same origin.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/_temps/ofrep/v1/evaluate/flags",
    request_body = OfrepEvaluationRequest,
    responses(
        (status = 200, description = "Evaluated client-visible flags", body = OfrepBulkResponse),
        (status = 304, description = "Unchanged for this context"),
        (status = 400, description = "Invalid context", body = OfrepEvaluationFailure),
        (status = 404, description = "No environment serves this host"),
        (status = 429, description = "Rate limit exceeded")
    )
)]
pub async fn ofrep_public_bulk(
    State(state): State<Arc<PublicFlagsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let explicit = match parse_ofrep(&body, "") {
        Ok(ctx) => ctx,
        Err(response) => return no_store(*response),
    };
    let scope = match resolve_public_scope(&state, &metadata).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let ctx = merge(scope.implicit, explicit);
    let flags = evaluate_visible(&scope.environment, true, None, &ctx)
        .into_iter()
        .map(|(flag, evaluation)| ofrep_success(&flag.snapshot.key, evaluation))
        .collect();
    no_store(bulk_response(
        &headers,
        bulk_etag(&scope.environment.client_etag, &ctx),
        flags,
    ))
}

/// OFREP single-flag evaluation, same origin.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/_temps/ofrep/v1/evaluate/flags/{key}",
    params(("key" = String, Path, description = "Flag key")),
    request_body = OfrepEvaluationRequest,
    responses(
        (status = 200, description = "Evaluated flag", body = OfrepEvaluationSuccess),
        (status = 400, description = "Invalid context", body = OfrepEvaluationFailure),
        (status = 404, description = "Flag not found or not client-visible", body = OfrepEvaluationFailure),
        (status = 429, description = "Rate limit exceeded")
    )
)]
pub async fn ofrep_public_single(
    State(state): State<Arc<PublicFlagsState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(key): Path<String>,
    body: Bytes,
) -> Response {
    let explicit = match parse_ofrep(&body, &key) {
        Ok(ctx) => ctx,
        Err(response) => return no_store(*response),
    };
    let scope = match resolve_public_scope(&state, &metadata).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let ctx = merge(scope.implicit, explicit);
    no_store(single_response(&scope.environment, true, &key, &ctx))
}

/// `Err` is the OFREP failure response, boxed like [`resolve_public_scope`]'s.
fn parse_ofrep(body: &Bytes, key: &str) -> Result<EvalContext, Box<Response>> {
    let request: OfrepEvaluationRequest = parse_body(body).map_err(|reason| {
        ofrep_failure(
            StatusCode::BAD_REQUEST,
            key,
            "PARSE_ERROR",
            format!("The OFREP request body is not valid JSON: {reason}"),
        )
    })?;
    let ctx = ofrep_context(&request);
    check_context(&ctx)
        .map_err(|reason| ofrep_failure(StatusCode::BAD_REQUEST, key, "INVALID_CONTEXT", reason))?;
    Ok(ctx)
}

fn single_response(
    environment: &EnvironmentFlags,
    client_only: bool,
    key: &str,
    ctx: &EvalContext,
) -> Response {
    let keys = [key.to_string()];
    match evaluate_visible(environment, client_only, Some(&keys), ctx)
        .into_iter()
        .next()
    {
        Some((flag, evaluation)) => {
            Json(ofrep_success(&flag.snapshot.key, evaluation)).into_response()
        }
        None => ofrep_failure(
            StatusCode::NOT_FOUND,
            key,
            "FLAG_NOT_FOUND",
            format!("Flag '{key}' does not exist in this environment."),
        ),
    }
}

// =============================================================================
// Server-side OFREP (deployment token)
// =============================================================================

/// The project and environment a deployment token is scoped to.
fn token_scope(auth: &temps_authz::AuthContext) -> Result<(i32, i32), Problem> {
    let project_id = auth.project_id().ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Deployment Token Required")
            .with_detail(
                "OFREP evaluation is scoped by a deployment token. Configure the OpenFeature \
                 provider with TEMPS_API_TOKEN as a bearer token.",
            )
    })?;
    let environment_id = auth
        .deployment_token_info()
        .and_then(|info| info.environment_id)
        .ok_or_else(|| Problem::from(FlagError::TokenNotEnvironmentScoped { project_id }))?;
    Ok((project_id, environment_id))
}

async fn token_environment(
    flag_service: &FlagService,
    auth: &temps_authz::AuthContext,
) -> Result<Arc<EnvironmentFlags>, Problem> {
    let (project_id, environment_id) = token_scope(auth)?;
    flag_service
        .environment_flags(project_id, environment_id)
        .await
        .map_err(|error| match error {
            FlagError::EnvironmentNotFound { .. } | FlagError::EnvironmentNotInProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Not Available")
                    .with_detail("The token's environment does not exist in the token's project.")
            }
            other => Problem::from(other),
        })
}

/// OFREP bulk evaluation for server-side OpenFeature SDKs.
///
/// Every flag of the token's environment, including server-only ones. Point
/// an OFREP provider at `TEMPS_API_URL` with `TEMPS_API_TOKEN` as its bearer
/// token.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/ofrep/v1/evaluate/flags",
    request_body = OfrepEvaluationRequest,
    responses(
        (status = 200, description = "Evaluated flags", body = OfrepBulkResponse),
        (status = 304, description = "Unchanged for this context"),
        (status = 400, description = "Invalid context or token not environment-scoped", body = OfrepEvaluationFailure),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
pub async fn ofrep_server_bulk(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<super::FlagsAppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Problem> {
    permission_guard!(auth, FlagsRead);
    let ctx = match parse_ofrep(&body, "") {
        Ok(ctx) => ctx,
        Err(response) => return Ok(*response),
    };
    let environment = token_environment(&state.flag_service, &auth).await?;
    let flags = evaluate_visible(&environment, false, None, &ctx)
        .into_iter()
        .map(|(flag, evaluation)| ofrep_success(&flag.snapshot.key, evaluation))
        .collect();
    Ok(bulk_response(
        &headers,
        bulk_etag(&environment.server_etag, &ctx),
        flags,
    ))
}

/// OFREP single-flag evaluation for server-side OpenFeature SDKs.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/ofrep/v1/evaluate/flags/{key}",
    params(("key" = String, Path, description = "Flag key")),
    request_body = OfrepEvaluationRequest,
    responses(
        (status = 200, description = "Evaluated flag", body = OfrepEvaluationSuccess),
        (status = 400, description = "Invalid context or token not environment-scoped", body = OfrepEvaluationFailure),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found", body = OfrepEvaluationFailure)
    ),
    security(("bearer_auth" = []))
)]
pub async fn ofrep_server_single(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<super::FlagsAppState>>,
    Path(key): Path<String>,
    body: Bytes,
) -> Result<Response, Problem> {
    permission_guard!(auth, FlagsRead);
    let ctx = match parse_ofrep(&body, &key) {
        Ok(ctx) => ctx,
        Err(response) => return Ok(*response),
    };
    let environment = token_environment(&state.flag_service, &auth).await?;
    Ok(single_response(&environment, false, &key, &ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{CompiledPatterns, FlagRule, FlagSnapshot, FlagValueType, RuleServe};

    fn delivered(key: &str, client_visible: bool, rate: f64) -> DeliveredFlag {
        DeliveredFlag {
            snapshot: FlagSnapshot {
                key: key.to_string(),
                value_type: FlagValueType::Bool,
                default_value: serde_json::json!(false),
                enabled: true,
                environment_value: None,
                salt: "a1b2c3".into(),
                variants: Vec::new(),
                rules: vec![FlagRule {
                    description: None,
                    conditions: vec![crate::eval::RuleCondition {
                        attribute: "country".into(),
                        operator: crate::eval::ConditionOperator::Eq,
                        value: Some(crate::eval::ConditionValue::Single(AttributeValue::String(
                            "DE".into(),
                        ))),
                    }],
                    rollout_percentage: None,
                    serve: RuleServe::Value {
                        value: serde_json::json!(true),
                    },
                }],
            },
            patterns: CompiledPatterns::default(),
            client_visible,
            exposure_sample_rate: rate,
        }
    }

    fn environment() -> EnvironmentFlags {
        EnvironmentFlags::new(
            1,
            10,
            vec![
                delivered("public.flag", true, 0.0),
                delivered("server.only", false, 0.0),
            ],
        )
    }

    /// R5.3: a server-only flag must be indistinguishable from a missing one
    /// on the unauthenticated surface, in bulk and by name.
    #[test]
    fn client_surfaces_never_evaluate_server_only_flags() {
        let env = environment();
        let ctx = EvalContext::default();

        let bulk: Vec<String> = evaluate_visible(&env, true, None, &ctx)
            .into_iter()
            .map(|(flag, _)| flag.snapshot.key.clone())
            .collect();
        assert_eq!(bulk, vec!["public.flag".to_string()]);

        let named = evaluate_visible(&env, true, Some(&["server.only".to_string()]), &ctx);
        assert!(named.is_empty());

        let response = single_response(&env, true, "server.only", &ctx);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = single_response(&env, true, "never.existed", &ctx);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn server_surfaces_see_every_flag() {
        let env = environment();
        let all = evaluate_visible(&env, false, None, &EvalContext::default());
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn explicit_attributes_override_implicit_ones() {
        let mut implicit = EvalContext {
            key: Some("cookie-visitor".into()),
            ..EvalContext::default()
        };
        implicit
            .attributes
            .insert("country".into(), AttributeValue::String("US".into()));
        let mut explicit = EvalContext {
            key: Some("user-42".into()),
            ..EvalContext::default()
        };
        explicit
            .attributes
            .insert("country".into(), AttributeValue::String("DE".into()));

        let merged = merge(implicit, explicit);
        assert_eq!(merged.key.as_deref(), Some("user-42"));
        assert_eq!(
            merged.attributes.get("country"),
            Some(&AttributeValue::String("DE".into()))
        );
    }

    #[test]
    fn an_empty_explicit_key_does_not_erase_the_visitor() {
        let implicit = EvalContext {
            key: Some("cookie-visitor".into()),
            attributes: HashMap::new(),
        };
        let explicit = EvalContext {
            key: Some(String::new()),
            attributes: HashMap::new(),
        };
        assert_eq!(
            merge(implicit, explicit).key.as_deref(),
            Some("cookie-visitor")
        );
    }

    #[test]
    fn oversized_contexts_are_rejected() {
        let mut ctx = EvalContext::default();
        for i in 0..=MAX_CONTEXT_ATTRIBUTES {
            ctx.attributes
                .insert(format!("a{i}"), AttributeValue::Bool(true));
        }
        assert!(check_context(&ctx).is_err());

        let long_value = EvalContext {
            key: None,
            attributes: HashMap::from([(
                "a".to_string(),
                AttributeValue::String("x".repeat(MAX_ATTRIBUTE_VALUE_LEN + 1)),
            )]),
        };
        assert!(check_context(&long_value).is_err());

        let long_key = EvalContext {
            key: Some("k".repeat(MAX_SUBJECT_KEY_LEN + 1)),
            attributes: HashMap::new(),
        };
        assert!(check_context(&long_key).is_err());
    }

    #[test]
    fn exposure_is_off_by_default_and_needs_a_subject() {
        assert!(!should_record_exposure(
            &delivered("f", true, 0.0),
            Some("v1")
        ));
        assert!(!should_record_exposure(&delivered("f", true, 1.0), None));
        assert!(should_record_exposure(
            &delivered("f", true, 1.0),
            Some("v1")
        ));
    }

    #[test]
    fn exposure_sampling_is_stable_and_proportional() {
        let flag = delivered("f", true, 0.1);
        let sampled = (0..20_000)
            .filter(|i| should_record_exposure(&flag, Some(&format!("v{i}"))))
            .count();
        let pct = sampled as f64 / 20_000.0 * 100.0;
        assert!((9.0..11.0).contains(&pct), "10% sampling produced {pct}%");
        assert_eq!(
            should_record_exposure(&flag, Some("v7")),
            should_record_exposure(&flag, Some("v7"))
        );
    }

    #[test]
    fn ofrep_context_maps_targeting_key_and_scalars() {
        let request: OfrepEvaluationRequest = serde_json::from_value(serde_json::json!({
            "context": {
                "targetingKey": "user-1",
                "country": "DE",
                "seats": 5,
                "beta": true,
                "nested": {"ignored": 1},
                "list": [1, 2]
            }
        }))
        .unwrap();
        let ctx = ofrep_context(&request);
        assert_eq!(ctx.key.as_deref(), Some("user-1"));
        assert_eq!(ctx.attributes.len(), 3);
        assert_eq!(ctx.attributes["seats"], AttributeValue::Number(5.0));
    }

    #[test]
    fn ofrep_reasons_follow_the_protocol_vocabulary() {
        assert_eq!(
            ofrep_reason(&EvalReason::RuleMatch { index: 0 }),
            "TARGETING_MATCH"
        );
        assert_eq!(
            ofrep_reason(&EvalReason::PercentageRollout { index: 0 }),
            "SPLIT"
        );
        assert_eq!(ofrep_reason(&EvalReason::EnvironmentValue), "STATIC");
        assert_eq!(ofrep_reason(&EvalReason::Disabled), "DISABLED");
        assert_eq!(ofrep_reason(&EvalReason::Default), "DEFAULT");
    }

    #[test]
    fn the_bulk_etag_changes_with_the_context() {
        let a = EvalContext {
            key: Some("u1".into()),
            attributes: HashMap::new(),
        };
        let b = EvalContext {
            key: Some("u2".into()),
            attributes: HashMap::new(),
        };
        assert_eq!(bulk_etag("d", &a), bulk_etag("d", &a));
        assert_ne!(bulk_etag("d", &a), bulk_etag("d", &b));
        assert_ne!(bulk_etag("d", &a), bulk_etag("e", &a));
    }

    /// A key that embeds what looks like an encoded attribute must not share
    /// a tag with the shorter key plus that attribute.
    #[test]
    fn the_bulk_etag_frames_every_field() {
        let mut attributes = HashMap::new();
        attributes.insert("plan".to_string(), AttributeValue::String("pro".into()));
        let split = EvalContext {
            key: Some("u".into()),
            attributes,
        };
        let joined = EvalContext {
            key: Some("u\u{0}plan\u{1}\"pro\"".into()),
            attributes: HashMap::new(),
        };
        assert_ne!(bulk_etag("d", &split), bulk_etag("d", &joined));

        let absent = EvalContext {
            key: None,
            attributes: HashMap::new(),
        };
        let empty = EvalContext {
            key: Some(String::new()),
            attributes: HashMap::new(),
        };
        assert_ne!(bulk_etag("d", &absent), bulk_etag("d", &empty));
    }

    #[test]
    fn empty_and_whitespace_bodies_are_an_empty_request() {
        let parsed: PublicFlagsRequest = parse_body(&Bytes::from_static(b"  ")).unwrap();
        assert!(parsed.key.is_none() && parsed.attributes.is_empty());
        assert!(parse_body::<PublicFlagsRequest>(&Bytes::from_static(b"{nope")).is_err());
    }
}
