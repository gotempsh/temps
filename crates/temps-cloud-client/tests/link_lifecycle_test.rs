// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The link as an instance actually uses it, against a live stub backend.
//!
//! The property under test throughout: **an instance is never worse off for
//! having connected.** A backend that is absent, broken, or refusing must cost
//! the operator no data and no uptime — only a visible, explained degradation.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{extract::State, routing::post, Json, Router};
use temps_cloud_client::link::{CloudLink, FlushOutcome};
use temps_cloud_client::status::{LinkStatus, MirrorHealth};
use temps_cloud_client::{BackendUrl, CloudError, CloudFeatureSwitches};
use temps_cloud_protocol::{SpanRecord, TelemetryBatch, WalGObjectTargetRequest};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Stub {
    /// Status the telemetry endpoint returns. Mutable so a test can take the
    /// backend down and bring it back.
    status: Arc<AtomicU16>,
    received: Arc<AtomicUsize>,
    submissions: Arc<Mutex<Vec<Uuid>>>,
    enroll_delay_ms: Arc<AtomicU64>,
    telemetry_delay_ms: Arc<AtomicU64>,
    telemetry_started: Arc<AtomicUsize>,
    revoked: Arc<AtomicUsize>,
}

async fn serve(stub: Stub) -> String {
    let app = Router::new()
        .route(
            "/v1/enroll",
            post(|State(s): State<Stub>| async move {
                let delay = s.enroll_delay_ms.load(Ordering::SeqCst);
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Json(serde_json::json!({
                    "tenant_id": Uuid::new_v4(),
                    "instance_token": "inst_live"
                }))
            }),
        )
        .route(
            "/v1/telemetry",
            post(
                |State(s): State<Stub>, Json(batch): Json<TelemetryBatch>| async move {
                    s.telemetry_started.fetch_add(1, Ordering::SeqCst);
                    let delay = s.telemetry_delay_ms.load(Ordering::SeqCst);
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                    s.submissions
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(batch.submission_id);
                    let code = s.status.load(Ordering::SeqCst);
                    if code != 200 {
                        return (
                            axum::http::StatusCode::from_u16(code).unwrap(),
                            Json(serde_json::json!({"detail": "stub failure"})),
                        );
                    }
                    let n = batch.spans.len();
                    s.received.fetch_add(n, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::OK,
                        Json(serde_json::json!({
                            "submission_id": batch.submission_id,
                            "processed_spans": n,
                            "stored_spans": n,
                            "metered_bytes": 1
                        })),
                    )
                },
            ),
        )
        .route(
            "/v1/revoke",
            post(|State(s): State<Stub>| async move {
                let code = s.status.load(Ordering::SeqCst);
                if code == 200 {
                    s.revoked.fetch_add(1, Ordering::SeqCst);
                }
                (
                    axum::http::StatusCode::from_u16(code).unwrap(),
                    Json(serde_json::json!({"detail": "stub revoke"})),
                )
            }),
        )
        .with_state(stub);

    let listener = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
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
        })
        .collect()
}

fn link(dir: &tempfile::TempDir) -> CloudLink {
    CloudLink::load_for_loopback_development(dir.path().to_path_buf(), "0.1.0-test")
}

fn backend(url: &str) -> BackendUrl {
    BackendUrl::loopback_development(url).unwrap()
}

fn enable_telemetry(link: &CloudLink) {
    link.set_feature_switches(CloudFeatureSwitches {
        telemetry: true,
        ..Default::default()
    })
    .expect("enable telemetry export");
}

#[tokio::test]
async fn a_fresh_instance_is_unconfigured_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let l = link(&d);

    assert_eq!(l.status(), LinkStatus::NotConfigured);
    assert!(
        !l.status().needs_attention(),
        "an unlinked instance is not a problem"
    );
    assert!(l.status().message().contains("locally"));
}

#[tokio::test]
async fn telemetry_is_not_buffered_before_the_instance_is_linked() {
    // Buffering for a backend that does not exist would burn memory for
    // nothing. Local storage is unaffected either way.
    let d = tempfile::tempdir().unwrap();
    let l = link(&d);

    l.record(spans(100));

    assert_eq!(l.spooled(), 0);
    assert_eq!(l.flush().await, FlushOutcome::NotLinked);
}

