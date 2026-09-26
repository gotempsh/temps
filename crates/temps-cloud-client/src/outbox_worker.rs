// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The background task that drains the Cloud-primary span outbox
//! (ADR-041 §3b).
//!
//! # Why this is not [`crate::flusher`]
//!
//! The flusher ships **one batch per 15-second tick**. Measured against
//! `link.rs`'s 500-span batch that is a ceiling of roughly 33 spans/second, and
//! above that the spool backlogs permanently and then discards. That is a
//! correct, deliberate cadence for a best-effort mirror running beside the
//! instance's real work — but it cannot be a primary write path.
//!
//! This worker drains **until idle** instead: it keeps claiming and shipping
//! batches for as long as there is work, and only then returns to its poll
//! interval. The precedent is already in this repository —
//! `crates/temps-otel/src/services/cloud_backfill.rs` loops `link.flush()`
//! until `FlushOutcome::Idle` for exactly this reason, and that is how the
//! backfill achieves usable throughput over the same transport.
//!
//! That single change lifts the ceiling from ~33 spans/second to roughly 500
//! spans per round trip. ADR-041 §3b is explicit that the *next* lever —
//! bounded concurrent in-flight submissions — must not be pulled until Cloud
//! confirms that `/v1/telemetry`'s idempotency and metering tolerate concurrent
//! submissions from one instance. So the drain is sequential, and the load test
//! exists to prove that is sufficient before any project can be Cloud-primary.
//!
//! # Bounded memory
//!
//! Each cycle holds at most one claimed batch (`OUTBOX_BATCH_SIZE` spans) in
//! memory. The queue's cost is disk — rows in Postgres — not RAM, however long
//! the outage lasts. That is the property the in-memory spool cannot have at
//! any size.

use std::sync::Arc;
use std::time::Duration;

use temps_cloud_protocol::Unavailable;
use uuid::Uuid;

use crate::link::{CloudLink, OutboxShipOutcome};
use crate::outbox::{SpanOutbox, DEAD_LETTER_PAYLOAD_RETENTION, OUTBOX_BATCH_SIZE};

/// Poll interval when the queue was empty last cycle.
///
/// One second, matching `ch_fanout`'s poll interval rather than the flusher's
/// 15 seconds: a fresh span also signals
/// [`SpanOutbox::wake_handle`](crate::outbox::SpanOutbox::wake_handle), so this
/// is only the ceiling on how long an *unsignalled* change (a row inserted by
/// another process, a recovered backend) waits to be noticed.
pub const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// First backoff step after a failed shipment.
pub const BASE_BACKOFF: Duration = Duration::from_secs(5);

/// Backoff ceiling.
///
/// Shorter than the mirror flusher's 300 s on purpose. A backed-off mirror
/// costs freshness; a backed-off primary path costs queue headroom, and every
/// second spent not retrying is a second closer to the byte cap and a real gap.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Longest a shutdown may spend on its final drain.
///
/// Matches the flusher's `SHUTDOWN_FLUSH_TIMEOUT` contract — a clean shutdown
/// should not lose work it could have delivered, and must not hang the process.
/// Unlike the flusher, nothing is lost when this times out: the rows are
/// durable and the next start resumes from them.
pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the worker re-reads the operator's byte cap and re-syncs its
/// cached queue size against Postgres.
pub const SETTINGS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// What one drain cycle did.
#[derive(Debug, Clone, PartialEq)]
pub enum DrainOutcome {
    /// The queue is empty. Nothing left to do this cycle.
    Idle,
    /// Not linked, or telemetry export is off. Rows are retained; the write-mode
    /// fallback resolves this, not a retry.
    NotLinked,
    /// Everything claimed this cycle shipped.
    Drained { spans: usize, batches: usize },
    /// A shipment failed. `spans` is what had already shipped before the
    /// failure, so progress is never reported as zero just because the cycle
    /// ended badly.
    Failed {
        spans: usize,
        batches: usize,
        reason: String,
    },
    /// Cloud accepted, but answered with a condition only the operator can
    /// resolve. The caller must act on this rather than log it: under
    /// Cloud-primary writes, continuing to retry into an exhausted quota
    /// converts a billing state into data loss (ADR-041 §7b).
    NeedsOperator {
        spans: usize,
        batches: usize,
        detail: Unavailable,
    },
}

