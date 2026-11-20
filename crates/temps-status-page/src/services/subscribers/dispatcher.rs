// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background delivery of subscriber mail (ADR 108 D2).
//!
//! Every [`TICK`] the dispatcher:
//!
//! 1. queues the notices of every served snapshot with subscriptions on into
//!    `status_page_notices` in publication order (`ON CONFLICT DO NOTHING`,
//!    so a restart never queues one twice; an in-memory set skips keys
//!    already queued);
//! 2. cancels open notices that are no longer on a public page: the update
//!    was made private or deleted, the page unpublished or its subscriptions
//!    switched off. This runs even when no page is live, so nothing waits
//!    forever;
//! 3. sends pending confirmation mails, minting each token at send time, and
//!    retries a failed one a few times;
//! 4. delivers open notices of the pages that can send to confirmed
//!    subscribers in id order from each notice's cursor;
//! 5. now and then deletes expired unconfirmed requests and old notices.
//!
//! Bounds: at most [`SENDS_PER_TICK`] mails per tick in total (about 36,000
//! an hour), [`NOTICES_PER_TICK`] notices and [`CONFIRMATIONS_PER_TICK`]
//! confirmations looked at per tick. Queries are per tick, not per page,
//! apart from one recipient batch per open notice and one claim per mail.
//!
//! Delivery is at most once per recipient and notice, across every console
//! sharing the database: before the provider is called, the recipient is
//! claimed by moving the notice's cursor past it in one conditional update,
//! which also checks the recipient is still confirmed. A second console that
//! read the same cursor loses the claim and sends nothing. A send that fails
//! hands its claim back, so the recipient is retried; a crash between the
//! claim and the send loses at most that one mail. Right before each send the
//! page is checked again as it is served now, so mail never outruns the page.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, Statement,
};
use temps_core::UtcDateTime;
use temps_entities::{status_page_notices, status_page_subscribers, status_pages};
use tracing::{debug, info, warn};

use super::error::SubscriptionError;
use super::mailer::{OutgoingMail, Sent};
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
/// Most open notices of sendable pages looked at per tick.
const NOTICES_PER_TICK: u64 = 20;
/// Most open notices checked per tick for being withdrawn. Open notices only
/// exist for updates of the last [`super::notices::NOTICE_WINDOW`], so this
/// is far above what is ever open.
const WITHDRAWN_CHECKS_PER_TICK: u64 = 1_000;
/// Most pending confirmations looked at per tick.
const CONFIRMATIONS_PER_TICK: u64 = 50;
/// Completed notices are kept this long, for the record, then deleted.
const NOTICE_RETENTION_DAYS: i64 = 30;
/// Prune every this many ticks (about ten minutes).
const PRUNE_EVERY_TICKS: u64 = 30;
/// A recipient whose mail failed this many times in a row is skipped, so one
/// bad address cannot hold a notice back from everyone after it. A
/// confirmation that failed this often waits for the visitor to ask again.
const MAX_ATTEMPTS_PER_RECIPIENT: u8 = 3;

/// A page that is served with subscriptions on, and what mail needs from it.
struct LivePage {
    snapshot: Arc<PublishedSnapshot>,
    sender: Option<String>,
    /// The sender was chosen by a project-scoped credential: its domain must
    /// still be granted to every project of the page.
    sender_scoped: bool,
    links: Option<PageLinks>,
}

pub struct DispatcherState {
    /// `(page id, notice key)` already queued; trimmed to the live notices
    /// every tick, so it never outgrows them.
    queued: HashSet<(i32, String)>,
    /// `(notice id, subscriber id)` -> consecutive failed sends.
    failures: HashMap<(i32, i32), u8>,
    /// Subscriber id -> consecutive failed confirmation sends.
    confirmation_failures: HashMap<i32, u8>,
    ticks: u64,
    notices_per_tick: u64,
}

impl Default for DispatcherState {
    fn default() -> Self {
        Self {
            queued: HashSet::new(),
            failures: HashMap::new(),
            confirmation_failures: HashMap::new(),
            ticks: 0,
            notices_per_tick: NOTICES_PER_TICK,
        }
    }
}

impl DispatcherState {
    /// A state that looks at fewer notices per tick, for tests.
    #[cfg(test)]
    pub(crate) fn with_notices_per_tick(notices_per_tick: u64) -> Self {
        Self {
            notices_per_tick,
            ..Self::default()
        }
    }
}

/// What one tick did, for logs and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub queued: usize,
    pub confirmations_sent: usize,
    pub notices_sent: usize,
    pub notices_completed: usize,
    pub notices_cancelled: usize,
    /// Mails not sent because the address is on the suppression list.
    pub suppressed: usize,
    pub failures: usize,
}