#[tokio::test]
async fn the_full_lifecycle_configure_enroll_record_flush() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    assert!(
        l.status().needs_attention(),
        "a configured-but-unlinked instance should ask the operator to finish"
    );

    l.enroll("abcd-2345").await.expect("enroll");
    assert!(matches!(l.status(), LinkStatus::Linked { .. }));
    enable_telemetry(&l);

    l.record(spans(3));
    assert_eq!(l.flush().await, FlushOutcome::Shipped { spans: 3 });

    assert_eq!(stub.received.load(Ordering::SeqCst), 3);
    assert_eq!(l.spooled(), 0);
    assert_eq!(l.health(), MirrorHealth::Healthy);
}

#[tokio::test]
async fn a_saturated_ingest_queue_drops_only_the_mirror_and_reports_it() {
    let d = tempfile::tempdir().unwrap();
    let url = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;

    let link = link(&d);
    link.configure(backend(&url)).unwrap();
    link.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&link);

    for _ in 0..9 {
        link.record(spans(1));
    }

    assert_eq!(link.spooled(), 8, "the producer queue must stay bounded");
    assert_eq!(
        link.health(),
        MirrorHealth::Dropping {
            spooled: 8,
            dropped: 1,
            reason: "spans were discarded before a mirror delivery attempt could report why"
                .to_string(),
        },
        "mirror pressure must be visible without affecting local ingest"
    );
}

#[tokio::test]
async fn concurrent_flushes_ship_each_submission_once() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;
    let link = Arc::new(link(&d));
    link.configure(backend(&url)).unwrap();
    link.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&link);
    link.record(spans(3));

    let first = tokio::spawn({
        let link = link.clone();
        async move { link.flush().await }
    });
    let second = tokio::spawn({
        let link = link.clone();
        async move { link.flush().await }
    });
    let outcomes = [first.await.unwrap(), second.await.unwrap()];

    assert!(outcomes.contains(&FlushOutcome::Shipped { spans: 3 }));
    assert!(outcomes.contains(&FlushOutcome::Idle));
    assert_eq!(stub.received.load(Ordering::SeqCst), 3);
    assert_eq!(
        stub.submissions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len(),
        1
    );
}

#[tokio::test]
async fn an_enrollment_response_cannot_cross_an_origin_change() {
    let d = tempfile::tempdir().unwrap();
    let first = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        enroll_delay_ms: Arc::new(AtomicU64::new(100)),
        ..Default::default()
    })
    .await;
    let second = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;
    let link = Arc::new(link(&d));
    link.configure(backend(&first)).unwrap();

    let enrollment = tokio::spawn({
        let link = link.clone();
        async move { link.enroll("abcd-2345").await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    link.configure(backend(&second)).unwrap();

    let error = enrollment.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("state changed"), "unexpected error: {error}");
    assert!(matches!(
        link.status(),
        LinkStatus::AwaitingEnrollment { .. }
    ));
}

#[tokio::test]
async fn an_outage_buffers_and_a_recovery_drains_without_loss() {
    // The everyday failure, start to finish.
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&l);

    // Backend goes down.
    stub.status.store(503, Ordering::SeqCst);
    l.record(spans(4));

    let outcome = l.flush().await;
    assert!(matches!(outcome, FlushOutcome::Retained { spans: 4, .. }));
    assert_eq!(l.spooled(), 4, "nothing may be lost to a transient failure");

    match l.health() {
        MirrorHealth::Buffering { spooled, .. } => assert_eq!(spooled, 4),
        other => panic!("expected Buffering, got {other:?}"),
    }
    assert!(!l.health().is_losing_data());
    assert!(l
        .health()
        .message()
        .contains("Source telemetry remains in local Temps storage"));

    // Backend recovers.
    stub.status.store(200, Ordering::SeqCst);
    assert_eq!(l.flush().await, FlushOutcome::Shipped { spans: 4 });
    assert_eq!(stub.received.load(Ordering::SeqCst), 4);
    let submissions = stub.submissions.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0], submissions[1], "retry changed its id");
    assert_eq!(l.health(), MirrorHealth::Healthy);
}