impl DrainOutcome {
    /// Spans that reached Cloud this cycle.
    pub fn shipped_spans(&self) -> usize {
        match self {
            DrainOutcome::Idle | DrainOutcome::NotLinked => 0,
            DrainOutcome::Drained { spans, .. }
            | DrainOutcome::Failed { spans, .. }
            | DrainOutcome::NeedsOperator { spans, .. } => *spans,
        }
    }

    /// Whether the next cycle should back off.
    pub fn should_back_off(&self) -> bool {
        matches!(
            self,
            DrainOutcome::Failed { .. } | DrainOutcome::NeedsOperator { .. }
        )
    }
}

/// Next interval after an outcome.
///
/// Separated from the loop so the policy is testable without waiting on real
/// time — a sleeping test is a slow test and a flaky one.
pub fn next_interval(current: Duration, outcome: &DrainOutcome) -> Duration {
    match outcome {
        // Progress, or nothing to do: return to the poll rate immediately.
        // Staying backed off after a recovery would leave a queue draining
        // minutes slower than it could, and queue headroom is what stands
        // between an outage and a gap.
        DrainOutcome::Idle | DrainOutcome::Drained { .. } => IDLE_POLL_INTERVAL,
        // Not linked: there is nothing to poll for, but keep ticking so linking
        // (or re-enabling telemetry) is noticed without a restart.
        DrainOutcome::NotLinked => MAX_BACKOFF,
        DrainOutcome::Failed { .. } | DrainOutcome::NeedsOperator { .. } => {
            (current.max(BASE_BACKOFF) * 2).min(MAX_BACKOFF)
        }
    }
}

/// Drain the outbox until it is empty, a shipment fails, or `max_batches` have
/// been shipped.
///
/// `max_batches` bounds one cycle so a very deep queue cannot monopolise the
/// task forever and starve the shutdown signal. It is a fairness bound, not a
/// throughput bound: the loop re-enters immediately on the next tick because
/// [`next_interval`] returns the poll rate after progress.
pub async fn drain_until_idle(
    link: &CloudLink,
    outbox: &SpanOutbox,
    max_batches: usize,
) -> DrainOutcome {
    let mut shipped_spans = 0usize;
    let mut shipped_batches = 0usize;

    for _ in 0..max_batches {
        let claimed = match outbox.claim(OUTBOX_BATCH_SIZE).await {
            Ok(claimed) => claimed,
            Err(error) => {
                return DrainOutcome::Failed {
                    spans: shipped_spans,
                    batches: shipped_batches,
                    reason: error.to_string(),
                }
            }
        };

        if claimed.is_empty() {
            return if shipped_batches == 0 {
                DrainOutcome::Idle
            } else {
                DrainOutcome::Drained {
                    spans: shipped_spans,
                    batches: shipped_batches,
                }
            };
        }

        let ids: Vec<i64> = claimed.iter().map(|row| row.id).collect();
        let spans: Vec<temps_cloud_protocol::SpanRecord> =
            claimed.iter().map(|row| row.span.clone()).collect();
        let count = spans.len();

        // A fresh submission id per attempt-group. The rows themselves are the
        // durable record and the retry unit, so there is no cross-restart id to
        // preserve the way `flush`'s `pending_submission` has to.
        match link.ship_outbox_batch(Uuid::new_v4(), spans).await {
            OutboxShipOutcome::Shipped { spans, warning } => {
                if let Err(error) = outbox.mark_delivered(&ids).await {
                    // Cloud has the spans; we could not record that. Retrying is
                    // safe — `/v1/telemetry` is idempotent per submission id and
                    // Cloud dedupes — so leave the rows pending rather than
                    // guessing.
                    tracing::warn!(
                        rows = ids.len(),
                        %error,
                        "Temps Cloud accepted a telemetry batch but the acknowledgement could \
                         not be recorded; the rows will be retried"
                    );
                    return DrainOutcome::Failed {
                        spans: shipped_spans,
                        batches: shipped_batches,
                        reason: error.to_string(),
                    };
                }
                shipped_spans += spans;
                shipped_batches += 1;

                if let Some(detail) = warning {
                    // Accepted, but degraded. Stop the cycle and hand the
                    // condition up: continuing to drain into an exhausted quota
                    // is how a billing state becomes data loss.
                    return DrainOutcome::NeedsOperator {
                        spans: shipped_spans,
                        batches: shipped_batches,
                        detail,
                    };
                }
            }
            OutboxShipOutcome::Retained { reason, .. } => {
                let _ = outbox.record_attempt_failure(&ids, &reason).await;
                return DrainOutcome::Failed {
                    spans: shipped_spans,
                    batches: shipped_batches,
                    reason,
                };
            }
            OutboxShipOutcome::Blocked { reason, .. } => {
                let _ = outbox.record_attempt_failure(&ids, &reason).await;
                return DrainOutcome::Failed {
                    spans: shipped_spans,
                    batches: shipped_batches,
                    reason,
                };
            }
            OutboxShipOutcome::NotLinked => {
                // Roll the attempt back: an unlinked instance did not *fail* to
                // deliver, it was never asked to. Counting these would burn the
                // retry budget of every queued span during a disconnect and
                // dead-letter the queue for a reason that has nothing to do
                // with the spans.
                let _ = outbox.release_claim(&ids).await;
                return DrainOutcome::NotLinked;
            }
            OutboxShipOutcome::Idle => {
                let _ = outbox.release_claim(&ids).await;
                return DrainOutcome::Idle;
            }
        }

        // Defensive: an ack that reports fewer spans than were claimed is still
        // an ack for the batch, and the rows are already settled. Nothing to do
        // here beyond keeping the loop honest about what it counted.
        debug_assert!(count > 0);
    }

    DrainOutcome::Drained {
        spans: shipped_spans,
        batches: shipped_batches,
    }
}

