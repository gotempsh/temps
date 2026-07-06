//! Edge analytics query API (Axum).
//!
//! Exposes REST endpoints that the origin Temps API calls to query
//! per-asset, per-domain, time-series analytics from the local SQLite store.
//! Protected by bearer token auth (same token used for origin registration).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::analytics::AnalyticsStore;
use crate::cache::EdgeCache;

/// Shared state for the edge API.
pub struct EdgeApiState {
    pub analytics: Arc<AnalyticsStore>,
    pub cache: Arc<EdgeCache>,
}

/// Bearer token auth state.
#[derive(Clone)]
struct TokenAuth {
    token_hash: String,
}

impl TokenAuth {
    fn new(token: &str) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        Self {
            token_hash: hex::encode(digest),
        }
    }

    fn verify(&self, provided: &str) -> bool {
        let digest = Sha256::digest(provided.as_bytes());
        let provided_hash = hex::encode(digest);
        constant_time_eq(self.token_hash.as_bytes(), provided_hash.as_bytes())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Bearer token auth middleware.
async fn require_auth(request: axum::extract::Request, next: Next) -> axum::response::Response {
    let auth = request.extensions().get::<Arc<TokenAuth>>().cloned();
    let auth = match auth {
        Some(a) => a,
        None => return (StatusCode::INTERNAL_SERVER_ERROR, "Auth not configured").into_response(),
    };

    let header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match header {
        Some(h) if h.starts_with("Bearer ") => {
            if auth.verify(&h[7..]) {
                next.run(request).await
            } else {
                (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
            }
        }
        _ => (StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response(),
    }
}

/// Common query params for time-range filters.
#[derive(Deserialize)]
pub struct TimeRangeParams {
    /// ISO 8601 start time (default: 24h ago)
    pub since: Option<String>,
    /// ISO 8601 end time (default: now)
    pub until: Option<String>,
}

impl TimeRangeParams {
    fn since_or_default(&self) -> String {
        self.since
            .clone()
            .unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339())
    }
    fn until_or_default(&self) -> String {
        self.until
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    }
}

#[derive(Deserialize)]
pub struct DomainParams {
    #[serde(flatten)]
    pub range: TimeRangeParams,
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
pub struct AssetParams {
    #[serde(flatten)]
    pub range: TimeRangeParams,
    pub domain: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Deserialize)]
pub struct TimeseriesParams {
    #[serde(flatten)]
    pub range: TimeRangeParams,
    /// Bucket size in minutes (default: 5)
    pub bucket: Option<u32>,
}

/// Build the edge analytics API router.
pub fn build_router(analytics: Arc<AnalyticsStore>, cache: Arc<EdgeCache>, token: &str) -> Router {
    let state = Arc::new(EdgeApiState { analytics, cache });
    let auth = Arc::new(TokenAuth::new(token));

    Router::new()
        .route("/edge/analytics/overview", get(overview))
        .route("/edge/analytics/domains", get(domains))
        .route("/edge/analytics/assets", get(top_assets))
        .route("/edge/analytics/timeseries", get(timeseries))
        .route("/edge/health", get(health))
        .route("/edge/cache/stats", get(cache_stats))
        .layer(middleware::from_fn(require_auth))
        .layer(Extension(auth))
        .with_state(state)
}

// -----------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------

async fn overview(
    State(state): State<Arc<EdgeApiState>>,
    Query(params): Query<TimeRangeParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let result = state
        .analytics
        .query_overview(&params.since_or_default(), &params.until_or_default())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query failed: {}", e),
            )
        })?;

    Ok(Json(result))
}

async fn domains(
    State(state): State<Arc<EdgeApiState>>,
    Query(params): Query<DomainParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(50).min(200);
    let result = state
        .analytics
        .query_domains(
            &params.range.since_or_default(),
            &params.range.until_or_default(),
            limit,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query failed: {}", e),
            )
        })?;

    Ok(Json(result))
}

async fn top_assets(
    State(state): State<Arc<EdgeApiState>>,
    Query(params): Query<AssetParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(50).min(500);
    let result = state
        .analytics
        .query_top_assets(
            &params.range.since_or_default(),
            &params.range.until_or_default(),
            limit,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query failed: {}", e),
            )
        })?;

    Ok(Json(result))
}

