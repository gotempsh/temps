// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background delivery of subscriber mail (ADR 108 D2).
//!
//! Every [`TICK`] the dispatcher:
//!
//! 1. queues the notices of every served snapshot with subscriptions on into
//!    `status_page_notices` (`ON CONFLICT DO NOTHING`, so a restart never
//!    queues one twice; an in-memory set skips keys already queued);
//! 2. sends pending confirmation mails, minting each token at send time;
//! 3. delivers open notices to confirmed subscribers in id order from each
//!    notice's cursor, advancing the cursor after every send;
//! 4. now and then deletes expired unconfirmed requests and old notices.
//!
//! Bounds: at most [`SENDS_PER_TICK`] mails per tick in total (about 36,000
//! an hour), [`NOTICES_PER_TICK`] notices and [`CONFIRMATIONS_PER_TICK`]
//! confirmations looked at per tick. Queries are per tick, not per page or
//! per subscriber, apart from one recipient batch per open notice.
//!
//! Delivery is at most once per recipient and notice: the cursor moves past
//! a recipient after one send, and a crash between the send and the cursor
//! write repeats at most that one mail. A notice whose update is no longer on
//! the public page (made private, deleted, the page unpublished or
//! subscriptions switched off) is cancelled, so mail never outruns the page.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use temps_core::UtcDateTime;
use temps_entities::{status_page_notices, status_page_subscribers, status_pages};
use tracing::{debug, info, warn};

use super::error::SubscriptionError;
use super::mailer::OutgoingMail;
use super::notices::{confirmation_message, with_footer};
use super::service::{PageLinks, StatusPageSubscriberService, CONFIRMATION_VALID_HOURS};
use super::tokens::{random_hex, token_hash, unsubscribe_token};
use crate::services::public_pages::PublishedSnapshot;

/// Time between dispatcher runs.
pub const TICK: Duration = Duration::from_secs(20);
/// Most mails sent per tick, all pages together.
pub const SENDS_PER_TICK: usize = 200;
/// Most recipients fetched per notice per tick.
const RECIPIENT_BATCH: u64 = 50;
/// Most open notices looked at per tick.
const NOTICES_PER_TICK: u64 = 20;
/// Most pending confirmations looked at per tick.
const CONFIRMATIONS_PER_TICK: u64 = 50;
/// Completed notices are kept this long, for the record, then deleted.
const NOTICE_RETENTION_DAYS: i64 = 30;
/// Prune every this many ticks (about ten minutes).
const PRUNE_EVERY_TICKS: u64 = 30;
/// A recipient whose mail failed this many times in a row is skipped, so one
/// bad address cannot hold a notice back from everyone after it.
const MAX_ATTEMPTS_PER_RECIPIENT: u8 = 3;

/// A page that is served with subscriptions on, and what mail needs from it.
struct LivePage {
    snapshot: Arc<PublishedSnapshot>,
    sender: Option<String>,
    links: Option<PageLinks>,
}

#[derive(Default)]
pub struct DispatcherState {
    /// `(page id, notice key)` already queued; trimmed to the live notices
    /// every tick, so it never outgrows them.
    queued: HashSet<(i32, String)>,
    /// `(notice id, subscriber id)` -> consecutive failed sends.
    failures: HashMap<(i32, i32), u8>,
    ticks: u64,
}

/// What one tick did, for logs and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub queued: usize,
    pub confirmations_sent: usize,
    pub notices_sent: usize,
    pub notices_completed: usize,
    pub notices_cancelled: usize,
    pub failures: usize,
}