/// Batches shipped per cycle before the loop yields to its select.
///
/// 40 × 500 = 20,000 spans, which at the load test's sustained rate is several
/// minutes of backlog per cycle — deep enough that a recovering instance
/// catches up quickly, bounded enough that a shutdown signal is never more than
/// one batch's round trip away from being seen.
const MAX_BATCHES_PER_CYCLE: usize = 40;

/// Reads the operator's byte cap from wherever the host keeps its settings.
///
/// A trait rather than a direct `ConfigService` dependency because
/// `temps-cloud-client` deliberately does not depend on `temps-config` — the
/// crate is meant to stay readable as "exactly what leaves the machine". The
/// host wires the real implementation at startup.
#[async_trait::async_trait]
pub trait OutboxCapSource: Send + Sync {
    /// Current cap in bytes, from the singleton `settings` row.
    ///
    /// Returning `None` means the setting could not be read; the worker keeps
    /// the cap it already has rather than falling back to a default, because
    /// silently widening or narrowing a data-loss boundary on a transient
    /// database blip is worse than being briefly stale.
    async fn outbox_max_bytes(&self) -> Option<u64>;
}

/// Notified of every drain outcome.
///
/// The worker itself knows nothing about projects or write modes — it moves
/// rows. ADR-041 §7b's decision, that a sustained refusal must close the `cloud`
/// interval and resume local writes rather than retrying until the queue
/// overflows, belongs to whoever owns those concepts. This is how that owner
/// hears about it, rather than the worker growing a dependency on the write
/// mode or the caller having to poll for a condition it can only infer.
#[async_trait::async_trait]
pub trait DrainObserver: Send + Sync {
    async fn on_outcome(&self, outcome: &DrainOutcome);
}

