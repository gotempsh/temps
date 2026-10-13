// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retry loop for ClickHouse migrations that run on a detached task at
//! startup.
//!
//! A one-shot attempt that only `warn!`s leaves the instance without its
//! tables until the next restart, and the failure resurfaces much later as
//! `Code: 60` (unknown table) on a dashboard. So every subsystem retries until
//! its migrations apply, with two cadences:
//!
//! - **Transient** failures (ClickHouse still booting, a network blip) back
//!   off exponentially from 5 s to 60 s, via [`temps_core::retry::RetryConfig`].
//! - **Decode** failures ([`FailureKind::Decode`]) cannot heal by waiting, so
//!   they are logged as such and re-checked at a slow, fixed interval. When an
//!   operator fixes the server (or the Temps setting plus a restart), the next
//!   attempt succeeds and no further intervention is needed.
//!
//! The loop is unbounded in attempts and bounded in delay: giving up would
//! strand the subsystem for the life of the process, and one cheap query every
//! few minutes is not a load concern.

use std::future::Future;
use std::time::Duration;

use temps_core::retry::RetryConfig;
use tracing::{info, warn};

use crate::diagnostics::{classify_message, FailureKind, COMPRESSION_REMEDY};

/// Delays between migration attempts.
#[derive(Debug, Clone)]
pub struct MigrationRetryPolicy {
    /// Backoff for failures that waiting can fix. `compute_delay(n)` is the
    /// delay after the `n+1`-th failed attempt.
    pub transient: RetryConfig,
    /// Fixed interval between attempts while the failure is a decode failure.
    pub decode_interval: Duration,
    /// Each delay is stretched by up to this many percent so several
    /// subsystems that failed together do not retry in lockstep.
    pub jitter_percent: u32,
}

impl Default for MigrationRetryPolicy {
    fn default() -> Self {
        Self {
            transient: RetryConfig::new(u32::MAX)
                .with_base_delay(Duration::from_secs(5))
                .with_max_delay(Duration::from_secs(60)),
            decode_interval: Duration::from_secs(300),
            jitter_percent: 20,
        }
    }
}

impl MigrationRetryPolicy {
    /// The delay before the next attempt, without jitter. `failures` counts
    /// failed attempts so far (>= 1).
    pub fn base_delay(&self, failures: u32, kind: FailureKind) -> Duration {
        if kind.persists_until_config_changes() {
            self.decode_interval
        } else {
            // `compute_delay` shifts by the attempt number; cap the shift so a
            // long-failing loop cannot overflow it.
            self.transient
                .compute_delay(failures.saturating_sub(1).min(16))
        }
    }

    /// `base` stretched by a random 0..=`jitter_percent` percent.
    pub fn jittered(&self, base: Duration) -> Duration {
        let extra_per_mille =
            (rand::random::<u32>() % (self.jitter_percent.saturating_mul(10) + 1)) as u64;
        base + base.mul_f64(extra_per_mille as f64 / 1000.0)
    }
}

