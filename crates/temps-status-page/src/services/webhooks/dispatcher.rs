// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background delivery of status page webhooks (ADR 108 D2).
//!
//! Every [`TICK`] the dispatcher:
//!
//! 1. queues, for every enabled endpoint of every served page, the page's
//!    events published since the endpoint was registered and within
//!    [`NOTICE_WINDOW`] (`ON CONFLICT DO NOTHING`, so an event is queued once
//!    per endpoint even across restarts);
//! 2. posts up to [`DELIVERIES_PER_TICK`] due deliveries, [`CONCURRENCY`] at
//!    a time, each bounded by the transport's timeout;
//! 3. now and then deletes finished deliveries older than 30 days.
//!
//! Before each group of [`CONCURRENCY`] requests the endpoints and the page
//! are read again, so a delivery whose event left the public page, or whose
//! endpoint was disabled or deleted meanwhile, is cancelled rather than sent,
//! and an edited URL is used at once. Each attempt is claimed in the database
//! before the request goes out, so two consoles sharing a database never send
//! the same attempt twice. A failed attempt is retried after
//! [`RETRY_DELAYS`]; after [`MAX_ATTEMPTS`] it is marked failed.
//!
//! Memory: the set of queued keys (trimmed every tick to the live events),
//! [`WEBHOOKS_PER_QUERY`] endpoint rows and at most [`INSERT_CHUNK`] payloads
//! being queued at a time.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use temps_core::UtcDateTime;
use temps_entities::{status_page_webhook_deliveries, status_page_webhooks};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::error::WebhookError;
use super::payload::{payload_for, signature, EVENT_HEADER, EVENT_ID_HEADER, SIGNATURE_HEADER};
use super::service::StatusPageWebhookService;
use super::transport::TransportError;
use crate::services::public_pages::PublishedSnapshot;
use crate::services::subscribers::notices::NOTICE_WINDOW;

pub const TICK: Duration = Duration::from_secs(20);
pub const MAX_ATTEMPTS: i32 = 3;
/// Delay before the second and third attempt.
pub const RETRY_DELAYS: [chrono::Duration; 2] =
    [chrono::Duration::minutes(1), chrono::Duration::minutes(10)];
pub const DELIVERIES_PER_TICK: u64 = 40;
pub const CONCURRENCY: usize = 8;
const RETENTION_DAYS: i64 = 30;
const PRUNE_EVERY_TICKS: u64 = 30;
const MAX_ERROR_CHARS: usize = 300;
/// Deliveries queued per insert statement; each binds six values, far below
/// PostgreSQL's 65,535 bind limit. Tiny under test, so a handful of events
/// already spans several statements.
pub const INSERT_CHUNK: usize = if cfg!(test) { 2 } else { 1_000 };
/// Endpoints read per query while queueing.
pub const WEBHOOKS_PER_QUERY: u64 = if cfg!(test) { 2 } else { 200 };
/// How long a claimed attempt is reserved for the dispatcher that claimed
/// it. An attempt takes at most the transport's timeout; a dispatcher that
/// stopped mid-attempt leaves it to be retried after this.
pub const CLAIM_LEASE: chrono::Duration = chrono::Duration::minutes(5);

#[derive(Default)]
pub struct WebhookDispatcherState {
    /// `(webhook id, event key)` already queued; trimmed every tick.
    queued: HashSet<(i32, String)>,
    ticks: u64,
    /// Most payloads held at once while queueing, for tests.
    peak_buffered: usize,
}

