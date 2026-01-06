//! KV API endpoints
//!
//! Implements SDK-compatible KV API endpoints using local services.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::context::{LocalTempsContext, LOCAL_PROJECT_ID};
use crate::services::kv::SetOptions;

/// GET request
#[derive(Deserialize)]
pub struct GetRequest {
    pub key: String,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// GET response
#[derive(Serialize)]
pub struct GetResponse {
    pub value: Option<Value>,
}

/// SET request
#[derive(Deserialize)]
pub struct SetRequest {
    pub key: String,
    pub value: Value,
    #[serde(default)]
    pub ex: Option<i64>,
    #[serde(default)]
    pub px: Option<i64>,
    #[serde(default)]
    pub nx: Option<bool>,
    #[serde(default)]
    pub xx: Option<bool>,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// SET response
#[derive(Serialize)]
pub struct SetResponse {
    pub result: Option<String>,
}

/// DEL request
#[derive(Deserialize)]
pub struct DelRequest {
    pub keys: Vec<String>,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// DEL response
#[derive(Serialize)]
pub struct DelResponse {
    pub deleted: i64,
}

/// INCR request
#[derive(Deserialize)]
pub struct IncrRequest {
    pub key: String,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// INCR response
#[derive(Serialize)]
pub struct IncrResponse {
    pub value: i64,
}

/// EXPIRE request
#[derive(Deserialize)]
pub struct ExpireRequest {
    pub key: String,
    pub seconds: i64,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// EXPIRE response
#[derive(Serialize)]
pub struct ExpireResponse {
    pub result: i32,
}

/// TTL request
#[derive(Deserialize)]
pub struct TtlRequest {
    pub key: String,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// TTL response
#[derive(Serialize)]
pub struct TtlResponse {
    pub ttl: i64,
}

/// KEYS request
#[derive(Deserialize)]
pub struct KeysRequest {
    pub pattern: String,
    #[serde(default)]
    pub project_id: Option<i32>,
}

/// KEYS response
#[derive(Serialize)]
pub struct KeysResponse {
    pub keys: Vec<String>,
}

/// Error response
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetails,
}

#[derive(Serialize)]
pub struct ErrorDetails {
    pub message: String,
    pub code: String,
}

fn error_response(status: StatusCode, message: &str, code: &str) -> impl IntoResponse {
    (
        status,
        Json(ErrorResponse {
            error: ErrorDetails {
                message: message.to_string(),
                code: code.to_string(),
            },
        }),
    )
}

/// GET endpoint - get value by key
pub async fn get_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<GetRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!("KV GET key={} project_id={}", request.key, project_id);

    match ctx.kv_service().get(project_id, &request.key).await {
        Ok(value) => (StatusCode::OK, Json(GetResponse { value })).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// SET endpoint - set value with optional expiration
pub async fn set_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<SetRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "KV SET key={} project_id={} ex={:?}",
        request.key, project_id, request.ex
    );

    let options = SetOptions {
        ex: request.ex,
        px: request.px,
        nx: request.nx.unwrap_or(false),
        xx: request.xx.unwrap_or(false),
    };

    match ctx
        .kv_service()
        .set(project_id, &request.key, request.value, options)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(SetResponse {
                result: Some("OK".to_string()),
            }),
        )
            .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// DEL endpoint - delete keys
pub async fn del_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<DelRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!("KV DEL keys={:?} project_id={}", request.keys, project_id);

    match ctx.kv_service().del(project_id, request.keys).await {
        Ok(deleted) => (StatusCode::OK, Json(DelResponse { deleted })).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// INCR endpoint - increment numeric value
pub async fn incr_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<IncrRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!("KV INCR key={} project_id={}", request.key, project_id);

    match ctx.kv_service().incr(project_id, &request.key).await {
        Ok(value) => (StatusCode::OK, Json(IncrResponse { value })).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// EXPIRE endpoint - set expiration on key
pub async fn expire_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<ExpireRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "KV EXPIRE key={} seconds={} project_id={}",
        request.key, request.seconds, project_id
    );

    match ctx
        .kv_service()
        .expire(project_id, &request.key, request.seconds)
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(ExpireResponse {
                result: if result { 1 } else { 0 },
            }),
        )
            .into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// TTL endpoint - get time to live
pub async fn ttl_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<TtlRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!("KV TTL key={} project_id={}", request.key, project_id);

    match ctx.kv_service().ttl(project_id, &request.key).await {
        Ok(ttl) => (StatusCode::OK, Json(TtlResponse { ttl })).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}

/// KEYS endpoint - find keys matching pattern
pub async fn keys_handler(
    State(ctx): State<Arc<LocalTempsContext>>,
    Json(request): Json<KeysRequest>,
) -> impl IntoResponse {
    let project_id = request.project_id.unwrap_or(LOCAL_PROJECT_ID);
    debug!(
        "KV KEYS pattern={} project_id={}",
        request.pattern, project_id
    );

    match ctx.kv_service().keys(project_id, &request.pattern).await {
        Ok(keys) => (StatusCode::OK, Json(KeysResponse { keys })).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &e.to_string(),
            "KV_ERROR",
        )
        .into_response(),
    }
}