/// The outcome of claiming one recipient of a notice.
enum Claim {
    /// This dispatcher moved the cursor past the recipient: send.
    Claimed,
    /// The recipient was deleted or unsubscribed meanwhile: skip.
    Gone,
    /// Another dispatcher moved the cursor, or the notice was completed:
    /// stop working on this notice this tick.
    Lost,
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
        let senders: Vec<(i32, Option<String>, bool)> = status_pages::Entity::find()
            .select_only()
            .column(status_pages::Column::Id)
            .column(status_pages::Column::SubscriptionSender)
            .column(status_pages::Column::SubscriptionSenderScoped)
            .filter(status_pages::Column::SubscriptionsEnabled.eq(true))
            .filter(status_pages::Column::Id.is_in(snapshots.keys().copied()))
            .into_tuple()
            .all(self.db.as_ref())
            .await?;
        let base = self.public_base().await?;
        let mut snapshots = snapshots;
        Ok(senders
            .into_iter()
            .filter_map(|(id, sender, sender_scoped)| {
                let snapshot = snapshots.remove(&id)?;
                let links = PageLinks::for_snapshot(&snapshot, base.as_deref());
                Some((
                    id,
                    LivePage {
                        snapshot,
                        sender,
                        sender_scoped,
                        links,
                    },
                ))
            })
            .collect())
    }

    /// The pages whose mail can go out now: links can be built, the sender
    /// is ready, and a sender chosen by a project-scoped credential is still
    /// granted to the page's projects.
    async fn sendable_pages(
        &self,
        pages: &HashMap<i32, LivePage>,
    ) -> Result<HashSet<i32>, SubscriptionError> {
        // Readiness per sender, asked once per tick.
        let mut ready: HashMap<String, bool> = HashMap::new();
        let mut sendable = HashSet::new();
        for (id, page) in pages {
            let Some(sender) = &page.sender else {
                continue;
            };
            if page.links.is_none() {
                continue;
            }
            if !ready.contains_key(sender) {
                let readiness = self.mailer.readiness(Some(sender)).await?;
                if let Some(problem) = &readiness.problem {
                    warn!(
                        status_page_id = *id,
                        reason = %problem.reason,
                        "status page subscriber mail is waiting: the sender cannot send"
                    );
                }
                ready.insert(sender.clone(), readiness.problem.is_none());
            }
            if !ready.get(sender).copied().unwrap_or(false) {
                continue;
            }
            if page.sender_scoped {
                let mut projects: Vec<i32> = page.snapshot.project_ids.iter().copied().collect();
                projects.sort_unstable();
                if !self.mailer.sender_granted(sender, &projects).await? {
                    warn!(
                        status_page_id = *id,
                        "status page subscriber mail is waiting: the sender's domain is no longer authorized for every project of the page"
                    );
                    continue;
                }
            }
            sendable.insert(*id);
        }
        Ok(sendable)
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
        report.notices_cancelled = self.cancel_withdrawn(&pages, now).await?;
        if pages.is_empty() {
            return Ok(report);
        }
        let sendable = self.sendable_pages(&pages).await?;

        let mut budget = SENDS_PER_TICK;
        self.send_confirmations(state, &pages, &sendable, now, &mut budget, &mut report)
            .await?;
        self.deliver_notices(state, &pages, &sendable, now, &mut budget, &mut report)
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
        let mut fresh = Vec::new();
        for page in pages.values() {
            let page_id = page.snapshot.status_page_id;
            for notice in &page.snapshot.notices {
                if !state.queued.contains(&(page_id, notice.key.clone())) {
                    fresh.push((page_id, notice));
                }
            }
        }
        // Ids are delivery order: queue in publication order, so a burst of
        // updates reaches subscribers as the incident unfolded.
        fresh.sort_by(|(page_a, a), (page_b, b)| {
            a.published_at
                .cmp(&b.published_at)
                .then_with(|| page_a.cmp(page_b))
                .then_with(|| a.key.cmp(&b.key))
        });
        let rows: Vec<status_page_notices::ActiveModel> = fresh
            .into_iter()
            .map(|(page_id, notice)| status_page_notices::ActiveModel {
                status_page_id: Set(page_id),
                notice_key: Set(notice.key.clone()),
                subject: Set(notice.subject.clone()),
                body_text: Set(notice.body_text.clone()),
                body_html: Set(notice.body_html.clone()),
                cursor_subscriber_id: Set(0),
                sent_count: Set(0),
                created_at: Set(now),
                ..Default::default()
            })
            .collect();
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

    /// Complete every open notice whose update is no longer on a public page.
    async fn cancel_withdrawn(
        &self,
        pages: &HashMap<i32, LivePage>,
        now: UtcDateTime,
    ) -> Result<usize, SubscriptionError> {
        // Pages that are not served with subscriptions on: all of theirs.
        let mut gone = status_page_notices::Entity::update_many()
            .col_expr(status_page_notices::Column::CompletedAt, Expr::value(now))
            .filter(status_page_notices::Column::CompletedAt.is_null());
        if !pages.is_empty() {
            gone = gone
                .filter(status_page_notices::Column::StatusPageId.is_not_in(pages.keys().copied()));
        }
        let mut cancelled = gone.exec(self.db.as_ref()).await?.rows_affected as usize;
        if !pages.is_empty() {
            // Live pages: the notices whose update left the page.
            let open: Vec<(i32, i32, String)> = status_page_notices::Entity::find()
                .select_only()
                .column(status_page_notices::Column::Id)
                .column(status_page_notices::Column::StatusPageId)
                .column(status_page_notices::Column::NoticeKey)
                .filter(status_page_notices::Column::CompletedAt.is_null())
                .order_by_asc(status_page_notices::Column::Id)
                .limit(WITHDRAWN_CHECKS_PER_TICK)
                .into_tuple()
                .all(self.db.as_ref())
                .await?;
            let withdrawn: Vec<i32> = open
                .into_iter()
                .filter(|(_, page_id, key)| {
                    !pages
                        .get(page_id)
                        .is_some_and(|page| page.snapshot.notices.iter().any(|n| n.key == *key))
                })
                .map(|(id, _, _)| id)
                .collect();
            if !withdrawn.is_empty() {
                cancelled += status_page_notices::Entity::update_many()
                    .col_expr(status_page_notices::Column::CompletedAt, Expr::value(now))
                    .filter(status_page_notices::Column::Id.is_in(withdrawn))
                    .filter(status_page_notices::Column::CompletedAt.is_null())
                    .exec(self.db.as_ref())
                    .await?
                    .rows_affected as usize;
            }
        }
        if cancelled > 0 {
            info!(
                cancelled,
                "status page notices cancelled: their updates are no longer on a public page"
            );
        }
        Ok(cancelled)
    }

    /// Whether `notice_key` is still announced by the page as it is served
    /// right now, not as it was when the tick began.
    fn still_announced(&self, page: &LivePage, notice_key: &str) -> bool {
        self.cache
            .get(&page.snapshot.slug)
            .filter(|current| current.status_page_id == page.snapshot.status_page_id)
            .is_some_and(|current| {
                current.accepts_subscriptions()
                    && current
                        .notices
                        .iter()
                        .any(|notice| notice.key == notice_key)
            })
    }

    async fn send_confirmations(
        &self,
        state: &mut DispatcherState,
        pages: &HashMap<i32, LivePage>,
        sendable: &HashSet<i32>,
        now: UtcDateTime,
        budget: &mut usize,
        report: &mut TickReport,
    ) -> Result<(), SubscriptionError> {
        if sendable.is_empty() || *budget == 0 {
            return Ok(());
        }
        let rows = status_page_subscribers::Entity::find()
            .filter(
                status_page_subscribers::Column::State.eq(status_page_subscribers::STATE_PENDING),
            )
            .filter(status_page_subscribers::Column::ConfirmationSentAt.is_null())
            .filter(status_page_subscribers::Column::StatusPageId.is_in(sendable.iter().copied()))
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
            let hash = token_hash(&token);
            // Claim the row: only one send per request, even if two ticks
            // overlapped.
            let claimed = status_page_subscribers::Entity::update_many()
                .col_expr(
                    status_page_subscribers::Column::ConfirmTokenHash,
                    Expr::value(hash.clone()),
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
                Ok(Sent::Delivered) => {
                    state.confirmation_failures.remove(&row.id);
                    report.confirmations_sent += 1;
                }
                Ok(Sent::Suppressed) => {
                    state.confirmation_failures.remove(&row.id);
                    report.suppressed += 1;
                }
                Err(error) => {
                    report.failures += 1;
                    let attempts = state.confirmation_failures.entry(row.id).or_insert(0);
                    *attempts += 1;
                    let give_up = *attempts >= MAX_ATTEMPTS_PER_RECIPIENT;
                    warn!(
                        status_page_id = row.status_page_id,
                        subscriber_id = row.id,
                        attempt = *attempts,
                        retried = !give_up,
                        error = %error,
                        "status page confirmation mail could not be sent"
                    );
                    if give_up {
                        // The visitor can ask again after the resend delay.
                        state.confirmation_failures.remove(&row.id);
                    } else {
                        // Hand the claim back so the next tick sends again;
                        // only this send's own claim, never a newer one.
                        status_page_subscribers::Entity::update_many()
                            .col_expr(
                                status_page_subscribers::Column::ConfirmationSentAt,
                                Expr::value(Option::<UtcDateTime>::None),
                            )
                            .col_expr(
                                status_page_subscribers::Column::ConfirmTokenHash,
                                Expr::value(Option::<String>::None),
                            )
                            .col_expr(
                                status_page_subscribers::Column::ConfirmExpiresAt,
                                Expr::value(Option::<UtcDateTime>::None),
                            )
                            .filter(status_page_subscribers::Column::Id.eq(row.id))
                            .filter(status_page_subscribers::Column::ConfirmTokenHash.eq(hash))
                            .exec(self.db.as_ref())
                            .await?;
                    }
                }
            }
            if *budget == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Move a notice's cursor from `from` to `recipient`, if it is still at
    /// `from`, and say whether the recipient is still confirmed.
    async fn claim_recipient(
        &self,
        notice_id: i32,
        from: i32,
        recipient: i32,
    ) -> Result<Claim, SubscriptionError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE status_page_notices SET cursor_subscriber_id = $3 \
                 WHERE id = $1 AND cursor_subscriber_id = $2 AND completed_at IS NULL \
                 RETURNING EXISTS (SELECT 1 FROM status_page_subscribers \
                     WHERE id = $3 AND state = 'confirmed') AS deliver",
                [notice_id.into(), from.into(), recipient.into()],
            ))
            .await?;
        Ok(match row {
            None => Claim::Lost,
            Some(row) if row.try_get::<bool>("", "deliver")? => Claim::Claimed,
            Some(_) => Claim::Gone,
        })
    }

    async fn deliver_notices(
        &self,
        state: &mut DispatcherState,
        pages: &HashMap<i32, LivePage>,
        sendable: &HashSet<i32>,
        now: UtcDateTime,
        budget: &mut usize,
        report: &mut TickReport,
    ) -> Result<(), SubscriptionError> {
        if sendable.is_empty() || *budget == 0 {
            return Ok(());
        }
        // Only notices whose page can send: a page whose sender is broken
        // never holds back the notices of the others.
        let open = status_page_notices::Entity::find()
            .filter(status_page_notices::Column::CompletedAt.is_null())
            .filter(status_page_notices::Column::StatusPageId.is_in(sendable.iter().copied()))
            .order_by_asc(status_page_notices::Column::Id)
            .limit(state.notices_per_tick)
            .all(self.db.as_ref())
            .await?;
        for notice in open {
            if *budget == 0 {
                break;
            }
            let Some(page) = pages.get(&notice.status_page_id) else {
                continue;
            };
            let (Some(sender), Some(links)) = (&page.sender, &page.links) else {
                continue;
            };
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
            let mut cursor = notice.cursor_subscriber_id;
            let mut stalled = false;
            for recipient in recipients {
                if !self.still_announced(page, &notice.notice_key) {
                    self.complete_notice(notice.id, now).await?;
                    report.notices_cancelled += 1;
                    info!(
                        status_page_id = notice.status_page_id,
                        notice_id = notice.id,
                        "status page notice cancelled while sending: the update is no longer on the public page"
                    );
                    stalled = true;
                    break;
                }
                match self
                    .claim_recipient(notice.id, cursor, recipient.id)
                    .await?
                {
                    Claim::Claimed => {}
                    Claim::Gone => {
                        cursor = recipient.id;
                        continue;
                    }
                    Claim::Lost => {
                        stalled = true;
                        break;
                    }
                }
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
                match sent {
                    Ok(Sent::Delivered) => {
                        state.failures.remove(&key);
                        report.notices_sent += 1;
                        status_page_notices::Entity::update_many()
                            .col_expr(
                                status_page_notices::Column::SentCount,
                                Expr::col(status_page_notices::Column::SentCount).add(1),
                            )
                            .filter(status_page_notices::Column::Id.eq(notice.id))
                            .exec(self.db.as_ref())
                            .await?;
                    }
                    Ok(Sent::Suppressed) => {
                        state.failures.remove(&key);
                        report.suppressed += 1;
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
                        } else {
                            // Hand the claim back and retry this recipient
                            // next tick, before anyone after it.
                            status_page_notices::Entity::update_many()
                                .col_expr(
                                    status_page_notices::Column::CursorSubscriberId,
                                    Expr::value(cursor),
                                )
                                .filter(status_page_notices::Column::Id.eq(notice.id))
                                .filter(
                                    status_page_notices::Column::CursorSubscriberId
                                        .eq(recipient.id),
                                )
                                .exec(self.db.as_ref())
                                .await?;
                            stalled = true;
                            break;
                        }
                    }
                }
                cursor = recipient.id;
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