/// Run until cancelled. Spawn this once at instance startup, beside the mirror
/// flusher — the two serve different projects and neither replaces the other.
pub async fn run(
    link: Arc<CloudLink>,
    outbox: Arc<SpanOutbox>,
    cap_source: Option<Arc<dyn OutboxCapSource>>,
    observer: Option<Arc<dyn DrainObserver>>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let wake = outbox.wake_handle();
    let mut interval = IDLE_POLL_INTERVAL;
    let mut settings_refresh = tokio::time::interval(SETTINGS_REFRESH_INTERVAL);
    settings_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Re-read the true queue size before accepting anything: a restart starts
    // with a zeroed cache, and without this the first enqueue would happily
    // double the queue past its cap.
    if let Err(error) = outbox.resync().await {
        tracing::warn!(
            %error,
            "Could not read the Temps Cloud telemetry outbox size at startup; \
             the byte cap will be enforced from the next successful resync"
        );
    }

    tracing::info!(
        max_bytes = outbox.max_bytes(),
        batch_size = OUTBOX_BATCH_SIZE,
        "Cloud-primary telemetry outbox worker starting"
    );

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = wake.notified() => {}
            _ = settings_refresh.tick() => {
                refresh_settings(&outbox, cap_source.as_ref()).await;
                continue;
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    // Bounded final drain. Nothing is lost if it times out —
                    // the rows are durable, which is the entire point.
                    match tokio::time::timeout(
                        SHUTDOWN_DRAIN_TIMEOUT,
                        drain_until_idle(&link, &outbox, MAX_BATCHES_PER_CYCLE),
                    )
                    .await
                    {
                        Ok(outcome) => tracing::info!(
                            shipped = outcome.shipped_spans(),
                            "Cloud-primary telemetry outbox drained during shutdown"
                        ),
                        Err(_) => tracing::info!(
                            timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
                            "Shutdown drain of the Cloud-primary telemetry outbox timed out; \
                             queued spans are durable and will ship after restart"
                        ),
                    }
                    tracing::info!("Cloud-primary telemetry outbox worker stopped");
                    return;
                }
            }
        }

        // ADR-041 §7c: a feature-switch change queues an async fallback that
        // `set_feature_switches` (which is synchronous, and called from the
        // settings path) cannot run itself. Run it before draining, so the
        // drain sees the post-fallback queue.
        link.run_pending_telemetry_fallback().await;

        let outcome = drain_until_idle(&link, &outbox, MAX_BATCHES_PER_CYCLE).await;
        interval = next_interval(interval, &outcome);

        // Before logging, so the write-mode owner reacts to a quota refusal in
        // the same cycle it happened rather than one interval later.
        if let Some(observer) = observer.as_ref() {
            observer.on_outcome(&outcome).await;
        }

        match &outcome {
            DrainOutcome::Drained { spans, batches } => {
                tracing::debug!(spans, batches, "shipped queued telemetry to Temps Cloud");
            }
            DrainOutcome::Failed {
                spans,
                batches,
                reason,
            } => {
                tracing::warn!(
                    shipped_spans = spans,
                    shipped_batches = batches,
                    reason,
                    retry_in_secs = interval.as_secs(),
                    "Cloud-primary telemetry shipment failed; spans remain queued"
                );
            }
            DrainOutcome::NeedsOperator { detail, .. } => {
                tracing::error!(
                    detail = ?detail,
                    "Temps Cloud accepted telemetry but needs operator action; \
                     Cloud-primary projects may be falling back to local storage"
                );
            }
            DrainOutcome::Idle | DrainOutcome::NotLinked => {}
        }

        // Cheap: one `DELETE` against a partial index, and only when there is
        // something to remove. Keeping it in the same loop avoids a second task
        // and a second set of timers for a table this small.
        if matches!(outcome, DrainOutcome::Idle) {
            if let Err(error) = outbox.sweep_settled().await {
                tracing::warn!(%error, "Temps Cloud telemetry outbox sweep failed");
            }
            // A dead letter is kept as evidence; the span it carried is not kept
            // forever. Nulling the payload past its age bound leaves the row —
            // and therefore the operator's "N deliveries failed, here is why" —
            // intact.
            match outbox.redact_expired_dead_letters().await {
                Ok(0) => {}
                Ok(rows) => tracing::info!(
                    rows,
                    retention_days = DEAD_LETTER_PAYLOAD_RETENTION.as_secs() / 86_400,
                    "Removed the span content of dead-lettered Temps Cloud telemetry rows past \
                     their retention; the failure record itself is kept"
                ),
                Err(error) => tracing::warn!(
                    %error,
                    "Could not redact expired Temps Cloud telemetry dead letters"
                ),
            }
            // Rows whose project has been deleted are never claimed, so without
            // this they would sit in the queue consuming the operator's byte cap
            // for as long as the instance runs.
            match outbox.purge_orphaned().await {
                Ok(0) => {}
                Ok(rows) => tracing::info!(
                    rows,
                    "Removed queued Temps Cloud telemetry rows whose project no longer exists"
                ),
                Err(error) => tracing::warn!(
                    %error,
                    "Could not purge orphaned Temps Cloud telemetry outbox rows"
                ),
            }
            if let Err(error) = outbox.resync().await {
                tracing::warn!(%error, "Temps Cloud telemetry outbox resync failed");
            }
        }
    }
}

