// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The background task that drains the spool.
//!
//! Runs beside the instance's own work, so it is written to be a bad citizen of
//! nothing: bounded interval, exponential backoff on failure, and it never
//! holds a lock across a network call.

use std::sync::Arc;
use std::time::Duration;

use crate::link::{CloudLink, FlushOutcome};

/// Interval between flushes when everything is healthy.
pub const BASE_INTERVAL: Duration = Duration::from_secs(15);

/// Ceiling for backoff. A backend that has been down for an hour should be
/// polled every few minutes, not every fifteen seconds — but it must still be
/// polled, or recovery would need a restart to notice.
pub const MAX_INTERVAL: Duration = Duration::from_secs(300);

/// Maximum time a clean shutdown may spend on its final delivery attempt.
///
/// The pending submission remains owned by [`CloudLink`] if the future is
/// cancelled, so timing out cannot corrupt the in-memory queue while shutdown
/// completes. Source telemetry remains authoritative in local Temps storage.
pub const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Next interval after an outcome.
///
/// Separated from the loop so the policy is testable without waiting on real
/// time — a sleeping test is a slow test and a flaky one.
pub fn next_interval(current: Duration, outcome: &FlushOutcome) -> Duration {
    match outcome {
        // Progress, or nothing to do: return to the base rate immediately.
        // Backing off after a success would leave a recovered backend receiving
        // telemetry minutes late for no reason.
        FlushOutcome::Shipped { .. } | FlushOutcome::Idle => BASE_INTERVAL,

        // Not linked: there is nothing to poll for. Slow all the way down, but
        // keep ticking so linking later is noticed without a restart.
        FlushOutcome::NotLinked => MAX_INTERVAL,

        // Transient failure: back off, capped.
        FlushOutcome::Retained { .. } => (current * 2).min(MAX_INTERVAL),

        // Permanent refusal. Backing off does not help — only the operator can
        // fix it — so poll at the base rate to pick up their fix promptly.
        FlushOutcome::Blocked { .. } => BASE_INTERVAL,
    }
}

/// Run until cancelled. Spawn this once at instance startup.
pub async fn run(link: Arc<CloudLink>, mut cancel: tokio::sync::watch::Receiver<bool>) {
    run_with_shutdown_timeout(link, &mut cancel, SHUTDOWN_FLUSH_TIMEOUT).await;
}

