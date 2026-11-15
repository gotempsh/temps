// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::service_access::{ServiceAccessCommand, ServiceAccessReply, ServiceAccessTarget};
use axum::{http::StatusCode, response::IntoResponse};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn four_inspections_share_a_two_request_worker_without_losing_renewals() {
    let slots = Arc::new(tokio::sync::Semaphore::new(2));
    let rejected = Arc::new(AtomicUsize::new(0));
    let refusals = rejected.clone();
    let worker_slots = slots.clone();
    let app = axum::Router::new().fallback(move || {
        let slots = worker_slots.clone();
        let refusals = refusals.clone();
        async move {
            let Ok(_permit) = slots.try_acquire() else {
                refusals.fetch_add(1, Ordering::SeqCst);
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            };
            tokio::time::sleep(Duration::from_secs(1)).await;
            axum::Json(ServiceAccessReply::Inspect {
                target: ServiceAccessTarget {
                    incarnation: uuid::Uuid::new_v4().to_string(),
                    container_id: "fixture".into(),
                },
                worker: temps_service_access_runtime::WorkerIdentity {
                    boot_id: uuid::Uuid::new_v4(),
                    seal_public_key: [1; 32],
                },
                connected: false,
                binding_version: None,
            })
            .into_response()
        }
    });
    let url = serve_raw(app).await;
    let provider = provider_at(&url);
    let handle = remote_handle();
    let inspect = || provider.service_access(&handle, ServiceAccessCommand::Inspect);
    let (a, b, c, d) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(inspect(), inspect(), inspect(), inspect())
    })
    .await
    .unwrap();
    for result in [a, b, c, d] {
        assert!(matches!(
            result.unwrap(),
            ServiceAccessReply::Inspect { .. }
        ));
    }
    assert!(rejected.load(Ordering::SeqCst) > 0);
    assert_eq!(slots.available_permits(), 2);
}

#[tokio::test]
async fn capacity_retries_are_bounded_and_other_errors_are_not_replayed() {
    for (status, expected) in [(429, 8), (401, 1), (403, 1), (500, 1)] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let app = axum::Router::new().fallback(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async move { StatusCode::from_u16(status).unwrap() }
        });
        let url = serve_raw(app).await;
        assert!(provider_at(&url)
            .service_access(&remote_handle(), ServiceAccessCommand::Inspect)
            .await
            .is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), expected);
    }
}

#[tokio::test]
async fn capacity_backoff_obeys_the_total_request_deadline() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let app = axum::Router::new().fallback(move || {
        count.fetch_add(1, Ordering::SeqCst);
        async { StatusCode::TOO_MANY_REQUESTS }
    });
    let url = serve_raw(app).await;
    let error = provider_at(&url)
        .call_body::<_, RemoteOkResponse>(
            "/agent/sandboxes/service-access",
            &RemoteHandleRequest {
                handle: remote_handle(),
            },
            false,
            Duration::from_millis(50),
            None,
            "service access",
            SMALL_RESPONSE_CAP,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("deadline exceeded"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