async fn timeseries(
    State(state): State<Arc<EdgeApiState>>,
    Query(params): Query<TimeseriesParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let bucket = params.bucket.unwrap_or(5).clamp(1, 1440);
    let result = state
        .analytics
        .query_timeseries(
            &params.range.since_or_default(),
            &params.range.until_or_default(),
            bucket,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Query failed: {}", e),
            )
        })?;

    Ok(Json(result))
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn cache_stats(State(state): State<Arc<EdgeApiState>>) -> impl IntoResponse {
    Json(state.cache.stats())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{self, AnalyticsStore};
    use crate::cache::EdgeCache;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    async fn setup() -> (Router, Arc<AnalyticsStore>) {
        let store = Arc::new(AnalyticsStore::open_in_memory().await.unwrap());
        let cache = Arc::new(EdgeCache::new(
            &PathBuf::from("/tmp/edge-test-cache"),
            1024 * 1024,
        ));

        let router = build_router(store.clone(), cache, "test-token");
        (router, store)
    }

    fn authed_get(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header("Authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap()
    }

    fn unauthed_get(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    // -----------------------------------------------------------------------
    // Auth tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_missing_auth_returns_401() {
        let (app, _) = setup().await;
        let resp = app.oneshot(unauthed_get("/edge/health")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_wrong_token_returns_401() {
        let (app, _) = setup().await;
        let req = Request::builder()
            .uri("/edge/health")
            .header("Authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_valid_auth_passes() {
        let (app, _) = setup().await;
        let resp = app.oneshot(authed_get("/edge/health")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
    }

    // -----------------------------------------------------------------------
    // Health & cache stats
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_health_endpoint() {
        let (app, _) = setup().await;
        let resp = app.oneshot(authed_get("/edge/health")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_cache_stats_endpoint() {
        let (app, _) = setup().await;
        let resp = app.oneshot(authed_get("/edge/cache/stats")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["hit_count"], 0);
        assert_eq!(json["miss_count"], 0);
    }

    // -----------------------------------------------------------------------
    // Analytics overview
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_overview_empty() {
        let (app, _) = setup().await;
        let resp = app
            .oneshot(authed_get("/edge/analytics/overview"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_requests"], 0);
        assert_eq!(json["cache_hit_rate"], 0.0);
    }

    #[tokio::test]
    async fn test_overview_with_data() {
        let (app, store) = setup().await;

        // Insert test events
        store
            .insert_batch(&[
                analytics::make_event(
                    "a.com", "/main.js", "GET", 200, "HIT", 5000, 0.0, None, true,
                ),
                analytics::make_event(
                    "a.com", "/app.js", "GET", 200, "MISS", 2000, 120.0, None, true,
                ),
                analytics::make_event("b.com", "/", "GET", 200, "BYPASS", 1000, 0.0, None, false),
            ])
            .await
            .unwrap();

        let resp = app
            .oneshot(authed_get("/edge/analytics/overview"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        assert_eq!(json["total_requests"], 3);
        assert_eq!(json["cache_hits"], 1);
        assert_eq!(json["cache_misses"], 1);
        assert_eq!(json["cache_bypasses"], 1);
        assert_eq!(json["bytes_from_cache"], 5000);
        assert_eq!(json["unique_domains"], 2);
    }

    // -----------------------------------------------------------------------
    // Domains endpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_domains_endpoint() {
        let (app, store) = setup().await;

        store
            .insert_batch(&[
                analytics::make_event("a.com", "/1.js", "GET", 200, "HIT", 100, 0.0, None, true),
                analytics::make_event("a.com", "/2.js", "GET", 200, "HIT", 200, 0.0, None, true),
                analytics::make_event("b.com", "/1.js", "GET", 200, "MISS", 300, 50.0, None, true),
            ])
            .await
            .unwrap();

        let resp = app
            .oneshot(authed_get("/edge/analytics/domains?limit=10"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["domain"], "a.com");
        assert_eq!(arr[0]["requests"], 2);
        assert_eq!(arr[1]["domain"], "b.com");
    }

    // -----------------------------------------------------------------------
    // Top assets endpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_top_assets_endpoint() {
        let (app, store) = setup().await;

        let mut events = Vec::new();
        for _ in 0..5 {
            events.push(analytics::make_event(
                "a.com", "/hot.js", "GET", 200, "HIT", 100, 0.0, None, true,
            ));
        }
        events.push(analytics::make_event(
            "a.com", "/cold.js", "GET", 200, "HIT", 100, 0.0, None, true,
        ));
        store.insert_batch(&events).await.unwrap();

        let resp = app
            .oneshot(authed_get("/edge/analytics/assets?limit=10"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr[0]["path"], "/hot.js");
        assert_eq!(arr[0]["requests"], 5);
    }

    // -----------------------------------------------------------------------
    // Timeseries endpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_timeseries_endpoint() {
        let (app, store) = setup().await;

        store
            .insert_batch(&[analytics::make_event(
                "a.com", "/x.js", "GET", 200, "HIT", 100, 0.0, None, true,
            )])
            .await
            .unwrap();

        let resp = app
            .oneshot(authed_get("/edge/analytics/timeseries?bucket=60"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert!(!arr.is_empty());
        assert_eq!(arr[0]["requests"], 1);
        assert_eq!(arr[0]["cache_hits"], 1);
    }

    // -----------------------------------------------------------------------
    // Full pipeline: write through handle → flush → query via API
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_full_pipeline_write_and_read() {
        let store = Arc::new(AnalyticsStore::open_in_memory().await.unwrap());
        let cache = Arc::new(EdgeCache::new(
            &PathBuf::from("/tmp/edge-pipeline-test"),
            1024 * 1024,
        ));

        // Create analytics handle
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let handle = crate::analytics::EdgeAnalyticsHandle { tx };

        // Record events through the handle
        handle.record(analytics::make_event(
            "test.com",
            "/_next/static/chunks/main.js",
            "GET",
            200,
            "HIT",
            50000,
            0.0,
            Some("ap-southeast"),
            true,
        ));
        handle.record(analytics::make_event(
            "test.com",
            "/_next/static/chunks/vendor.js",
            "GET",
            200,
            "MISS",
            120000,
            85.5,
            Some("ap-southeast"),
            true,
        ));
        handle.record(analytics::make_event(
            "test.com",
            "/",
            "GET",
            200,
            "BYPASS",
            3000,
            0.0,
            Some("ap-southeast"),
            false,
        ));

        // Drain the channel and insert (simulating the writer flush)
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert_eq!(events.len(), 3);
        store.insert_batch(&events).await.unwrap();

        // Now query via the API
        let router = build_router(store.clone(), cache, "pipeline-token");

        // Overview
        let req = Request::builder()
            .uri("/edge/analytics/overview")
            .header("Authorization", "Bearer pipeline-token")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["total_requests"], 3);
        assert_eq!(json["cache_hits"], 1);
        assert_eq!(json["cache_misses"], 1);
        assert_eq!(json["cache_bypasses"], 1);
        assert_eq!(json["bytes_from_cache"], 50000);
        assert_eq!(json["bytes_from_origin"], 120000);

        // Top assets
        let req = Request::builder()
            .uri("/edge/analytics/assets?limit=5")
            .header("Authorization", "Bearer pipeline-token")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        // 3 unique paths
        assert_eq!(arr.len(), 3);

        // Domains
        let req = Request::builder()
            .uri("/edge/analytics/domains")
            .header("Authorization", "Bearer pipeline-token")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["domain"], "test.com");
        assert_eq!(arr[0]["requests"], 3);
    }

    // -----------------------------------------------------------------------
    // Edge case: time range filtering
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_overview_with_time_range_filter() {
        let (app, store) = setup().await;

        // Insert event with known timestamp
        store
            .insert_batch(&[analytics::make_event(
                "a.com", "/x.js", "GET", 200, "HIT", 100, 0.0, None, true,
            )])
            .await
            .unwrap();

        // Query with future time range — should return 0
        let resp = app
            .oneshot(authed_get(
                "/edge/analytics/overview?since=2099-01-01T00%3A00%3A00Z&until=2099-12-31T00%3A00%3A00Z",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_requests"], 0);
    }
}