async fn run_with_shutdown_timeout(
    link: Arc<CloudLink>,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    shutdown_flush_timeout: Duration,
) {
    let mut interval = BASE_INTERVAL;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    // One last attempt on the way out, bounded: a clean
                    // shutdown should not lose a spool we could have delivered,
                    // but it also must not hang the process.
                    match tokio::time::timeout(shutdown_flush_timeout, link.flush()).await {
                        Ok(FlushOutcome::Shipped { spans }) => {
                            tracing::info!(spans, "mirrored telemetry during shutdown");
                        }
                        Ok(FlushOutcome::Retained { spans, reason })
                        | Ok(FlushOutcome::Blocked { spans, reason }) => {
                            tracing::warn!(
                                spans,
                                reason,
                                "shutdown flush could not mirror telemetry; source remains in local storage"
                            );
                        }
                        Ok(FlushOutcome::Idle | FlushOutcome::NotLinked) => {}
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = shutdown_flush_timeout.as_millis(),
                                spooled_spans = link.spooled(),
                                "shutdown flush timed out; source telemetry remains in local storage"
                            );
                        }
                    }
                    tracing::info!("cloud mirror stopped");
                    return;
                }
            }
        }

        let outcome = link.flush().await;
        interval = next_interval(interval, &outcome);

        match &outcome {
            FlushOutcome::Shipped { spans } => {
                tracing::debug!(spans, "mirrored telemetry");
            }
            FlushOutcome::Retained { spans, reason } => {
                tracing::warn!(
                    spans,
                    reason,
                    retry_in_secs = interval.as_secs(),
                    "buffering"
                );
            }
            FlushOutcome::Blocked { spans, reason } => {
                tracing::error!(spans, reason, "telemetry shipment needs operator action");
            }
            FlushOutcome::Idle | FlushOutcome::NotLinked => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};

    use axum::{extract::State, routing::post, Json, Router};
    use temps_cloud_protocol::{SpanRecord, TelemetryBatch};
    use uuid::Uuid;

    use super::*;

    #[derive(Clone, Default)]
    struct Stub {
        status: Arc<AtomicU16>,
        telemetry_delay_ms: Arc<AtomicU64>,
        received: Arc<AtomicUsize>,
    }

    async fn serve(stub: Stub) -> Option<String> {
        let app = Router::new()
            .route(
                "/v1/enroll",
                post(|| async {
                    Json(serde_json::json!({
                        "tenant_id": Uuid::new_v4(),
                        "instance_token": "inst_shutdown_test"
                    }))
                }),
            )
            .route(
                "/v1/telemetry",
                post(
                    |State(stub): State<Stub>, Json(batch): Json<TelemetryBatch>| async move {
                        let delay = stub.telemetry_delay_ms.load(Ordering::SeqCst);
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        let status = stub.status.load(Ordering::SeqCst);
                        if status != 200 {
                            return (
                                axum::http::StatusCode::from_u16(status)
                                    .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                                Json(serde_json::json!({"detail": "stub failure"})),
                            );
                        }
                        let spans = batch.spans.len();
                        stub.received.fetch_add(spans, Ordering::SeqCst);
                        (
                            axum::http::StatusCode::OK,
                            Json(serde_json::json!({
                                "submission_id": batch.submission_id,
                                "processed_spans": spans,
                                "stored_spans": spans,
                                "metered_bytes": 1
                            })),
                        )
                    },
                ),
            )
            .with_state(stub);
        let listener = match tokio::net::TcpListener::bind::<SocketAddr>(
            "127.0.0.1:0".parse().expect("loopback address must parse"),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping flusher network test: sandbox denied TCP bind");
                return None;
            }
            Err(error) => panic!("test server must bind: {error}"),
        };
        let address = listener.local_addr().expect("test server has an address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(format!("http://{address}"))
    }

    fn span() -> SpanRecord {
        // A `Metered`-fidelity record: the ADR-040 fields stay at their
        // defaults, which is what the flusher sees for a project that never
        // opted in.
        SpanRecord {
            trace_id: "shutdown-trace".into(),
            span_id: "shutdown-span".into(),
            name: "shutdown".into(),
            ts_millis: 1,
            duration_ms: 1.0,
            attributes: Default::default(),
            ..Default::default()
        }
    }

    async fn linked_test_link(stub: Stub) -> Option<(Arc<CloudLink>, tempfile::TempDir)> {
        let directory = tempfile::tempdir().expect("temporary directory must be created");
        let backend = serve(stub).await?;
        let link = Arc::new(CloudLink::load_for_loopback_development(
            directory.path().to_path_buf(),
            "shutdown-test",
        ));
        link.configure(
            crate::BackendUrl::loopback_development(&backend)
                .expect("stub backend URL must be accepted"),
        )
        .expect("test link must be configured");
        link.enroll("shutdown-code")
            .await
            .expect("test link must enroll");
        link.set_feature_switches(crate::CloudFeatureSwitches {
            telemetry: true,
            ..Default::default()
        })
        .expect("enable telemetry export");
        Some((link, directory))
    }

    async fn cancel_and_join(link: Arc<CloudLink>, timeout: Duration) {
        let (cancel, mut receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            run_with_shutdown_timeout(link, &mut receiver, timeout).await;
        });
        cancel.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown must remain bounded")
            .expect("flusher task must exit cleanly");
    }

    #[test]
    fn a_transient_failure_backs_off_and_is_capped() {
        let mut d = BASE_INTERVAL;
        let retained = FlushOutcome::Retained {
            spans: 1,
            reason: "unreachable".into(),
        };

        d = next_interval(d, &retained);
        assert_eq!(d, BASE_INTERVAL * 2);

        for _ in 0..20 {
            d = next_interval(d, &retained);
        }
        assert_eq!(d, MAX_INTERVAL, "backoff must be bounded");
    }

    #[test]
    fn success_returns_to_the_base_rate_immediately() {
        // Staying backed off after recovery would deliver telemetry minutes
        // late for no reason.
        assert_eq!(
            next_interval(MAX_INTERVAL, &FlushOutcome::Shipped { spans: 10 }),
            BASE_INTERVAL
        );
        assert_eq!(
            next_interval(MAX_INTERVAL, &FlushOutcome::Idle),
            BASE_INTERVAL
        );
    }

    #[test]
    fn a_permanent_refusal_does_not_back_off() {
        // Only the operator can fix it, so poll at the base rate to pick up
        // their fix promptly rather than making them wait out a backoff.
        assert_eq!(
            next_interval(
                MAX_INTERVAL,
                &FlushOutcome::Blocked {
                    spans: 1,
                    reason: "re-enroll".into()
                }
            ),
            BASE_INTERVAL
        );
    }

    #[test]
    fn an_unlinked_instance_still_ticks() {
        // Slowly — but it must tick, or linking an account would need a restart
        // before anything shipped.
        let d = next_interval(BASE_INTERVAL, &FlushOutcome::NotLinked);
        assert_eq!(d, MAX_INTERVAL);
        assert!(d < Duration::from_secs(3600), "must still poll");
    }

    #[tokio::test]
    async fn shutdown_flushes_queued_spans_before_stopping() {
        let stub = Stub {
            status: Arc::new(AtomicU16::new(200)),
            ..Default::default()
        };
        let Some((link, _directory)) = linked_test_link(stub.clone()).await else {
            return;
        };
        link.record(vec![span()]);

        cancel_and_join(link.clone(), Duration::from_secs(1)).await;

        assert_eq!(stub.received.load(Ordering::SeqCst), 1);
        assert_eq!(link.spooled(), 0);
    }

    #[tokio::test]
    async fn shutdown_retains_spans_when_the_backend_rejects_the_attempt() {
        let stub = Stub {
            status: Arc::new(AtomicU16::new(200)),
            ..Default::default()
        };
        let Some((link, _directory)) = linked_test_link(stub.clone()).await else {
            return;
        };
        stub.status.store(503, Ordering::SeqCst);
        link.record(vec![span()]);

        cancel_and_join(link.clone(), Duration::from_secs(1)).await;

        assert_eq!(stub.received.load(Ordering::SeqCst), 0);
        assert_eq!(
            link.spooled(),
            1,
            "a failed final attempt must remain queued"
        );
    }

    #[tokio::test]
    async fn shutdown_timeout_is_bounded_without_corrupting_the_pending_submission() {
        let stub = Stub {
            status: Arc::new(AtomicU16::new(200)),
            telemetry_delay_ms: Arc::new(AtomicU64::new(500)),
            ..Default::default()
        };
        let Some((link, _directory)) = linked_test_link(stub).await else {
            return;
        };
        link.record(vec![span()]);

        cancel_and_join(link.clone(), Duration::from_millis(20)).await;

        assert_eq!(
            link.spooled(),
            1,
            "timing out must not corrupt the in-memory submission"
        );
    }
}
