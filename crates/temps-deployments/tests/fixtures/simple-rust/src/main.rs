//! Simple Axum web server for testing Nixpacks deployment
//!
//! This is a minimal Rust web application using Axum framework.

use axum::{
    extract::Json,
    response::Html,
    routing::get,
    Router,
};
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    framework: String,
    version: String,
    deployed_with: String,
    rust_version: String,
}

#[derive(Serialize)]
struct InfoResponse {
    name: String,
    version: String,
    description: String,
}

/// Root endpoint returning HTML
async fn root() -> Html<&'static str> {
    Html(
        r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Nixpacks + Rust</title>
            <style>
                body {
                    font-family: system-ui, sans-serif;
                    max-width: 800px;
                    margin: 0 auto;
                    padding: 2rem;
                    line-height: 1.6;
                }
                h1 { color: #CE422B; }
                .info-box {
                    background: #f5f5f5;
                    padding: 1rem;
                    border-radius: 8px;
                    margin-top: 2rem;
                }
                a { color: #CE422B; }
            </style>
        </head>
        <body>
            <h1>🦀 Hello from Nixpacks + Rust!</h1>
            <p>This is a simple Axum web server deployed using Nixpacks auto-detection.</p>
            <div class="info-box">
                <h2>Deployment Info</h2>
                <ul>
                    <li><strong>Framework:</strong> Axum 0.7</li>
                    <li><strong>Language:</strong> Rust</li>
                    <li><strong>Deployed with:</strong> Nixpacks</li>
                    <li><strong>Status:</strong> ✅ Running</li>
                </ul>
            </div>
            <div style="margin-top: 1rem;">
                <a href="/health">Check Health API →</a>
            </div>
        </body>
        </html>
        "#,
    )
}

/// Health check endpoint
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        framework: "Axum".to_string(),
        version: "0.7".to_string(),
        deployed_with: "nixpacks".to_string(),
        rust_version: env!("CARGO_PKG_RUST_VERSION").to_string(),
    })
}

/// Application info endpoint
async fn info() -> Json<InfoResponse> {
    Json(InfoResponse {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Simple Rust/Axum app for Nixpacks testing".to_string(),
    })
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Build router
    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/info", get(info));

    // Get port from environment or use default
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3000);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!("Server starting on http://{}", addr);

    // Run server
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind to address");

    axum::serve(listener, app)
        .await
        .expect("Server failed to start");
}