async fn refresh_settings(outbox: &SpanOutbox, cap_source: Option<&Arc<dyn OutboxCapSource>>) {
    let Some(source) = cap_source else {
        return;
    };
    if let Some(max_bytes) = source.outbox_max_bytes().await {
        if max_bytes != outbox.max_bytes() {
            tracing::info!(
                previous_bytes = outbox.max_bytes(),
                max_bytes,
                "Temps Cloud telemetry outbox cap changed"
            );
            outbox.set_max_bytes(max_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_returns_to_the_poll_rate_immediately() {
        // Queue headroom is what stands between an outage and a real gap, so a
        // recovered backend must not keep draining on a backed-off schedule.
        assert_eq!(
            next_interval(
                MAX_BACKOFF,
                &DrainOutcome::Drained {
                    spans: 500,
                    batches: 1
                }
            ),
            IDLE_POLL_INTERVAL
        );
        assert_eq!(
            next_interval(MAX_BACKOFF, &DrainOutcome::Idle),
            IDLE_POLL_INTERVAL
        );
    }

    #[test]
    fn a_failure_backs_off_and_is_capped() {
        let failed = DrainOutcome::Failed {
            spans: 0,
            batches: 0,
            reason: "unreachable".into(),
        };
        let mut interval = next_interval(IDLE_POLL_INTERVAL, &failed);
        assert_eq!(interval, BASE_BACKOFF * 2);
        for _ in 0..20 {
            interval = next_interval(interval, &failed);
        }
        assert_eq!(interval, MAX_BACKOFF, "backoff must be bounded");
    }

    #[test]
    fn the_primary_path_backs_off_faster_than_the_mirror() {
        // The mirror can afford 300 s because local storage still has
        // everything. The primary path cannot: every second not retrying is a
        // second closer to the cap.
        assert!(
            MAX_BACKOFF < crate::flusher::MAX_INTERVAL,
            "a primary write path must not idle as long as a best-effort mirror"
        );
    }

    #[test]
    fn an_unlinked_instance_still_ticks() {
        // Slowly — but it must tick, or re-linking would need a restart before
        // the queue moved.
        let interval = next_interval(IDLE_POLL_INTERVAL, &DrainOutcome::NotLinked);
        assert_eq!(interval, MAX_BACKOFF);
        assert!(interval < Duration::from_secs(3600));
    }

    #[test]
    fn partial_progress_before_a_failure_is_still_reported() {
        // "The cycle ended badly" and "nothing shipped" are different facts,
        // and collapsing them would make a recovering queue look stuck.
        let outcome = DrainOutcome::Failed {
            spans: 1_500,
            batches: 3,
            reason: "unreachable".into(),
        };
        assert_eq!(outcome.shipped_spans(), 1_500);
        assert!(outcome.should_back_off());
    }

    #[test]
    fn idle_and_not_linked_report_no_progress_and_are_distinguishable() {
        assert_eq!(DrainOutcome::Idle.shipped_spans(), 0);
        assert_eq!(DrainOutcome::NotLinked.shipped_spans(), 0);
        assert!(!DrainOutcome::Idle.should_back_off());
        assert!(!DrainOutcome::NotLinked.should_back_off());
        assert_ne!(DrainOutcome::Idle, DrainOutcome::NotLinked);
    }

    #[test]
    fn a_degraded_acceptance_backs_off_and_keeps_its_progress() {
        let outcome = DrainOutcome::NeedsOperator {
            spans: 500,
            batches: 1,
            detail: Unavailable::NotEnrolled,
        };
        assert_eq!(outcome.shipped_spans(), 500);
        assert!(outcome.should_back_off());
    }

    #[test]
    fn one_cycle_is_bounded_so_shutdown_is_never_starved() {
        // Both halves are compile-time facts: a cycle that claims nothing would
        // never drain, and one too shallow to hold a recovering instance's
        // backlog turns a Cloud outage into a permanently growing queue.
        const _: () = assert!(MAX_BATCHES_PER_CYCLE > 0);
        const _: () = assert!(
            MAX_BATCHES_PER_CYCLE * OUTBOX_BATCH_SIZE as usize >= 10_000,
            "a cycle must be deep enough for a recovering instance to catch up"
        );
    }
}