impl StatusPageSubscriberService {
    /// Run the dispatcher forever. Errors are logged and retried next tick.
    pub async fn run_dispatcher(self: Arc<Self>) {
        let mut state = DispatcherState::default();
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match self.dispatch_tick(&mut state, Utc::now()).await {
                Ok(report) if report != TickReport::default() => {
                    info!(?report, "status page subscriber mail dispatched")
                }
                Ok(_) => {}
                Err(error) => warn!(
                    error = %error,
                    "status page subscriber dispatch failed; retrying next tick"
                ),
            }
        }
    }

    async fn live_pages(&self) -> Result<HashMap<i32, LivePage>, SubscriptionError> {
        let snapshots: HashMap<i32, Arc<PublishedSnapshot>> = self
            .cache
            .all()
            .into_iter()
            .filter(|snapshot| snapshot.accepts_subscriptions())
            .map(|snapshot| (snapshot.status_page_id, snapshot))
            .collect();
        if snapshots.is_empty() {
            return Ok(HashMap::new());
        }
        let senders: Vec<(i32, Option<String>)> = status_pages::Entity::find()
            .select_only()
            .column(status_pages::Column::Id)
            .column(status_pages::Column::SubscriptionSender)
            .filter(status_pages::Column::SubscriptionsEnabled.eq(true))
            .filter(status_pages::Column::Id.is_in(snapshots.keys().copied()))
            .into_tuple()
            .all(self.db.as_ref())
            .await?;
        let base = self.public_base().await?;
        let mut snapshots = snapshots;
        Ok(senders
            .into_iter()
            .filter_map(|(id, sender)| {
                let snapshot = snapshots.remove(&id)?;
                let links = PageLinks::for_snapshot(&snapshot, base.as_deref());
                Some((
                    id,
                    LivePage {
                        snapshot,
                        sender,
                        links,
                    },
                ))
            })
            .collect())
    }

    /// One pass of the dispatcher.
    pub async fn dispatch_tick(
        &self,
        state: &mut DispatcherState,
        now: UtcDateTime,
    ) -> Result<TickReport, SubscriptionError> {
        let mut report = TickReport::default();
        state.ticks += 1;
        if state.ticks % PRUNE_EVERY_TICKS == 1 {
            self.prune(now).await?;
        }
        let pages = self.live_pages().await?;
        report.queued = self.queue_notices(state, &pages, now).await?;
        if pages.is_empty() {
            return Ok(report);
        }

        // Readiness per sender, asked once per tick.
        let mut ready: HashMap<String, bool> = HashMap::new();
        for page in pages.values() {
            if let Some(sender) = &page.sender {
                if !ready.contains_key(sender) {
                    let readiness = self.mailer.readiness(Some(sender)).await?;
                    if let Some(problem) = &readiness.problem {
                        warn!(
                            status_page_id = page.snapshot.status_page_id,
                            reason = %problem.reason,
                            "status page subscriber mail is waiting: the sender cannot send"
                        );
                    }
                    ready.insert(sender.clone(), readiness.problem.is_none());
                }
            }
        }
        let can_send = |page: &LivePage| {
            page.links.is_some()
                && page
                    .sender
                    .as_ref()
                    .is_some_and(|sender| ready.get(sender).copied().unwrap_or(false))
        };

        let mut budget = SENDS_PER_TICK;
        self.send_confirmations(&pages, &can_send, now, &mut budget, &mut report)
            .await?;
        self.deliver_notices(state, &pages, &can_send, now, &mut budget, &mut report)
            .await?;
        Ok(report)
    }

    async fn queue_notices(
        &self,
        state: &mut DispatcherState,
        pages: &HashMap<i32, LivePage>,
        now: UtcDateTime,
    ) -> Result<usize, SubscriptionError> {
        let live: HashSet<(i32, String)> = pages
            .values()
            .flat_map(|page| {
                page.snapshot
                    .notices
                    .iter()
                    .map(|notice| (page.snapshot.status_page_id, notice.key.clone()))
            })
            .collect();
        state.queued.retain(|key| live.contains(key));
        let mut rows = Vec::new();
        for page in pages.values() {
            let page_id = page.snapshot.status_page_id;
            for notice in &page.snapshot.notices {
                if state.queued.contains(&(page_id, notice.key.clone())) {
                    continue;
                }
                rows.push(status_page_notices::ActiveModel {
                    status_page_id: Set(page_id),
                    notice_key: Set(notice.key.clone()),
                    subject: Set(notice.subject.clone()),
                    body_text: Set(notice.body_text.clone()),
                    body_html: Set(notice.body_html.clone()),
                    cursor_subscriber_id: Set(0),
                    sent_count: Set(0),
                    created_at: Set(now),
                    ..Default::default()
                });
            }
        }
        let count = rows.len();
        if count > 0 {
            status_page_notices::Entity::insert_many(rows)
                .on_conflict(
                    OnConflict::columns([
                        status_page_notices::Column::StatusPageId,
                        status_page_notices::Column::NoticeKey,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec_without_returning(self.db.as_ref())
                .await?;
            state.queued.extend(live);
        }
        Ok(count)
    }

    async fn send_confirmations(
        &self,
        pages: &HashMap<i32, LivePage>,
        can_send: &(dyn Fn(&LivePage) -> bool + Sync),
        now: UtcDateTime,
        budget: &mut usize,
        report: &mut TickReport,
    ) -> Result<(), SubscriptionError> {
        let sendable: Vec<i32> = pages
            .iter()
            .filter(|(_, page)| can_send(page))
            .map(|(id, _)| *id)
            .collect();
        if sendable.is_empty() || *budget == 0 {
            return Ok(());
        }
        let rows = status_page_subscribers::Entity::find()
            .filter(
                status_page_subscribers::Column::State.eq(status_page_subscribers::STATE_PENDING),
            )
            .filter(status_page_subscribers::Column::ConfirmationSentAt.is_null())
            .filter(status_page_subscribers::Column::StatusPageId.is_in(sendable))
            .order_by_asc(status_page_subscribers::Column::Id)
            .limit(CONFIRMATIONS_PER_TICK.min(*budget as u64))
            .all(self.db.as_ref())
            .await?;
        for row in rows {
            let Some(page) = pages.get(&row.status_page_id) else {
                continue;
            };
            let (Some(sender), Some(links)) = (&page.sender, &page.links) else {
                continue;
            };
            let token = random_hex()?;
            // Claim the row: only one send per request, even if two ticks
            // overlapped.
            let claimed = status_page_subscribers::Entity::update_many()
                .col_expr(
                    status_page_subscribers::Column::ConfirmTokenHash,
                    Expr::value(token_hash(&token)),
                )
                .col_expr(
                    status_page_subscribers::Column::ConfirmExpiresAt,
                    Expr::value(now + chrono::Duration::hours(CONFIRMATION_VALID_HOURS)),
                )
                .col_expr(
                    status_page_subscribers::Column::ConfirmationSentAt,
                    Expr::value(now),
                )
                .filter(status_page_subscribers::Column::Id.eq(row.id))
                .filter(status_page_subscribers::Column::ConfirmationSentAt.is_null())
                .exec(self.db.as_ref())
                .await?;
            if claimed.rows_affected != 1 {
                continue;
            }
            let (subject, text, html) = confirmation_message(
                &page.snapshot.title,
                &links.confirm(&token),
                CONFIRMATION_VALID_HOURS,
            );
            *budget = budget.saturating_sub(1);
            match self
                .mailer
                .send(OutgoingMail {
                    subscriber_id: row.id,
                    from: sender.clone(),
                    from_name: page.snapshot.title.clone(),
                    to: row.email,
                    subject,
                    text,
                    html,
                })
                .await
            {
                Ok(()) => report.confirmations_sent += 1,
                Err(error) => {
                    // The visitor can ask again after the resend delay.
                    report.failures += 1;
                    warn!(
                        status_page_id = row.status_page_id,
                        subscriber_id = row.id,
                        error = %error,
                        "status page confirmation mail could not be sent"
                    );
                }
            }
            if *budget == 0 {
                break;
            }
        }
        Ok(())
    }

    async fn deliver_notices(
        &self,
        state: &mut DispatcherState,
        pages: &HashMap<i32, LivePage>,
        can_send: &(dyn Fn(&LivePage) -> bool + Sync),
        now: UtcDateTime,
        budget: &mut usize,
        report: &mut TickReport,
    ) -> Result<(), SubscriptionError> {
        let open = status_page_notices::Entity::find()
            .filter(status_page_notices::Column::CompletedAt.is_null())
            .order_by_asc(status_page_notices::Column::Id)
            .limit(NOTICES_PER_TICK)
            .all(self.db.as_ref())
            .await?;
        for notice in open {
            let page = pages.get(&notice.status_page_id);
            let still_public = page.is_some_and(|page| {
                page.snapshot
                    .notices
                    .iter()
                    .any(|live| live.key == notice.notice_key)
            });
            if !still_public {
                self.complete_notice(notice.id, now).await?;
                report.notices_cancelled += 1;
                info!(
                    status_page_id = notice.status_page_id,
                    notice_id = notice.id,
                    sent = notice.sent_count,
                    "status page notice cancelled: the update is no longer on the public page"
                );
                continue;
            }
            let Some(page) = page.filter(|page| can_send(page)) else {
                continue;
            };
            let (Some(sender), Some(links)) = (&page.sender, &page.links) else {
                continue;
            };
            if *budget == 0 {
                break;
            }
            let limit = RECIPIENT_BATCH.min(*budget as u64);
            let recipients = status_page_subscribers::Entity::find()
                .filter(status_page_subscribers::Column::StatusPageId.eq(notice.status_page_id))
                .filter(
                    status_page_subscribers::Column::State
                        .eq(status_page_subscribers::STATE_CONFIRMED),
                )
                .filter(status_page_subscribers::Column::Id.gt(notice.cursor_subscriber_id))
                .filter(status_page_subscribers::Column::ConfirmedAt.lte(notice.created_at))
                .order_by_asc(status_page_subscribers::Column::Id)
                .limit(limit)
                .all(self.db.as_ref())
                .await?;
            let fetched = recipients.len() as u64;
            let mut stalled = false;
            for recipient in recipients {
                let unsubscribe = links.unsubscribe(&unsubscribe_token(
                    &self.encryption,
                    &recipient.unsubscribe_nonce,
                ));
                let (text, html) = with_footer(
                    &notice.body_text,
                    &notice.body_html,
                    &page.snapshot.title,
                    &links.page(),
                    &unsubscribe,
                );
                *budget = budget.saturating_sub(1);
                let sent = self
                    .mailer
                    .send(OutgoingMail {
                        subscriber_id: recipient.id,
                        from: sender.clone(),
                        from_name: page.snapshot.title.clone(),
                        to: recipient.email.clone(),
                        subject: notice.subject.clone(),
                        text,
                        html,
                    })
                    .await;
                let key = (notice.id, recipient.id);
                let advance = match sent {
                    Ok(()) => {
                        state.failures.remove(&key);
                        report.notices_sent += 1;
                        true
                    }
                    Err(error) => {
                        report.failures += 1;
                        let attempts = state.failures.entry(key).or_insert(0);
                        *attempts += 1;
                        let give_up = *attempts >= MAX_ATTEMPTS_PER_RECIPIENT;
                        warn!(
                            status_page_id = notice.status_page_id,
                            notice_id = notice.id,
                            subscriber_id = recipient.id,
                            attempt = *attempts,
                            skipped = give_up,
                            error = %error,
                            "status page notice could not be sent to a subscriber"
                        );
                        if give_up {
                            state.failures.remove(&key);
                        }
                        give_up
                    }
                };
                if !advance {
                    // Retry this recipient next tick, before anyone after it.
                    stalled = true;
                    break;
                }
                status_page_notices::Entity::update_many()
                    .col_expr(
                        status_page_notices::Column::CursorSubscriberId,
                        Expr::value(recipient.id),
                    )
                    .col_expr(
                        status_page_notices::Column::SentCount,
                        Expr::col(status_page_notices::Column::SentCount).add(1),
                    )
                    .filter(status_page_notices::Column::Id.eq(notice.id))
                    .filter(status_page_notices::Column::CursorSubscriberId.lt(recipient.id))
                    .exec(self.db.as_ref())
                    .await?;
                if *budget == 0 {
                    stalled = true;
                    break;
                }
            }
            if !stalled && fetched < limit {
                self.complete_notice(notice.id, now).await?;
                report.notices_completed += 1;
                debug!(
                    status_page_id = notice.status_page_id,
                    notice_id = notice.id,
                    "status page notice delivered to every subscriber"
                );
            }
        }
        Ok(())
    }

    async fn complete_notice(
        &self,
        notice_id: i32,
        now: UtcDateTime,
    ) -> Result<(), SubscriptionError> {
        status_page_notices::Entity::update_many()
            .col_expr(status_page_notices::Column::CompletedAt, Expr::value(now))
            .filter(status_page_notices::Column::Id.eq(notice_id))
            .filter(status_page_notices::Column::CompletedAt.is_null())
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Delete unconfirmed requests past the confirmation window (their
    /// addresses are not kept) and completed notices past retention.
    async fn prune(&self, now: UtcDateTime) -> Result<(), SubscriptionError> {
        let expired = status_page_subscribers::Entity::delete_many()
            .filter(
                status_page_subscribers::Column::State.eq(status_page_subscribers::STATE_PENDING),
            )
            .filter(
                status_page_subscribers::Column::RequestedAt
                    .lt(now - chrono::Duration::hours(CONFIRMATION_VALID_HOURS)),
            )
            .exec(self.db.as_ref())
            .await?;
        let old = status_page_notices::Entity::delete_many()
            .filter(
                status_page_notices::Column::CompletedAt
                    .lt(now - chrono::Duration::days(NOTICE_RETENTION_DAYS)),
            )
            .exec(self.db.as_ref())
            .await?;
        if expired.rows_affected > 0 || old.rows_affected > 0 {
            info!(
                unconfirmed_deleted = expired.rows_affected,
                notices_deleted = old.rows_affected,
                "pruned status page subscriptions"
            );
        }
        Ok(())
    }
}
