// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Handing Cloud-primary projects back to local span storage (ADR-041 §7c).
//!
//! Runs when the link goes away — `DELETE /cloud`, or the Cloud telemetry
//! feature switch being turned off — and does three things, in this order:
//!
//! 1. **Bounded final drain.** Ship whatever the outbox can still deliver while
//!    the credential is alive. The flusher already has this shape at shutdown
//!    (`SHUTDOWN_FLUSH_TIMEOUT`); the same reasoning applies here, with one
//!    difference: after a disconnect there is no next start that could deliver
//!    it, so this is genuinely the last chance.
//! 2. **Spill.** Everything the drain could not ship is written to the local
//!    span store. Those are real spans and the local store is about to be
//!    primary again — dropping them would be the loudest possible violation of
//!    "there is never a state in which the instance is storing spans nowhere".
//! 3. **Flip.** Every Cloud-primary project moves to `write_mode = local` in one
//!    transaction, closing its `cloud` interval and opening a `local` one.
//!
//! # Why the order is drain → spill → flip, and not flip first
//!
//! Flipping first would be simpler and would lose data. The drain needs a live
//! credential; the credential is revoked immediately after this returns. Doing
//! the flip first buys nothing, because local span writes have *already*
//! resumed by the time this runs — the ingest path reads the link's own
//! `is_linked()`/`telemetry_enabled()` atomics, not the project row, precisely
//! so that resumption cannot be delayed by a transaction.
//!
//! # What is lost, honestly
//!
//! A span that reached the outbox is either delivered to Cloud or written
//! locally. Nothing in this path drops one. What *is* lost is visibility of
//! telemetry already in Cloud: it stays there, readable only while a link
//! exists, and disconnecting makes that window unreadable from this instance.
//! The confirmation dialog says so; this module cannot fix it, and pretending
//! otherwise (an automatic export-on-disconnect) is deliberately Phase C.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::TimeZone;
use temps_cloud_client::{
    CloudFallbackReason, CloudLink, CloudTelemetryFallback, SpanOutbox, OUTBOX_BATCH_SIZE,
};
use temps_entities::project_telemetry_write_intervals::TelemetryWriteIntervalReason;

use crate::services::telemetry_write_mode::{CloudWriteSuspension, TelemetryWriteModeService};
use crate::storage::OtelStorage;
use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};

/// Wall-clock budget for the final drain.
///
/// Bounded for the same reason the flusher's shutdown flush is: a disconnect is
/// a user action behind an HTTP request, and it must return. Ten seconds at
/// ~500 spans per round trip is a meaningful amount of backlog, and everything
/// it does not reach is spilled locally rather than lost.
pub const FINAL_DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// Wall-clock budget for the spill.
///
/// Separate from the drain's budget so a slow Cloud cannot eat the time the
/// *local* write needs — the spill is the step that decides whether spans exist
/// at all, so it must never be the one that gets squeezed.
pub const SPILL_BUDGET: Duration = Duration::from_secs(20);

/// Rows written to local storage per spill batch.
const SPILL_BATCH: u32 = OUTBOX_BATCH_SIZE;

/// Wall-clock budget for a spill triggered by an operator's own settings
/// change, rather than by a disconnect.
///
/// Shorter than [`SPILL_BUDGET`] because it runs inside an HTTP request the
/// operator is waiting on. It does not need the disconnect's headroom: a
/// settings change is not a last chance — nothing is revoked afterwards, so
/// anything the budget does not reach is still queued, still shippable, and
/// still caught by the fidelity-downgrade guard that refuses to withdraw
/// consent while rows remain.
pub const SETTINGS_SPILL_BUDGET: Duration = Duration::from_secs(5);

/// Writes queued Cloud-bound spans back into the local span store.
///
/// # Why this is its own type
///
/// Three unrelated events need exactly this behaviour — a disconnect
/// ([`CloudPrimaryFallback`]), an operator setting a project back to
/// `write_mode = local`, and an operator lowering a project's fidelity — and
/// two of those originate in [`TelemetryWriteModeService`], which
/// [`CloudPrimaryFallback`] already depends on. Extracting the spill is what
/// lets the write-mode service reuse it without an `Arc` cycle, and means there
/// is exactly one implementation of "get these spans onto local disk before
/// anything settles them".
pub struct OutboxSpiller {
    outbox: Arc<SpanOutbox>,
    /// The **local** store, never the routed decorator: this writes spans that
    /// are on their way back from Cloud, and routing them would send them
    /// straight back out again.
    local_storage: Arc<dyn OtelStorage>,
}