#[tokio::test]
async fn changing_backend_origin_requires_remote_disconnect_first() {
    let d = tempfile::tempdir().unwrap();
    let first = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;
    let second = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;

    let l = link(&d);
    l.configure(backend(&first)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    assert!(matches!(l.status(), LinkStatus::Linked { .. }));
    enable_telemetry(&l);
    l.record(spans(2));
    assert_eq!(l.spooled(), 2);

    let error = l.configure(backend(&second)).unwrap_err().to_string();

    assert!(error.contains("Disconnect"), "unexpected error: {error}");
    assert!(matches!(l.status(), LinkStatus::Linked { .. }));
    assert_eq!(
        l.spooled(),
        2,
        "a refused origin change must retain the active link's telemetry"
    );
}

#[tokio::test]
async fn a_rejected_credential_retains_the_batch_for_reenrollment() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(401)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&l);
    l.record(spans(2));

    match l.flush().await {
        FlushOutcome::Retained { spans: 2, reason } => {
            assert!(
                reason.contains("re-enroll"),
                "must tell the operator what to do: {reason}"
            );
        }
        other => panic!("expected Retained, got {other:?}"),
    }

    assert_eq!(l.spooled(), 2, "re-enrollment can repair the credential");
}

#[tokio::test]
async fn the_credential_survives_a_restart() {
    let d = tempfile::tempdir().unwrap();
    let url = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;

    let id = {
        let l = link(&d);
        l.configure(backend(&url)).unwrap();
        l.enroll("abcd-2345").await.unwrap();
        l.instance_id().unwrap()
    };

    // Simulated restart: a new CloudLink over the same directory.
    let reloaded = link(&d);
    assert!(matches!(reloaded.status(), LinkStatus::Linked { .. }));
    assert_eq!(
        reloaded.instance_id().unwrap(),
        id,
        "instance identity must be stable across restarts"
    );
}

#[tokio::test]
async fn an_in_flight_submission_survives_restart_with_the_same_identity() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(503)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    {
        let link = link(&d);
        link.configure(backend(&url)).unwrap();
        link.enroll("abcd-2345").await.unwrap();
        enable_telemetry(&link);
        link.record(spans(3));
        assert!(matches!(
            link.flush().await,
            FlushOutcome::Retained { spans: 3, .. }
        ));
        assert_eq!(link.spooled(), 3);
    }

    let restarted = link(&d);
    enable_telemetry(&restarted);
    assert_eq!(
        restarted.spooled(),
        3,
        "a process restart must retain an in-flight shipment"
    );

    stub.status.store(200, Ordering::SeqCst);
    assert_eq!(restarted.flush().await, FlushOutcome::Shipped { spans: 3 });
    assert_eq!(restarted.spooled(), 0);
    assert_eq!(stub.received.load(Ordering::SeqCst), 3);
    let submissions = stub.submissions.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(submissions.len(), 2);
    assert_eq!(
        submissions[0], submissions[1],
        "restart changed the id of an already-reserved submission"
    );
}

#[tokio::test]
async fn disabling_telemetry_purges_durable_retry_before_reenable_or_restart() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(503)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    {
        let link = link(&d);
        link.configure(backend(&url)).unwrap();
        link.enroll("abcd-2345").await.unwrap();
        enable_telemetry(&link);
        link.record(spans(3));
        assert!(matches!(
            link.flush().await,
            FlushOutcome::Retained { spans: 3, .. }
        ));

        link.set_feature_switches(CloudFeatureSwitches::default())
            .expect("revoke telemetry export");
        assert_eq!(link.spooled(), 0, "revocation must purge queued data");
    }

    stub.status.store(200, Ordering::SeqCst);
    let restarted = link(&d);
    enable_telemetry(&restarted);
    assert_eq!(restarted.spooled(), 0, "durable retry survived revocation");
    assert_eq!(restarted.flush().await, FlushOutcome::Idle);
    assert_eq!(
        stub.submissions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len(),
        1,
        "revoked telemetry was retried after restart"
    );
}

