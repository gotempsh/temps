//! LocalTemps API Server
//!
//! Creates an Axum server that provides SDK-compatible API endpoints.

use std::sync::Arc;

use axum::{
    middleware,
    routing::{delete, get, head, post},
    Router,
};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::context::{LocalTempsContext, DEFAULT_API_PORT, LOCAL_PROJECT_ID, LOCAL_TOKEN};
use crate::services::AnalyticsService;

use super::analytics::{self, AnalyticsState};
use super::auth::auth_middleware;
use super::autoinit::auto_init_middleware;
use super::blob;
use super::kv;

/// Health check response
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Services status response
#[derive(Serialize)]
pub struct ServicesResponse {
    pub services: Vec<crate::context::ServiceStatus>,
    pub api_url: String,
    pub token: String,
    pub project_id: i32,
}

/// Create the API server router
pub fn create_api_router(
    ctx: Arc<LocalTempsContext>,
    db: Option<Arc<DatabaseConnection>>,
) -> Router {
    // CORS configuration - allow all origins for local development
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Health check route (no auth required)
    let health_routes = Router::new().route("/health", get(health_handler));

    // KV routes (auth required)
    let kv_routes = Router::new()
        .route("/get", post(kv::get_handler))
        .route("/set", post(kv::set_handler))
        .route("/del", post(kv::del_handler))
        .route("/incr", post(kv::incr_handler))
        .route("/expire", post(kv::expire_handler))
        .route("/ttl", post(kv::ttl_handler))
        .route("/keys", post(kv::keys_handler));

    // Blob routes (auth required)
    // Note: Axum requires catch-all parameters to be at the end of the route
    let blob_routes = Router::new()
        .route("/", post(blob::upload_handler))
        .route("/", get(blob::list_handler))
        .route("/", delete(blob::delete_handler))
        .route("/copy", post(blob::copy_handler))
        .route("/{*path}", head(blob::head_handler))
        .route("/{*path}", get(blob::download_handler));

    // Services status route (no auth required - for UI)
    let services_routes = Router::new().route("/services", get(services_handler));

    // Build base router with KV and Blob routes
    let mut router = Router::new()
        .merge(health_routes)
        .merge(services_routes.with_state(ctx.clone()))
        .nest(
            "/api/kv",
            kv_routes
                .layer(middleware::from_fn(auth_middleware))
                .layer(middleware::from_fn_with_state(
                    ctx.clone(),
                    auto_init_middleware,
                ))
                .with_state(ctx.clone()),
        )
        .nest(
            "/api/blob",
            blob_routes
                .layer(middleware::from_fn(auth_middleware))
                .layer(middleware::from_fn_with_state(
                    ctx.clone(),
                    auto_init_middleware,
                ))
                .with_state(ctx.clone()),
        );

    // Add analytics routes if database is available
    if let Some(db) = db {
        let analytics_service = Arc::new(AnalyticsService::new(db));
        let analytics_state = Arc::new(AnalyticsState { analytics_service });

        // SDK endpoints (what @temps-sdk/react-analytics calls)
        let sdk_routes = Router::new()
            .route("/event", post(analytics::handle_event))
            .route("/speed", post(analytics::handle_speed))
            .route("/session-replay/init", post(analytics::handle_session_init))
            .route(
                "/session-replay/events",
                post(analytics::handle_session_events),
            )
            .with_state(analytics_state.clone());

        // Inspector endpoints (for UI to read data)
        let inspector_routes = Router::new()
            .route("/events", get(analytics::list_events))
            .route("/events", delete(analytics::clear_events))
            .route("/events/count", get(analytics::count_events))
            .route("/events/{id}", get(analytics::get_event))
            .with_state(analytics_state);

        router = router
            .nest("/api/_temps", sdk_routes)
            .nest("/api/inspector", inspector_routes);

        info!("Analytics routes enabled");
    } else {
        info!("Analytics routes disabled (database unavailable)");
    }

    router.layer(cors)
}

/// Health check handler
async fn health_handler() -> axum::Json<HealthResponse> {
    axum::Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Services status handler
async fn services_handler(
    axum::extract::State(ctx): axum::extract::State<Arc<LocalTempsContext>>,
) -> axum::Json<ServicesResponse> {
    let services = ctx.get_service_status().await;

    axum::Json(ServicesResponse {
        services,
        api_url: format!("http://localhost:{}", DEFAULT_API_PORT),
        token: LOCAL_TOKEN.to_string(),
        project_id: LOCAL_PROJECT_ID,
    })
}

/// Create and start the API server
pub async fn create_api_server(
    ctx: Arc<LocalTempsContext>,
    db: Option<Arc<DatabaseConnection>>,
    port: u16,
) -> anyhow::Result<()> {
    let router = create_api_router(ctx, db);

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("LocalTemps API server listening on http://{}", addr);
    info!("Token: {}", LOCAL_TOKEN);
    info!("Project ID: {}", LOCAL_PROJECT_ID);

    axum::serve(listener, router).await?;

    Ok(())
}