impl OutboxSpiller {
    pub fn new(outbox: Arc<SpanOutbox>, local_storage: Arc<dyn OtelStorage>) -> Self {
        Self {
            outbox,
            local_storage,
        }
    }

    /// Write everything still queued for `project_ids` into the local span
    /// store, then mark those rows spilled.
    ///
    /// The local write happens **before** the rows are settled. Settling first
    /// would mean a storage failure silently discarded the only copy.
    pub async fn spill(&self, project_ids: &[i32], budget: Duration) -> usize {
        if project_ids.is_empty() {
            return 0;
        }
        let deadline = Instant::now() + budget;
        let mut spilled = 0usize;

        while Instant::now() < deadline {
            let claimed = match self
                .outbox
                .pending_for_projects(project_ids, SPILL_BATCH)
                .await
            {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::error!(
                        %error,
                        "Could not read queued Cloud-primary spans to write them back to local \
                         storage; they remain queued and will be retried if the link returns"
                    );
                    break;
                }
            };
            if claimed.is_empty() {
                break;
            }

            let ids: Vec<i64> = claimed.iter().map(|row| row.id).collect();
            let spans: Vec<SpanRecord> = claimed
                .iter()
                .map(|row| rehydrate(row.project_id, &row.span))
                .collect();
            let count = spans.len();

            if let Err(error) = self.local_storage.store_spans(spans).await {
                tracing::error!(
                    spans = count,
                    %error,
                    "Could not write queued Cloud-primary spans back to local storage; they \
                     remain queued rather than being discarded"
                );
                break;
            }

            if let Err(error) = self.outbox.mark_spilled(&ids).await {
                // The spans are safely in local storage. Failing to mark them
                // means a later drain may ship them to Cloud as well, which is
                // duplication rather than loss — the safe side of this trade.
                tracing::warn!(
                    rows = ids.len(),
                    %error,
                    "Wrote queued spans to local storage but could not mark them spilled; they \
                     may also be delivered to Cloud if the link returns"
                );
                break;
            }
            spilled += count;
        }

        spilled
    }
}

#[async_trait]
impl crate::services::telemetry_write_mode::TelemetrySpiller for OutboxSpiller {
    async fn spill_projects(&self, project_ids: &[i32]) -> usize {
        self.spill(project_ids, SETTINGS_SPILL_BUDGET).await
    }
}

/// Turns the link's "I am going away" event into ledger and storage changes.
pub struct CloudPrimaryFallback {
    write_modes: Arc<TelemetryWriteModeService>,
    outbox: Arc<SpanOutbox>,
    spiller: Arc<OutboxSpiller>,
    link: Arc<CloudLink>,
}

impl CloudPrimaryFallback {
    pub fn new(
        write_modes: Arc<TelemetryWriteModeService>,
        outbox: Arc<SpanOutbox>,
        local_storage: Arc<dyn OtelStorage>,
        link: Arc<CloudLink>,
    ) -> Self {
        Self::with_spiller(
            write_modes,
            outbox.clone(),
            Arc::new(OutboxSpiller::new(outbox, local_storage)),
            link,
        )
    }

    /// Reuse a spiller the caller already built, so the disconnect path and the
    /// settings path share one instance rather than each holding their own
    /// handle on the same two dependencies.
    pub fn with_spiller(
        write_modes: Arc<TelemetryWriteModeService>,
        outbox: Arc<SpanOutbox>,
        spiller: Arc<OutboxSpiller>,
        link: Arc<CloudLink>,
    ) -> Self {
        Self {
            write_modes,
            outbox,
            spiller,
            link,
        }
    }