#[tokio::test]
async fn disabling_telemetry_cancels_a_waiting_flush() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        telemetry_delay_ms: Arc::new(AtomicU64::new(30_000)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;
    let link = Arc::new(link(&d));
    link.configure(backend(&url)).unwrap();
    link.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&link);
    link.record(spans(2));

    let flush = tokio::spawn({
        let link = Arc::clone(&link);
        async move { link.flush().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while stub.telemetry_started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("telemetry request starts");

    link.set_feature_switches(CloudFeatureSwitches::default())
        .expect("revoke telemetry export");
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), flush)
        .await
        .expect("revocation cancels the waiting request")
        .expect("flush task joins");
    assert_eq!(outcome, FlushOutcome::NotLinked);
    assert_eq!(link.spooled(), 0);
    assert_eq!(stub.received.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disconnecting_clears_the_credential_but_keeps_the_identity() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    let id = l.instance_id().unwrap();
    l.record(spans(5));

    l.revoke().await.unwrap();
    l.disconnect().unwrap();

    assert_eq!(stub.revoked.load(Ordering::SeqCst), 1);
    assert!(matches!(l.status(), LinkStatus::AwaitingEnrollment { .. }));
    assert_eq!(
        l.spooled(),
        0,
        "buffered data for a severed link is pointless"
    );
    assert_eq!(l.instance_id().unwrap(), id, "re-linking must reattach");
}

#[tokio::test]
async fn failed_remote_revocation_keeps_the_local_link() {
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;
    let link = link(&d);
    link.configure(backend(&url)).unwrap();
    link.enroll("abcd-2345").await.unwrap();

    stub.status.store(503, Ordering::SeqCst);
    assert!(link.revoke().await.is_err());

    assert!(matches!(link.status(), LinkStatus::Linked { .. }));
    assert_eq!(stub.revoked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_corrupt_state_file_leaves_the_instance_working_and_unlinked() {
    // One damaged file must never stop an instance from starting.
    let d = tempfile::tempdir().unwrap();
    let state_dir = d.path().join("cloud-link");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("state.json"), "{ truncated").unwrap();

    let l = link(&d);
    assert!(matches!(l.status(), LinkStatus::StateUnreadable { .. }));
    assert!(
        l.status().needs_attention(),
        "the operator must be told that credentials could not be read"
    );
    l.record(spans(10)); // must not panic
    assert_eq!(l.flush().await, FlushOutcome::NotLinked);
}

#[tokio::test]
async fn flushing_an_empty_spool_is_idle_not_an_error() {
    let d = tempfile::tempdir().unwrap();
    let url = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    enable_telemetry(&l);

    assert_eq!(l.flush().await, FlushOutcome::Idle);
    assert_eq!(l.health(), MirrorHealth::Healthy);
}

#[tokio::test]
async fn a_refused_credential_becomes_a_visible_state_the_operator_must_act_on() {
    // Without this the operator watches a spool that never drains and is told
    // only "Linked" — the one state where waiting cannot help.
    let d = tempfile::tempdir().unwrap();
    let stub = Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    };
    let url = serve(stub.clone()).await;

    let l = link(&d);
    l.configure(backend(&url)).unwrap();
    l.enroll("abcd-2345").await.unwrap();
    assert!(matches!(l.status(), LinkStatus::Linked { .. }));
    enable_telemetry(&l);

    stub.status.store(401, Ordering::SeqCst);
    l.record(spans(1));
    l.flush().await;

    match l.status() {
        LinkStatus::CredentialRejected { .. } => {}
        other => panic!("expected CredentialRejected, got {other:?}"),
    }
    assert!(l.status().needs_attention());
    assert!(l.status().message().contains("Re-enroll"));

    // Recovering clears it.
    stub.status.store(200, Ordering::SeqCst);
    l.enroll("abcd-2345").await.unwrap();
    assert!(matches!(l.status(), LinkStatus::Linked { .. }));
}

#[tokio::test]
async fn linking_does_not_enable_any_export_without_explicit_consent() {
    match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => drop(listener),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("sandbox denied TCP bind; skipping Cloud consent lifecycle test");
            return;
        }
        Err(error) => panic!("probe loopback listener: {error}"),
    }
    let d = tempfile::tempdir().unwrap();
    let url = serve(Stub {
        status: Arc::new(AtomicU16::new(200)),
        ..Default::default()
    })
    .await;
    let link = link(&d);
    link.configure(backend(&url)).unwrap();
    link.enroll("abcd-2345").await.unwrap();

    assert_eq!(link.feature_switches(), CloudFeatureSwitches::default());
    link.record(spans(2));
    assert_eq!(link.spooled(), 0, "telemetry export defaults off");
    let error = link
        .native_object_target(&WalGObjectTargetRequest {
            backup_id: Uuid::new_v4(),
            instance_id: link.instance_id().expect("linked instance id"),
            relative_key: "base/part-0001".into(),
        })
        .await
        .expect_err("backup export defaults off");
    assert!(matches!(
        error,
        CloudError::FeatureDisabled { feature: "backups" }
    ));
    assert!(!link.notifications_available());
}