impl WebhookDispatcherState {
    /// Most payloads held in memory at once while queueing.
    pub fn peak_buffered(&self) -> usize {
        self.peak_buffered
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct WebhookTickReport {
    pub queued: usize,
    pub delivered: usize,
    pub retried: usize,
    pub failed: usize,
    pub cancelled: usize,
}

/// What one attempt led to.
enum Attempt {
    Delivered,
    Failed(String),
}

fn bounded(reason: &str) -> String {
    let mut out: String = reason.chars().take(MAX_ERROR_CHARS).collect();
    if reason.chars().count() > MAX_ERROR_CHARS {
        out.push('…');
    }
    out
}

impl StatusPageWebhookService {
    /// Run the dispatcher forever. Errors are logged and retried next tick.
    pub async fn run_dispatcher(self: Arc<Self>) {
        let mut state = WebhookDispatcherState::default();
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match self.dispatch_tick(&mut state, Utc::now()).await {
                Ok(report) if report != WebhookTickReport::default() => {
                    info!(?report, "status page webhooks dispatched")
                }
                Ok(_) => {}
                Err(error) => warn!(
                    error = %error,
                    "status page webhook dispatch failed; retrying next tick"
                ),
            }
        }
    }

    pub async fn dispatch_tick(
        &self,
        state: &mut WebhookDispatcherState,
        now: UtcDateTime,
    ) -> Result<WebhookTickReport, WebhookError> {
        let mut report = WebhookTickReport::default();
        state.ticks += 1;
        if state.ticks % PRUNE_EVERY_TICKS == 1 {
            self.prune(now).await?;
        }
        let snapshots: HashMap<i32, Arc<PublishedSnapshot>> = self
            .cache
            .all()
            .into_iter()
            .map(|snapshot| (snapshot.status_page_id, snapshot))
            .collect();
        report.queued = self.queue(state, &snapshots, now).await?;
        self.deliver(now, &mut report).await?;
        Ok(report)
    }

    async fn queue(
        &self,
        state: &mut WebhookDispatcherState,
        snapshots: &HashMap<i32, Arc<PublishedSnapshot>>,
        now: UtcDateTime,
    ) -> Result<usize, WebhookError> {
        if snapshots.is_empty() {
            state.queued.clear();
            return Ok(0);
        }
        let mut live = HashSet::new();
        let mut rows = Vec::with_capacity(INSERT_CHUNK);
        let mut count = 0;
        let mut after = 0;
        // Endpoints a page at a time and payloads an insert at a time: a
        // backlog after a restart (many pages, endpoints and recent events)
        // never sits in memory at once.
        loop {
            let webhooks = status_page_webhooks::Entity::find()
                .filter(status_page_webhooks::Column::Enabled.eq(true))
                .filter(status_page_webhooks::Column::StatusPageId.is_in(snapshots.keys().copied()))
                .filter(status_page_webhooks::Column::Id.gt(after))
                .order_by_asc(status_page_webhooks::Column::Id)
                .limit(WEBHOOKS_PER_QUERY)
                .all(self.db.as_ref())
                .await?;
            let Some(last) = webhooks.last() else {
                break;
            };
            after = last.id;
            let fetched = webhooks.len() as u64;
            for webhook in &webhooks {
                let Some(snapshot) = snapshots.get(&webhook.status_page_id) else {
                    continue;
                };
                let since = webhook.created_at.max(now - NOTICE_WINDOW);
                for event in snapshot
                    .events
                    .iter()
                    .filter(|e| e.published_at >= since && e.published_at <= now)
                {
                    let key = (webhook.id, event.key.clone());
                    if !state.queued.contains(&key) {
                        let payload = payload_for(&snapshot.slug, &snapshot.title, event);
                        match serde_json::to_string(&payload) {
                            Ok(body) => rows.push(status_page_webhook_deliveries::ActiveModel {
                                webhook_id: Set(webhook.id),
                                event_key: Set(event.key.clone()),
                                payload: Set(body),
                                attempts: Set(0),
                                next_attempt_at: Set(now),
                                created_at: Set(now),
                                ..Default::default()
                            }),
                            Err(error) => {
                                warn!(
                                    webhook_id = webhook.id,
                                    error = %error,
                                    "failed to serialize a status page webhook payload"
                                );
                                continue;
                            }
                        }
                        state.peak_buffered = state.peak_buffered.max(rows.len());
                        if rows.len() >= INSERT_CHUNK {
                            count += rows.len();
                            self.insert_deliveries(std::mem::take(&mut rows)).await?;
                        }
                    }
                    live.insert(key);
                }
            }
            if fetched < WEBHOOKS_PER_QUERY {
                break;
            }
        }
        count += rows.len();
        self.insert_deliveries(rows).await?;
        state.queued = live;
        Ok(count)
    }

    /// One statement per call: PostgreSQL binds at most 65,535 values, so a
    /// batch stays at [`INSERT_CHUNK`] rows.
    async fn insert_deliveries(
        &self,
        rows: Vec<status_page_webhook_deliveries::ActiveModel>,
    ) -> Result<(), WebhookError> {
        if rows.is_empty() {
            return Ok(());
        }
        status_page_webhook_deliveries::Entity::insert_many(rows)
            .on_conflict(
                OnConflict::columns([
                    status_page_webhook_deliveries::Column::WebhookId,
                    status_page_webhook_deliveries::Column::EventKey,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    async fn deliver(
        &self,
        now: UtcDateTime,
        report: &mut WebhookTickReport,
    ) -> Result<(), WebhookError> {
        let due = status_page_webhook_deliveries::Entity::find()
            .filter(status_page_webhook_deliveries::Column::DeliveredAt.is_null())
            .filter(status_page_webhook_deliveries::Column::FailedAt.is_null())
            .filter(status_page_webhook_deliveries::Column::NextAttemptAt.lte(now))
            .order_by_asc(status_page_webhook_deliveries::Column::NextAttemptAt)
            .order_by_asc(status_page_webhook_deliveries::Column::Id)
            .limit(DELIVERIES_PER_TICK)
            .all(self.db.as_ref())
            .await?;

        for group in due.chunks(CONCURRENCY) {
            // Read the endpoints and the page again for every group: a change
            // made while the previous group was sending applies to this one.
            let ids: Vec<i32> = group.iter().map(|delivery| delivery.webhook_id).collect();
            let webhooks: HashMap<i32, status_page_webhooks::Model> =
                status_page_webhooks::Entity::find()
                    .filter(status_page_webhooks::Column::Id.is_in(ids))
                    .all(self.db.as_ref())
                    .await?
                    .into_iter()
                    .map(|webhook| (webhook.id, webhook))
                    .collect();
            let snapshots: HashMap<i32, Arc<PublishedSnapshot>> = self
                .cache
                .all()
                .into_iter()
                .map(|snapshot| (snapshot.status_page_id, snapshot))
                .collect();
            let mut set = JoinSet::new();
            let mut started = Vec::with_capacity(group.len());
            for delivery in group {
                let Some(webhook) = webhooks.get(&delivery.webhook_id) else {
                    // Deleted: its deliveries go with it.
                    continue;
                };
                // The body is rebuilt from the event as the page shows it now,
                // so a retry never sends content the page has since changed or
                // removed (a component taken off the page, an edited title).
                let current = snapshots.get(&webhook.status_page_id).and_then(|snapshot| {
                    snapshot
                        .events
                        .iter()
                        .find(|event| event.key == delivery.event_key)
                        .and_then(|event| {
                            serde_json::to_string(&payload_for(
                                &snapshot.slug,
                                &snapshot.title,
                                event,
                            ))
                            .ok()
                        })
                });
                let cancel_reason = if !webhook.enabled {
                    Some("the webhook was disabled")
                } else if current.is_none() {
                    Some("the event is no longer on the public page")
                } else if delivery.attempts >= MAX_ATTEMPTS {
                    Some("the last attempt was interrupted before its result was recorded")
                } else {
                    None
                };
                if let Some(reason) = cancel_reason {
                    status_page_webhook_deliveries::Entity::update_many()
                        .col_expr(
                            status_page_webhook_deliveries::Column::FailedAt,
                            Expr::value(now),
                        )
                        .col_expr(
                            status_page_webhook_deliveries::Column::LastError,
                            Expr::value(reason),
                        )
                        .filter(status_page_webhook_deliveries::Column::Id.eq(delivery.id))
                        .filter(status_page_webhook_deliveries::Column::DeliveredAt.is_null())
                        .filter(status_page_webhook_deliveries::Column::FailedAt.is_null())
                        .exec(self.db.as_ref())
                        .await?;
                    report.cancelled += 1;
                    continue;
                }
                if !self.claim(delivery, now).await? {
                    // Another dispatcher took this attempt.
                    continue;
                }
                let secret = match self.encryption.decrypt_string(&webhook.secret_encrypted) {
                    Ok(secret) => secret,
                    Err(error) => {
                        warn!(
                            webhook_id = webhook.id,
                            error = %error,
                            "cannot decrypt a status page webhook secret; not sending"
                        );
                        self.record(
                            delivery,
                            webhook,
                            now,
                            Attempt::Failed(
                                "the signing secret cannot be read; recreate the webhook"
                                    .to_string(),
                            ),
                            true,
                            report,
                        )
                        .await?;
                        continue;
                    }
                };
                let body = current.unwrap_or_default();
                let event_id = serde_json::from_str::<super::payload::WebhookPayload>(&body)
                    .map(|p| (p.kind, p.event.id))
                    .unwrap_or_default();
                let headers = vec![
                    (SIGNATURE_HEADER, signature(&secret, now.timestamp(), &body)),
                    (EVENT_HEADER, event_id.0),
                    (EVENT_ID_HEADER, event_id.1),
                ];
                let transport = self.transport.clone();
                let url = webhook.url.clone();
                let index = started.len();
                started.push((delivery, webhook));
                set.spawn(async move { (index, transport.post(&url, headers, body).await) });
            }
            let mut outcomes: Vec<(usize, Result<u16, TransportError>)> =
                Vec::with_capacity(started.len());
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(outcome) => outcomes.push(outcome),
                    Err(error) => warn!(error = %error, "a webhook delivery task failed"),
                }
            }
            for (index, outcome) in outcomes {
                let Some((delivery, webhook)) = started.get(index) else {
                    continue;
                };
                let attempt = match outcome {
                    Ok(status) if (200..300).contains(&status) => Attempt::Delivered,
                    Ok(status) => Attempt::Failed(format!("the endpoint answered HTTP {status}")),
                    Err(error) => Attempt::Failed(error.to_string()),
                };
                self.record(delivery, webhook, now, attempt, false, report)
                    .await?;
            }
        }
        Ok(())
    }

    /// Reserve the next attempt of `delivery` for this dispatcher. `false`
    /// when another dispatcher already took it or the delivery finished.
    async fn claim(
        &self,
        delivery: &status_page_webhook_deliveries::Model,
        now: UtcDateTime,
    ) -> Result<bool, WebhookError> {
        let claimed = status_page_webhook_deliveries::Entity::update_many()
            .col_expr(
                status_page_webhook_deliveries::Column::Attempts,
                Expr::value(delivery.attempts + 1),
            )
            .col_expr(
                status_page_webhook_deliveries::Column::NextAttemptAt,
                Expr::value(now + CLAIM_LEASE),
            )
            .filter(status_page_webhook_deliveries::Column::Id.eq(delivery.id))
            .filter(status_page_webhook_deliveries::Column::Attempts.eq(delivery.attempts))
            .filter(status_page_webhook_deliveries::Column::NextAttemptAt.lte(now))
            .filter(status_page_webhook_deliveries::Column::DeliveredAt.is_null())
            .filter(status_page_webhook_deliveries::Column::FailedAt.is_null())
            .exec(self.db.as_ref())
            .await?;
        Ok(claimed.rows_affected == 1)
    }

    async fn record(
        &self,
        delivery: &status_page_webhook_deliveries::Model,
        webhook: &status_page_webhooks::Model,
        now: UtcDateTime,
        attempt: Attempt,
        final_attempt: bool,
        report: &mut WebhookTickReport,
    ) -> Result<(), WebhookError> {
        // The attempt this dispatcher claimed; only its own claim is written.
        let attempts = delivery.attempts + 1;
        let mut update = status_page_webhook_deliveries::Entity::update_many()
            .filter(status_page_webhook_deliveries::Column::Id.eq(delivery.id))
            .filter(status_page_webhook_deliveries::Column::Attempts.eq(attempts));
        let mut hook = status_page_webhooks::Entity::update_many()
            .col_expr(
                status_page_webhooks::Column::LastAttemptAt,
                Expr::value(now),
            )
            .filter(status_page_webhooks::Column::Id.eq(webhook.id));
        match attempt {
            Attempt::Delivered => {
                update = update.col_expr(
                    status_page_webhook_deliveries::Column::DeliveredAt,
                    Expr::value(now),
                );
                hook = hook
                    .col_expr(
                        status_page_webhooks::Column::LastSuccessAt,
                        Expr::value(now),
                    )
                    .col_expr(
                        status_page_webhooks::Column::LastFailureReason,
                        Expr::value(Option::<String>::None),
                    );
                debug!(
                    webhook_id = webhook.id,
                    delivery_id = delivery.id,
                    "webhook delivered"
                );
                report.delivered += 1;
            }
            Attempt::Failed(reason) => {
                let reason = bounded(&reason);
                update = update.col_expr(
                    status_page_webhook_deliveries::Column::LastError,
                    Expr::value(reason.clone()),
                );
                let retry = (!final_attempt && attempts < MAX_ATTEMPTS)
                    .then(|| RETRY_DELAYS.get((attempts - 1) as usize).copied())
                    .flatten();
                match retry {
                    Some(delay) => {
                        update = update.col_expr(
                            status_page_webhook_deliveries::Column::NextAttemptAt,
                            Expr::value(now + delay),
                        );
                        report.retried += 1;
                    }
                    None => {
                        update = update.col_expr(
                            status_page_webhook_deliveries::Column::FailedAt,
                            Expr::value(now),
                        );
                        report.failed += 1;
                    }
                }
                warn!(
                    webhook_id = webhook.id,
                    status_page_id = webhook.status_page_id,
                    attempts,
                    reason = %reason,
                    "status page webhook delivery failed"
                );
                hook = hook.col_expr(
                    status_page_webhooks::Column::LastFailureReason,
                    Expr::value(reason),
                );
            }
        }
        update.exec(self.db.as_ref()).await?;
        hook.exec(self.db.as_ref()).await?;
        Ok(())
    }

    async fn prune(&self, now: UtcDateTime) -> Result<(), WebhookError> {
        let cutoff = now - chrono::Duration::days(RETENTION_DAYS);
        status_page_webhook_deliveries::Entity::delete_many()
            .filter(
                Condition::any()
                    .add(status_page_webhook_deliveries::Column::DeliveredAt.lt(cutoff))
                    .add(status_page_webhook_deliveries::Column::FailedAt.lt(cutoff)),
            )
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }
}
