// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The client against a live HTTP backend.
//!
//! A stub server stands in for the managed backend so this suite can assert the
//! behaviour that matters to a self-hosted operator — what happens when the
//! backend is slow, wrong, unauthorised or simply gone — without depending on
//! the backend implementation.
//!
//! The governing property under test: **a failing backend never costs the
//! instance data or uptime.** It either succeeds, or it degrades to a state the
//! operator can see.

use std::net::SocketAddr;

use axum::{routing::post, Json, Router};
use temps_cloud_client::spool::Spool;
use temps_cloud_client::{BackendUrl, CloudClient, CloudError};
use temps_cloud_protocol::{SpanRecord, TelemetryBatch};
use uuid::Uuid;

/// Start a stub backend and return its base URL.
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn client(url: &str) -> CloudClient {
    CloudClient::new(BackendUrl::loopback_development(url).unwrap()).unwrap()
}

fn spans(n: usize) -> Vec<SpanRecord> {
    (0..n)
        .map(|i| SpanRecord {
            trace_id: "t".into(),
            span_id: format!("s{i}"),
            name: "GET /".into(),
            ts_millis: i as i64,
            duration_ms: 1.0,
            attributes: Default::default(),
            // ADR-040's queryable-fidelity fields stay absent: the transport
            // is fidelity-agnostic.
            ..Default::default()
        })
        .collect()
}

#[tokio::test]
async fn enrolling_with_a_good_code_yields_a_token() {
    let tenant = Uuid::new_v4();
    let url = serve(Router::new().route(
        "/v1/enroll",
        post(move || async move {
            Json(serde_json::json!({
                "tenant_id": tenant,
                "instance_token": "inst_abc123"
            }))
        }),
    ))
    .await;

    let got = client(&url)
        .enroll("abcd-2345", Uuid::new_v4(), "0.1.0")
        .await
        .expect("enrollment should succeed");

    assert_eq!(got.instance_token, "inst_abc123");
    assert_eq!(got.tenant_id, tenant);
    // The stub omitted `capabilities` entirely, as an older backend would.
    // Defaulting to empty — rather than failing to parse — is what keeps a new
    // instance working against an older backend.
    assert!(got.capabilities.is_empty());
}

#[tokio::test]
async fn a_refused_code_surfaces_the_backends_own_wording() {
    // "this code has expired" is far more useful to a lone operator than
    // "enrollment failed", so the backend's detail must reach them intact.
    let url = serve(Router::new().route(
        "/v1/enroll",
        post(|| async {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"detail": "this code has expired — generate a new one"})),
            )
        }),
    ))
    .await;

    match client(&url)
        .enroll("dead-beef", Uuid::new_v4(), "0.1.0")
        .await
    {
        Err(CloudError::EnrollmentRefused { detail }) => {
            assert!(detail.contains("expired"), "lost the reason: {detail}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn shipping_returns_the_acknowledgement() {
    let url = serve(Router::new().route(
        "/v1/telemetry",
        post(|Json(batch): Json<TelemetryBatch>| async move {
            Json(serde_json::json!({
                "submission_id": batch.submission_id,
                "processed_spans": batch.spans.len(),
                "stored_spans": batch.spans.len(),
                "metered_bytes": 512
            }))
        }),
    ))
    .await;

    let submission_id = Uuid::new_v4();
    let ack = client(&url)
        .ship("inst_abc", submission_id, spans(3))
        .await
        .expect("ship should succeed");

    assert_eq!(ack.submission_id, submission_id);
    assert_eq!(ack.processed_spans, 3);
    assert_eq!(ack.stored_spans, 3);
    assert_eq!(ack.metered_bytes, 512);
    assert!(ack.warning.is_none());
}

#[tokio::test]
async fn a_rejected_credential_is_repairable_and_keeps_the_submission() {
    let url = serve(Router::new().route(
        "/v1/telemetry",
        post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    ))
    .await;

    let err = client(&url)
        .ship("stale-token", Uuid::new_v4(), spans(1))
        .await
        .unwrap_err();

    assert!(matches!(err, CloudError::CredentialRejected));
    assert!(
        err.is_retryable(),
        "re-enrollment can repair a rejected credential, so the submission must survive"
    );
}

#[tokio::test]
async fn a_backend_error_is_retryable_so_the_batch_is_kept() {
    let url = serve(Router::new().route(
        "/v1/telemetry",
        post(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
    ))
    .await;

    let err = client(&url)
        .ship("inst_abc", Uuid::new_v4(), spans(2))
        .await
        .unwrap_err();

    assert!(err.is_retryable(), "5xx is our problem, not the payload's");
}

#[tokio::test]
async fn rate_limiting_is_treated_as_transient() {
    let url = serve(Router::new().route(
        "/v1/telemetry",
        post(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    ))
    .await;

    assert!(client(&url)
        .ship("inst_abc", Uuid::new_v4(), spans(1))
        .await
        .unwrap_err()
        .is_retryable());
}

#[tokio::test]
async fn an_absent_backend_degrades_without_losing_the_batch() {
    // Nothing is listening. This is the everyday case — a backend outage, a
    // firewall, a laptop offline — and it must cost the operator nothing.
    let client = client("http://127.0.0.1:1");
    let mut spool = Spool::new(100);
    let batch = spans(5);

    spool.push(batch.clone());
    let attempt = spool.take(5);

    let err = client
        .ship("inst_abc", Uuid::new_v4(), attempt.clone())
        .await
        .unwrap_err();

    assert!(err.is_retryable());
    spool.requeue(attempt);

    assert_eq!(spool.len(), 5, "the batch must survive a failed shipment");
    assert_eq!(spool.dropped(), 0, "nothing should have been discarded");
}

#[tokio::test]
async fn a_malformed_acknowledgement_is_reported_rather_than_panicking() {
    let url = serve(Router::new().route(
        "/v1/telemetry",
        post(|| async { Json(serde_json::json!({"unexpected": true})) }),
    ))
    .await;

    match client(&url)
        .ship("inst_abc", Uuid::new_v4(), spans(1))
        .await
    {
        Err(CloudError::InvalidAcknowledgement { detail, .. }) => {
            assert!(detail.contains("ack"))
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}