    /// Ship what we still can, within [`FINAL_DRAIN_BUDGET`].
    ///
    /// Errors are logged, not propagated: the spill below is what guarantees
    /// the spans survive, and a failed drain only means more of them take that
    /// route.
    async fn final_drain(&self) -> usize {
        let deadline = Instant::now() + FINAL_DRAIN_BUDGET;
        let mut shipped = 0usize;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let drained = tokio::time::timeout(
                remaining,
                temps_cloud_client::outbox_worker::drain_until_idle(&self.link, &self.outbox, 1),
            )
            .await;

            match drained {
                Ok(outcome) => {
                    shipped += outcome.shipped_spans();
                    if outcome.shipped_spans() == 0 {
                        // Idle, unlinked, or failing. Nothing more to gain from
                        // spending the rest of the budget on it.
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        shipped
    }

    /// Which projects need a local home for their queued spans.
    ///
    /// **Scoped by what is actually in the queue, not by declared write mode.**
    /// The two sets diverge, and the difference is a data-loss bug: a project
    /// switched back to `write_mode = local` while the link was still up
    /// correctly leaves its rows queued (the worker would have shipped them),
    /// and it no longer appears in `cloud_primary_project_ids()`. Scoping the
    /// spill by the declared mode alone would leave exactly those rows outside
    /// both the final drain and the spill, and after `disconnect()` nothing ever
    /// claims them again — neither delivered to Cloud nor written locally, which
    /// this module's own invariant says never happens.
    ///
    /// The declared-mode set is still unioned in so a Cloud-primary project with
    /// an empty queue is still counted and logged as affected, and so a race
    /// where a span is enqueued between the two reads is covered by the second.
    async fn spill_scope(&self, declared_cloud_primary: &[i32]) -> Vec<i32> {
        let mut ids: Vec<i32> = declared_cloud_primary.to_vec();
        match self.outbox.pending_project_ids().await {
            Ok(queued) => ids.extend(queued),
            Err(error) => tracing::error!(
                %error,
                "Could not read which projects still have telemetry queued while disconnecting; \
                 falling back to the declared Cloud-primary projects only, so rows belonging to a \
                 project already switched back to local storage may stay queued"
            ),
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

#[async_trait]
impl CloudTelemetryFallback for CloudPrimaryFallback {
    async fn revert_to_local(&self, reason: CloudFallbackReason) -> usize {
        let ledger_reason = match reason {
            CloudFallbackReason::Disconnected | CloudFallbackReason::TelemetryDisabled => {
                TelemetryWriteIntervalReason::CloudDisconnected
            }
        };

        // Which projects are affected has to be read *before* the flip, because
        // afterwards none of them is Cloud-primary any more.
        let project_ids = match self.write_modes.cloud_primary_project_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(
                    %error,
                    "Could not determine which projects are Cloud-primary while disconnecting; \
                     their queued spans stay in the outbox rather than being discarded"
                );
                Vec::new()
            }
        };

        let shipped = self.final_drain().await;
        // Read *after* the drain: anything the drain could not ship is exactly
        // what still needs a local home, and a project that drained to empty
        // does not need to be in the scope at all.
        let spill_scope = self.spill_scope(&project_ids).await;
        let spilled = self.spiller.spill(&spill_scope, SPILL_BUDGET).await;

        match self.write_modes.revert_all_to_local(ledger_reason).await {
            Ok(reverted) => {
                tracing::info!(
                    projects = ?reverted,
                    spill_scope = ?spill_scope,
                    shipped,
                    spilled,
                    reason = ?reason,
                    "Cloud-primary projects reverted to local span storage"
                );
                reverted.len()
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    shipped,
                    spilled,
                    "Could not revert Cloud-primary projects to local span storage. Span writes \
                     have already resumed locally because the link is gone, but the project rows \
                     still say `cloud` — set them back to `local` in project settings."
                );
                0
            }
        }
    }
}

/// Applies ADR-041 §7b: a sustained Cloud refusal closes the `cloud` interval
/// and resumes local span writes, and a recovery reopens it.
///
/// # Why this is a separate type from [`CloudPrimaryFallback`]
///
/// They fire on different events and do different things. The fallback runs
/// once, on a link that is about to disappear, and rewrites the operator's
/// declared intent. This runs on every drain cycle, on a link that is still
/// there, and deliberately leaves the declared intent alone — the operator did
/// not change their mind, Cloud stopped accepting. That distinction is the
/// whole reason the ledger exists (ADR-041 §1) and collapsing the two would
/// erase it.
pub struct CloudWriteSuspensionObserver {
    write_modes: Arc<TelemetryWriteModeService>,
}

impl CloudWriteSuspensionObserver {
    pub fn new(write_modes: Arc<TelemetryWriteModeService>) -> Self {
        Self { write_modes }
    }
}

#[async_trait]
impl temps_cloud_client::DrainObserver for CloudWriteSuspensionObserver {
    async fn on_outcome(&self, outcome: &temps_cloud_client::DrainOutcome) {
        use temps_cloud_client::DrainOutcome;
        use temps_cloud_protocol::Unavailable;

        let suspension = match outcome {
            DrainOutcome::NeedsOperator { detail, .. } => match detail {
                // The sharpest risk in the whole design. Under a mirror this
                // means "sampling"; under Cloud-primary writes it would mean
                // sampling away the only copy.
                Unavailable::QuotaExhausted { .. } | Unavailable::NotEntitled { .. } => {
                    Some(CloudWriteSuspension::QuotaExhausted)
                }
                Unavailable::NotEnrolled => Some(CloudWriteSuspension::CredentialRejected),
                // `Degraded` is explicitly transient and carries a retry hint.
                // Falling back for it would move a project's storage on every
                // backend hiccup, which is churn, not safety.
                _ => None,
            },
            _ => None,
        };

        if let Some(suspension) = suspension {
            let detail = match outcome {
                DrainOutcome::NeedsOperator { detail, .. } => format!("{detail:?}"),
                _ => String::new(),
            };
            if let Err(error) = self
                .write_modes
                .suspend_cloud_writes(suspension, detail)
                .await
            {
                tracing::error!(
                    %error,
                    "Temps Cloud refused telemetry, but this instance could not record the \
                     fallback to local span storage. Spans are being written locally regardless \
                     — the ingest path checks the suspension flag, which is already set."
                );
            }
            return;
        }

        // A clean shipment is the only evidence that Cloud is accepting again.
        // `Idle` is not: an empty queue proves nothing about whether the next
        // batch would be accepted.
        if matches!(outcome, DrainOutcome::Drained { spans, .. } if *spans > 0) {
            if let Err(error) = self.write_modes.resume_cloud_writes().await {
                tracing::error!(
                    %error,
                    "Temps Cloud is accepting telemetry again, but this instance could not \
                     reopen the Cloud-primary write intervals. Spans keep going to local \
                     storage, which is safe but not what the project settings ask for."
                );
            }
        }
    }
}

/// Rebuild a local span from the Cloud projection.
///
/// Lossy by construction and honestly so: the projection carries what the
/// project consented to ship, and fields it never carried (span events, the
/// non-allowlisted attributes, the deployment id) are left empty rather than
/// invented. A span with fewer attributes is a true record of what this
/// instance had; a span with fabricated ones is not.
fn rehydrate(project_id: i32, span: &temps_cloud_protocol::SpanRecord) -> SpanRecord {
    let start_time = chrono::Utc
        .timestamp_millis_opt(span.ts_millis)
        .single()
        .unwrap_or_else(chrono::Utc::now);
    SpanRecord {
        project_id,
        deployment_id: None,
        resource: ResourceInfo {
            service_name: span
                .service_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            deployment_environment: span.environment.clone(),
            ..Default::default()
        },
        trace_id: span.trace_id.clone(),
        span_id: span.span_id.clone(),
        parent_span_id: span.parent_span_id.clone(),
        name: span.name.clone(),
        kind: span
            .span_kind
            .as_deref()
            .map(parse_kind)
            .unwrap_or(SpanKind::Internal),
        start_time,
        end_time: start_time + chrono::Duration::microseconds((span.duration_ms * 1_000.0) as i64),
        duration_ms: span.duration_ms,
        status_code: span
            .status_code
            .as_deref()
            .map(parse_status)
            .unwrap_or(SpanStatusCode::Unset),
        status_message: String::new(),
        attributes: span
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        events: Vec::new(),
    }
}

fn parse_kind(value: &str) -> SpanKind {
    match value.to_ascii_lowercase().as_str() {
        "server" => SpanKind::Server,
        "client" => SpanKind::Client,
        "producer" => SpanKind::Producer,
        "consumer" => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}

fn parse_status(value: &str) -> SpanStatusCode {
    match value.to_ascii_lowercase().as_str() {
        "ok" => SpanStatusCode::Ok,
        "error" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queryable_span() -> temps_cloud_protocol::SpanRecord {
        temps_cloud_protocol::SpanRecord {
            trace_id: "abc123".into(),
            span_id: "def456".into(),
            name: "GET /orders".into(),
            ts_millis: 1_700_000_000_000,
            duration_ms: 12.5,
            attributes: [("http.route".to_string(), "/orders".to_string())]
                .into_iter()
                .collect(),
            project_ref: "pseudonym".into(),
            service_name: Some("api".into()),
            span_kind: Some("SERVER".into()),
            status_code: Some("ERROR".into()),
            parent_span_id: Some("parent1".into()),
            environment: Some("production".into()),
        }
    }

    #[test]
    fn a_spilled_span_keeps_everything_the_projection_carried() {
        let span = rehydrate(42, &queryable_span());

        assert_eq!(span.project_id, 42, "the local project id is restored");
        assert_eq!(span.trace_id, "abc123");
        assert_eq!(span.span_id, "def456");
        assert_eq!(span.parent_span_id.as_deref(), Some("parent1"));
        assert_eq!(span.name, "GET /orders");
        assert_eq!(span.kind, SpanKind::Server);
        assert_eq!(span.status_code, SpanStatusCode::Error);
        assert_eq!(span.resource.service_name, "api");
        assert_eq!(
            span.resource.deployment_environment.as_deref(),
            Some("production")
        );
        assert_eq!(
            span.attributes.get("http.route").map(String::as_str),
            Some("/orders")
        );
        assert_eq!(span.duration_ms, 12.5);
    }

    #[test]
    fn a_spilled_span_invents_nothing_the_projection_did_not_carry() {
        // A span with fewer attributes is a true record of what this instance
        // had. A span with fabricated ones is not, and would be indistinguishable
        // from real data forever after.
        let span = rehydrate(42, &queryable_span());
        assert!(span.events.is_empty());
        assert!(span.deployment_id.is_none());
        assert!(span.status_message.is_empty());
    }

    #[test]
    fn a_metered_span_still_spills_rather_than_being_discarded() {
        // A `Metered` record should never reach the outbox — the §1 gate makes
        // Cloud-primary at `metered` unreachable — but if one did, it is still a
        // row this instance accepted, and writing a sparse span beats dropping
        // it silently.
        let metered = temps_cloud_protocol::SpanRecord {
            trace_id: "hmac-trace".into(),
            span_id: "hmac-span".into(),
            name: "span".into(),
            ts_millis: 1_700_000_000_000,
            duration_ms: 1.0,
            ..Default::default()
        };
        let span = rehydrate(7, &metered);

        assert_eq!(span.name, "span");
        assert_eq!(span.resource.service_name, "unknown");
        assert_eq!(span.kind, SpanKind::Internal);
        assert_eq!(span.status_code, SpanStatusCode::Unset);
    }

    #[test]
    fn an_out_of_range_timestamp_does_not_panic_or_produce_a_nonsense_time() {
        let mut span = queryable_span();
        span.ts_millis = i64::MAX;
        let rehydrated = rehydrate(7, &span);
        // Falls back to "now" rather than panicking or wrapping into the year
        // 262143, which would silently place the span outside every query
        // window the operator will ever ask for.
        assert!(rehydrated.start_time <= chrono::Utc::now());
    }

    #[test]
    fn the_spill_budget_is_not_squeezed_by_the_drain_budget() {
        // The spill decides whether the spans exist at all; the drain only
        // decides where. If they shared a budget, a slow Cloud would eat the
        // time the local write needs.
        assert!(SPILL_BUDGET > FINAL_DRAIN_BUDGET);
    }

    #[test]
    fn both_disconnect_reasons_close_the_interval_with_the_same_explanation() {
        // "You disconnected Cloud" and "you turned the telemetry switch off"
        // have the same consequence and the same fix, so they read the same in
        // the ledger rather than needing two near-identical strings.
        for reason in [
            CloudFallbackReason::Disconnected,
            CloudFallbackReason::TelemetryDisabled,
        ] {
            let ledger = match reason {
                CloudFallbackReason::Disconnected | CloudFallbackReason::TelemetryDisabled => {
                    TelemetryWriteIntervalReason::CloudDisconnected
                }
            };
            assert!(ledger.is_involuntary());
            assert!(ledger.message().contains("this instance"));
        }
    }
}