/// Run `apply` until it succeeds, sleeping between failures per `policy`.
///
/// `subsystem` names what is being migrated ("analytics", "proxy-log", …) in
/// every log line. `first_attempt_done` is signalled after the first attempt,
/// whatever its outcome, so a caller can wait for one attempt without waiting
/// out the backoff.
///
/// Every attempt is logged with its attempt count. Failures are classified
/// from the error's message with [`classify_message`], which is what the
/// per-crate error types preserve of a ClickHouse failure.
pub async fn retry_until_applied<T, E, F, Fut>(
    subsystem: &str,
    policy: &MigrationRetryPolicy,
    mut apply: F,
    first_attempt_done: Option<tokio::sync::oneshot::Sender<()>>,
) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut first_attempt_done = first_attempt_done;
    let mut attempt: u32 = 1;
    loop {
        let result = apply().await;
        if let Some(done) = first_attempt_done.take() {
            // The receiver is gone once startup stopped waiting; nothing to do.
            let _ = done.send(());
        }
        match result {
            Ok(applied) => {
                if attempt > 1 {
                    info!(
                        subsystem,
                        attempts = attempt,
                        "ClickHouse migrations applied after earlier failures"
                    );
                }
                return applied;
            }
            Err(error) => {
                let message = error.to_string();
                let kind = classify_message(&message);
                let delay = policy.jittered(policy.base_delay(attempt, kind));
                if kind.persists_until_config_changes() {
                    warn!(
                        subsystem,
                        attempt,
                        failure_kind = kind.as_str(),
                        persists_until_config_changes = true,
                        retry_in_secs = delay.as_secs(),
                        error = %message,
                        remedy = COMPRESSION_REMEDY,
                        "ClickHouse migrations cannot succeed until the compression \
                         configuration changes; re-checking at a slow interval so recovery \
                         needs no restart"
                    );
                } else {
                    warn!(
                        subsystem,
                        attempt,
                        failure_kind = kind.as_str(),
                        persists_until_config_changes = false,
                        retry_in_secs = delay.as_secs(),
                        error = %message,
                        "ClickHouse migrations failed; retrying with backoff"
                    );
                }
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Run `apply` until it succeeds on a detached task, then hand its result to
/// `on_applied`.
///
/// Uses the current Tokio runtime when there is one. Plugin registration and
/// the standalone proxy's setup are sometimes synchronous with no runtime on
/// the thread, where a bare `tokio::spawn` panics; there the retry loop runs
/// on its own named thread with a private current-thread runtime, and this
/// call waits for the *first* attempt only (as the one-shot code it replaces
/// did), so a healthy server is migrated before first use while an unhealthy
/// one never blocks startup beyond that attempt.
pub fn spawn_retrying<T, E, F, Fut, D>(
    subsystem: &'static str,
    policy: MigrationRetryPolicy,
    apply: F,
    on_applied: D,
) where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    D: FnOnce(T) + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let applied = retry_until_applied(subsystem, &policy, apply, None).await;
                on_applied(applied);
            });
        }
        Err(_) => {
            let (first_attempt_done, first_attempt) = tokio::sync::oneshot::channel();
            let spawned = std::thread::Builder::new()
                .name(format!("temps-ch-migrate-{subsystem}"))
                .spawn(move || {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(async move {
                            let applied = retry_until_applied(
                                subsystem,
                                &policy,
                                apply,
                                Some(first_attempt_done),
                            )
                            .await;
                            on_applied(applied);
                        }),
                        Err(error) => warn!(
                            subsystem,
                            %error,
                            "Could not build a runtime to apply ClickHouse migrations; they \
                             will not run until the next restart"
                        ),
                    }
                });
            match spawned {
                // Returns as soon as the first attempt has finished, or at
                // once if the thread could not build its runtime.
                Ok(_) => {
                    let _ = first_attempt.blocking_recv();
                }
                Err(error) => warn!(
                    subsystem,
                    %error,
                    "Could not start a thread to apply ClickHouse migrations; they will not \
                     run until the next restart"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    const DECODE_ERROR: &str = "failed to read migration tracking table: decompression error: \
                                incorrect magic number";

    #[test]
    fn transient_backoff_doubles_from_five_seconds_up_to_a_minute() {
        let policy = MigrationRetryPolicy::default();
        let secs = |failures| {
            policy
                .base_delay(failures, FailureKind::Unreachable)
                .as_secs()
        };
        assert_eq!(secs(1), 5);
        assert_eq!(secs(2), 10);
        assert_eq!(secs(3), 20);
        assert_eq!(secs(4), 40);
        assert_eq!(secs(5), 60);
        assert_eq!(secs(500), 60, "delay stays bounded however long it fails");
    }

    #[test]
    fn decode_failures_use_the_slow_fixed_cadence() {
        let policy = MigrationRetryPolicy::default();
        for failures in [1, 2, 50] {
            assert_eq!(
                policy.base_delay(failures, FailureKind::Decode),
                Duration::from_secs(300)
            );
        }
    }

    #[test]
    fn jitter_only_ever_lengthens_a_delay_within_its_bound() {
        let policy = MigrationRetryPolicy::default();
        let base = Duration::from_secs(60);
        for _ in 0..200 {
            let jittered = policy.jittered(base);
            assert!(jittered >= base);
            assert!(jittered <= base + base.mul_f64(0.2));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_until_they_apply() {
        let attempts = Arc::new(AtomicU32::new(0));
        let started = tokio::time::Instant::now();

        let applied = retry_until_applied(
            "test",
            &MigrationRetryPolicy::default(),
            || {
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 4 {
                        Err("network error: connection refused".to_string())
                    } else {
                        Ok(n)
                    }
                }
            },
            None,
        )
        .await;

        assert_eq!(applied, 4);
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        // 5 + 10 + 20 seconds of backoff, each stretched by at most 20%.
        let waited = started.elapsed();
        assert!(waited >= Duration::from_secs(35), "{waited:?}");
        assert!(waited <= Duration::from_secs(42), "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn decode_failures_are_rechecked_slowly_and_recover_without_a_restart() {
        let attempts = Arc::new(AtomicU32::new(0));
        let started = tokio::time::Instant::now();

        let applied = retry_until_applied(
            "test",
            &MigrationRetryPolicy::default(),
            || {
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    // The operator fixes the server before the third attempt.
                    if n < 3 {
                        Err(DECODE_ERROR.to_string())
                    } else {
                        Ok(())
                    }
                }
            },
            None,
        )
        .await;

        assert_eq!(applied, ());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_secs(600), "{waited:?}");
        assert!(waited <= Duration::from_secs(720), "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn first_attempt_signal_fires_before_any_backoff() {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let attempts = Arc::new(AtomicU32::new(0));

        let task = tokio::spawn({
            let attempts = attempts.clone();
            async move {
                retry_until_applied(
                    "test",
                    &MigrationRetryPolicy::default(),
                    || {
                        let attempts = attempts.clone();
                        async move {
                            let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                            if n < 2 {
                                Err::<(), _>("network error: refused".to_string())
                            } else {
                                Ok(())
                            }
                        }
                    },
                    Some(tx),
                )
                .await
            }
        });

        // Yield until the first attempt ran; the backoff sleep is still pending.
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_ok(), "signalled after the first attempt");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        task.await.expect("retry task completes");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn without_a_runtime_the_first_attempt_completes_before_spawn_retrying_returns() {
        // A plain `#[test]` has no Tokio runtime on the thread: the path the
        // standalone proxy's synchronous setup takes.
        let attempts = Arc::new(AtomicU32::new(0));
        let applied = Arc::new(AtomicU32::new(0));

        spawn_retrying(
            "test",
            MigrationRetryPolicy::default(),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Ok::<u32, String>(7)
                    }
                }
            },
            {
                let applied = applied.clone();
                move |value| applied.store(value, Ordering::SeqCst)
            },
        );

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        // `on_applied` runs on the detached thread right after the attempt;
        // give it a moment rather than racing it.
        for _ in 0..100 {
            if applied.load(Ordering::SeqCst) == 7 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(applied.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn without_a_runtime_a_failing_first_attempt_does_not_block_startup_for_the_backoff() {
        let attempts = Arc::new(AtomicU32::new(0));
        let started = std::time::Instant::now();

        spawn_retrying(
            "test",
            MigrationRetryPolicy::default(),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err::<(), _>("network error: connection refused".to_string())
                    }
                }
            },
            |()| {},
        );

        // Returned after one failed attempt, long before the 5 s backoff.
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(attempts.load(Ordering::SeqCst) >= 1);
    }
}
