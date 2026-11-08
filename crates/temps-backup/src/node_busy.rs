// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Waiting for a slot on a worker node that already runs its maximum of
//! backups and restores.
//!
//! A node's agent runs a bounded number of data operations at once and turns
//! the next one away with 429 ([`ExternalServiceError::NodeBusy`]): nothing
//! ran, and the same request succeeds once one of the others finishes. A
//! backup or restore run therefore waits for a slot, with exponential backoff
//! and a total bound, instead of failing. The caller's `wait` hook records
//! the wait where the run is shown (a restore run's phase) and sleeps, so it
//! can also stop the wait (a cancelled run).

use std::future::Future;
use std::time::Duration;

use temps_providers::services::ExternalServiceError;

/// How long to wait between tries, and in total.
#[derive(Debug, Clone, Copy)]
pub struct NodeBusyPolicy {
    /// The wait before the second try; each later wait doubles.
    pub first: Duration,
    /// The longest single wait.
    pub max_delay: Duration,
    /// The longest total wait; the next wait would pass it, so the run fails
    /// with the node's last answer.
    pub budget: Duration,
}

impl NodeBusyPolicy {
    /// 15 s, doubling to 5 min, for at most 6 h: a node's agent ends any data
    /// operation within its 6 h deadline, so a node busy for longer has
    /// something else wrong with it.
    pub const DEFAULT: Self = Self {
        first: Duration::from_secs(15),
        max_delay: Duration::from_secs(5 * 60),
        budget: Duration::from_secs(6 * 60 * 60),
    };

    fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.first.saturating_mul(factor).min(self.max_delay)
    }
}

/// One wait for a slot, given to the caller's hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeWait {
    /// The try that was turned away (1 for the first).
    pub attempt: u32,
    /// How long the hook must wait before the next try.
    pub delay: Duration,
    /// How long the run has waited so far, before this wait.
    pub waited: Duration,
    pub node: String,
    /// The node's own words.
    pub detail: String,
}

impl NodeWait {
    /// A line for a run's log or phase message.
    pub fn describe(&self) -> String {
        format!(
            "node '{}' already runs its maximum of backups and restores; try {} in {}s \
             (waited {}s so far): {}",
            self.node,
            self.attempt + 1,
            self.delay.as_secs(),
            self.waited.as_secs(),
            self.detail
        )
    }
}

