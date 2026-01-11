//! Auto-initialization middleware
//!
//! This middleware automatically initializes services on first API call,
//! providing a zero-config developer experience.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::{error, info};

use crate::context::LocalTempsContext;

/// Middleware that auto-initializes services before processing requests
pub async fn auto_init_middleware(
    State(ctx): State<Arc<LocalTempsContext>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Try to ensure services are initialized
    if let Err(e) = ctx.ensure_initialized().await {
        error!("Auto-initialization failed: {}", e);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "Services not available. Please ensure Docker is running.\nError: {}",
                e
            ),
        )
            .into_response();
    }

    // Log first-time initialization
    static LOGGED_INIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED_INIT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        info!("Services auto-initialized on first API call");
    }

    // Continue with the request
    next.run(request).await
}
