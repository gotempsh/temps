// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Public webhook ingestion handler. Mounted OUTSIDE the authenticated
//! tree — the only thing that proves the request is legitimate is the
//! signing-secret verification performed by the provider adapter.
//!
//! Design notes:
//!   * No `RequireAuth` extractor — Stripe can't send a bearer token.
//!   * The `path_token` in the URL is unguessable (256 bits of entropy)
//!     but it is not the auth mechanism — it just routes the request
//!     to the right integration. Signature verification is what really
//!     authenticates the call.
//!   * Body is read as raw `Bytes` (not parsed as JSON) because the HMAC
//!     is computed over the exact byte sequence Stripe sent. We cap at
//!     1 MiB to keep a rogue sender from OOM-ing the server.
//!   * We always return 2xx (or 4xx for a broken secret) so Stripe does
//!     not retry forever. Duplicates and unknown event types count as
//!     success.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use bytes::Bytes;
use serde::Serialize;
use tracing::{error, warn};

use crate::error::RevenueError;
use crate::providers::ProviderError;
use crate::service::ingestion::IngestOutcome;
use crate::service::RevenueIngestionService;

/// 1 MiB. Stripe webhook payloads are a few kB; the cap is purely a
/// defensive guard.
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub struct PublicState {
    pub ingestion: Arc<RevenueIngestionService>,
}

impl PublicState {
    pub fn new(ingestion: Arc<RevenueIngestionService>) -> Self {
        Self { ingestion }
    }
}

#[derive(Debug, Serialize)]
struct WebhookAck {
    status: &'static str,
    ingested: usize,
}

/// Receive a provider webhook. URL shape:
/// `POST /webhooks/revenue/{provider}/{path_token}`
async fn receive_webhook(
    State(state): State<Arc<PublicState>>,
    Path((provider, path_token)): Path<(String, String)>,
    req: Request<Body>,
) -> impl IntoResponse {
    let (parts, body) = req.into_parts();
    let headers: HeaderMap = parts.headers;

    let body_bytes = match read_capped_body(body).await {
        Ok(b) => b,
        Err(status) => return status.into_response(),
    };

    match state
        .ingestion
        .ingest(&provider, &path_token, headers, body_bytes)
        .await
    {
        Ok(IngestOutcome::Ingested(n)) => (
            StatusCode::OK,
            Json(WebhookAck {
                status: "ingested",
                ingested: n,
            }),
        )
            .into_response(),
        Ok(IngestOutcome::Duplicate) => (
            StatusCode::OK,
            Json(WebhookAck {
                status: "duplicate",
                ingested: 0,
            }),
        )
            .into_response(),
        Ok(IngestOutcome::Ignored) => (
            StatusCode::ACCEPTED,
            Json(WebhookAck {
                status: "ignored",
                ingested: 0,
            }),
        )
            .into_response(),
        Err(err) => webhook_error_response(err),
    }
}

async fn read_capped_body(body: Body) -> Result<Bytes, StatusCode> {
    match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => Ok(b),
        Err(e) => {
            warn!(error = %e, "rejected oversized webhook body");
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        }
    }
}

fn webhook_error_response(err: RevenueError) -> axum::response::Response {
    match &err {
        // 404: wrong or rotated token — Stripe will eventually stop
        // retrying after enough failures.
        RevenueError::IntegrationNotFoundByToken | RevenueError::IntegrationNotFound { .. } => {
            StatusCode::NOT_FOUND.into_response()
        }
        // 410 Gone — disabled integration. Stripe interprets this as
        // "stop retrying".
        RevenueError::IntegrationDisabled { .. } => StatusCode::GONE.into_response(),
        // 400: mismatched provider in URL vs stored integration.
        RevenueError::ProviderMismatch { .. } => StatusCode::BAD_REQUEST.into_response(),
        // 400: unknown provider (e.g. old URL after we removed an adapter).
        RevenueError::UnknownProvider { .. } => StatusCode::BAD_REQUEST.into_response(),
        // Provider-layer errors map to the most appropriate HTTP code.
        RevenueError::Provider { source, .. } => provider_status(source).into_response(),
        // Internal — log and 500.
        other => {
            error!("webhook internal error: {}", other);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn provider_status(err: &ProviderError) -> StatusCode {
    match err {
        ProviderError::MissingHeader { .. }
        | ProviderError::MalformedHeader
        | ProviderError::MalformedPayload { .. }
        | ProviderError::ReplayExpired => StatusCode::BAD_REQUEST,
        ProviderError::InvalidSignature => StatusCode::UNAUTHORIZED,
    }
}

pub fn configure_public_routes() -> Router<Arc<PublicState>> {
    Router::new().route(
        "/webhooks/revenue/{provider}/{path_token}",
        post(receive_webhook),
    )
}