/// Run `call`; while the node answers busy and the total wait stays within
/// `policy.budget`, hand the wait to `wait` (which records it and sleeps
/// `delay`) and try again.
///
/// The outer `Err` is the hook's own error (a cancelled run), the inner
/// result the last try's: a success, another error as it is, or `NodeBusy`
/// once the budget is spent, its detail saying how long the run waited.
pub async fn retry_while_node_busy<T, E, Call, CallFut, Wait, WaitFut>(
    policy: &NodeBusyPolicy,
    mut call: Call,
    mut wait: Wait,
) -> Result<Result<T, ExternalServiceError>, E>
where
    Call: FnMut() -> CallFut,
    CallFut: Future<Output = Result<T, ExternalServiceError>>,
    Wait: FnMut(NodeWait) -> WaitFut,
    WaitFut: Future<Output = Result<(), E>>,
{
    let mut waited = Duration::ZERO;
    let mut attempt = 1u32;
    loop {
        match call().await {
            Err(ExternalServiceError::NodeBusy {
                node,
                route,
                detail,
            }) => {
                let delay = policy.delay(attempt);
                if waited.saturating_add(delay) > policy.budget {
                    return Ok(Err(ExternalServiceError::NodeBusy {
                        node,
                        route,
                        detail: format!(
                            "{detail} (still busy after waiting {}s over {attempt} tries)",
                            waited.as_secs()
                        ),
                    }));
                }
                wait(NodeWait {
                    attempt,
                    delay,
                    waited,
                    node,
                    detail,
                })
                .await?;
                waited = waited.saturating_add(delay);
                attempt = attempt.saturating_add(1);
            }
            other => return Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    fn busy() -> ExternalServiceError {
        ExternalServiceError::NodeBusy {
            node: "worker-1".to_string(),
            route: "/agent/services/data-restore".to_string(),
            detail: "this node already runs 2 backups or restores".to_string(),
        }
    }

    /// A busy node is waited for with doubling, capped delays, and the call
    /// that finds a slot is the result.
    #[tokio::test]
    async fn a_busy_node_is_waited_for_until_a_slot_frees() {
        let calls = Arc::new(AtomicU32::new(0));
        let waits = Arc::new(Mutex::new(Vec::new()));
        let policy = NodeBusyPolicy {
            first: Duration::from_secs(15),
            max_delay: Duration::from_secs(40),
            budget: Duration::from_secs(3600),
        };
        let outcome = retry_while_node_busy(
            &policy,
            || {
                let calls = calls.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) < 3 {
                        Err(busy())
                    } else {
                        Ok("restored")
                    }
                }
            },
            |wait| {
                let waits = waits.clone();
                async move {
                    waits.lock().unwrap().push(wait);
                    Ok::<(), std::convert::Infallible>(())
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.unwrap(), "restored");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let waits = waits.lock().unwrap();
        let delays: Vec<u64> = waits.iter().map(|w| w.delay.as_secs()).collect();
        assert_eq!(delays, vec![15, 30, 40]);
        let waited: Vec<u64> = waits.iter().map(|w| w.waited.as_secs()).collect();
        assert_eq!(waited, vec![0, 15, 45]);
        assert_eq!(waits[0].node, "worker-1");
        assert!(
            waits[2].describe().contains("try 4 in 40s"),
            "{}",
            waits[2].describe()
        );
    }

    /// The wait is bounded: past the budget the run gets the node's answer,
    /// saying how long it waited.
    #[tokio::test]
    async fn a_node_busy_past_the_budget_fails_with_how_long_it_waited() {
        let policy = NodeBusyPolicy {
            first: Duration::from_secs(10),
            max_delay: Duration::from_secs(10),
            budget: Duration::from_secs(25),
        };
        let calls = Arc::new(AtomicU32::new(0));
        let outcome = retry_while_node_busy(
            &policy,
            || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(busy())
                }
            },
            |_| async { Ok::<(), std::convert::Infallible>(()) },
        )
        .await
        .unwrap();
        // Waits of 10 s and 10 s fit; a third would pass 25 s.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        match outcome {
            Err(ExternalServiceError::NodeBusy { detail, .. }) => {
                assert!(
                    detail.contains("after waiting 20s over 3 tries"),
                    "{detail}"
                )
            }
            other => panic!("expected NodeBusy, got {other:?}"),
        }
    }

    /// Any other answer is returned at once, and the hook can stop the wait.
    #[tokio::test]
    async fn other_errors_are_not_retried_and_the_hook_can_stop_the_wait() {
        let calls = Arc::new(AtomicU32::new(0));
        let outcome = retry_while_node_busy(
            &NodeBusyPolicy::DEFAULT,
            || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(ExternalServiceError::InternalError {
                        reason: "dump failed".to_string(),
                    })
                }
            },
            |_| async { Ok::<(), &str>(()) },
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            Err(ExternalServiceError::InternalError { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let stopped = retry_while_node_busy(
            &NodeBusyPolicy::DEFAULT,
            || async { Err::<(), _>(busy()) },
            |_| async { Err("cancelled") },
        )
        .await;
        assert_eq!(stopped.unwrap_err(), "cancelled");
    }

    /// End to end through the real client: an agent that answers 429 twice
    /// (its limit of data operations) and then restores is waited for, and
    /// the run gets the restore instead of an error.
    #[tokio::test]
    async fn a_restore_turned_away_by_a_busy_agent_runs_once_a_slot_frees() {
        use axum::{routing::post, Json, Router};
        use temps_providers::remote_service_client::{
            RemoteServiceClient, RemoteServiceRestoreRequest, WorkerS3Destination,
        };

        static CALLS: AtomicU32 = AtomicU32::new(0);
        async fn agent() -> (axum::http::StatusCode, Json<serde_json::Value>) {
            if CALLS.fetch_add(1, Ordering::SeqCst) < 2 {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({
                        "success": false,
                        "data": null,
                        "error": "Cannot restore service 'orders': this node already runs 2 backups or restores; retry when one finishes"
                    })),
                );
            }
            (
                axum::http::StatusCode::OK,
                Json(
                    serde_json::json!({"success": true, "data": {"buckets": ["files"]}, "error": null}),
                ),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/agent/services/data-restore", post(agent)),
            )
            .await
            .unwrap();
        });
        let (_, sealing_key) = temps_core::worker_ingress::generate_ingress_key().unwrap();
        let client = RemoteServiceClient::new(
            format!("http://{address}"),
            "node-token".to_string(),
            "worker-1".to_string(),
        )
        .unwrap()
        .with_sealing(temps_core::sealed_request::NodeSealing::Key(sealing_key));
        let request = RemoteServiceRestoreRequest {
            service_config: temps_providers::externalsvc::ServiceConfig {
                name: "orders".to_string(),
                service_type: temps_providers::ServiceType::S3,
                version: None,
                parameters: serde_json::json!({}),
            },
            source: WorkerS3Destination {
                bucket: "backups".to_string(),
                region: "us-east-1".to_string(),
                endpoint: None,
                force_path_style: true,
                access_key_id: "a".to_string(),
                secret_access_key: "s".to_string(),
                session_token: None,
            },
            location: "external_services/s3/orders/backup-1".to_string(),
        };
        let waits = Arc::new(Mutex::new(Vec::new()));
        let restored = retry_while_node_busy(
            &NodeBusyPolicy {
                first: Duration::from_millis(10),
                max_delay: Duration::from_millis(20),
                budget: Duration::from_secs(5),
            },
            || client.restore_service(&request),
            |wait| {
                let waits = waits.clone();
                async move {
                    tokio::time::sleep(wait.delay).await;
                    waits.lock().unwrap().push(wait);
                    Ok::<(), std::convert::Infallible>(())
                }
            },
        )
        .await
        .unwrap()
        .expect("the restore runs once the node has a slot");
        server.abort();
        assert_eq!(restored.buckets, vec!["files".to_string()]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
        let waits = waits.lock().unwrap();
        assert_eq!(waits.len(), 2);
        assert_eq!(waits[0].node, "worker-1");
        assert!(
            waits[0].detail.contains("already runs 2"),
            "{}",
            waits[0].detail
        );
    }

    #[test]
    fn delays_double_and_are_capped_without_overflow() {
        let policy = NodeBusyPolicy::DEFAULT;
        assert_eq!(policy.delay(1), Duration::from_secs(15));
        assert_eq!(policy.delay(2), Duration::from_secs(30));
        assert_eq!(policy.delay(6), Duration::from_secs(300));
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(300));
    }
}
