//! Sample Rust app demonstrating Temps error tracking with source context.
//!
//! Deploy to Temps, enable "Error Tracking Source Context" in the project
//! settings, and hit GET /boom — the error shows up in Temps error tracking with
//! the actual source code around each stack frame.

use axum::{http::StatusCode, routing::get, Router};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    // SENTRY_DSN is auto-injected by Temps when error tracking is configured.
    //
    // `release` is read explicitly from SENTRY_RELEASE, which Temps injects with
    // the deployed commit SHA (see ADR-033) — this is what lets uploaded source
    // line up with the frames. `attach_stacktrace` makes errors carry frames.
    // The guard flushes on drop (kept alive for the whole program).
    let _guard = std::env::var("SENTRY_DSN").ok().map(|dsn| {
        sentry::init((
            dsn,
            sentry::ClientOptions {
                release: std::env::var("SENTRY_RELEASE").ok().map(Into::into),
                attach_stacktrace: true,
                ..Default::default()
            },
        ))
    });

    let app = Router::new()
        .route("/", get(|| async { "ok — try GET /boom to send an error to Temps" }))
        .route("/health", get(|| async { "ok" }))
        .route("/boom", get(boom));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Deliberately reports an error so you can see it — with source context — in
/// Temps error tracking.
async fn boom() -> (StatusCode, &'static str) {
    let err = do_work().unwrap_err();
    sentry::capture_error(err.as_ref());
    (StatusCode::INTERNAL_SERVER_ERROR, "boom reported to Temps")
}

fn do_work() -> Result<(), Box<dyn std::error::Error>> {
    Err("something broke deep in the call stack".into())
}
