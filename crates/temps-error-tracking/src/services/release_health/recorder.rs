// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Session ingest for release health (ADR 111, D3).
//!
//! Sessions arrive once per app start and again when they end, so they are
//! on the ingest hot path. The envelope handler turns each session update
//! or aggregate into a [`SessionDelta`] and `try_send`s it into a bounded
//! queue; it never waits on the database. One background task sums the
//! deltas in memory by project, environment, release and the hour the
//! session started, and writes them every [`FLUSH_INTERVAL`] (or when
//! [`MAX_PENDING_KEYS`] keys are pending) as one multi-row upsert per
//! [`FLUSH_CHUNK`] keys. A full queue drops the delta and counts it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use relay_event_schema::protocol::{
    SessionAggregates, SessionAttributes, SessionErrored, SessionLike, SessionUpdate,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::hll::{hash_id, Hll};
use crate::services::group_workflow::next_release::MAX_RELEASE_CHARS;

/// Deltas the queue holds before new ones are dropped.
pub const QUEUE_CAPACITY: usize = 10_000;
/// How often pending counters are written.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
/// Pending keys that force an early flush, bounding the task's memory
/// (each key holds at most three 256-byte sketches).
pub const MAX_PENDING_KEYS: usize = 5_000;
/// Keys written by one statement.
pub const FLUSH_CHUNK: usize = 200;
/// Attempts to write a key before its counts are given up.
const MAX_FLUSH_ATTEMPTS: u8 = 3;
/// Longest SDK environment name kept (Sentry's own limit).
pub const MAX_ENVIRONMENT_CHARS: usize = 64;
/// Sessions that started longer ago than this are refused, as Relay does.
pub const MAX_SESSION_AGE: TimeDelta = TimeDelta::days(5);
/// Clock skew tolerated for a session that starts in the future.
pub const MAX_SESSION_SKEW: TimeDelta = TimeDelta::hours(1);

/// Where a session is counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub project_id: i32,
    /// The SDK's environment, empty when it sent none.
    pub environment: String,
    pub release: String,
    /// The hour the session started; all updates of a session land here.
    pub bucket_start: DateTime<Utc>,
}

/// What one session update or aggregate adds to its hour.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDelta {
    pub key: SessionKey,
    pub started: u32,
    pub crashed: u32,
    pub abnormal: u32,
    /// Errored sessions an aggregate reports as a count.
    pub errored_exact: u32,
    /// Hash of a session that reported an error, counted once per session.
    pub errored_session: Option<u64>,
    pub user: Option<u64>,
    pub crashed_user: Option<u64>,
}

/// Why a session was not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRejection {
    /// No release, or one Temps does not register (too long, a NUL).
    Release,
    /// An environment longer than [`MAX_ENVIRONMENT_CHARS`] or with a NUL.
    Environment,
    /// Started more than [`MAX_SESSION_AGE`] ago or too far in the future.
    Timestamp,
}

fn clean_release(attributes: &SessionAttributes) -> Result<String, SessionRejection> {
    let release = attributes.release.trim();
    if release.is_empty() || release.chars().count() > MAX_RELEASE_CHARS || release.contains('\0') {
        return Err(SessionRejection::Release);
    }
    Ok(release.to_owned())
}

fn clean_environment(attributes: &SessionAttributes) -> Result<String, SessionRejection> {
    let environment = attributes.environment.as_deref().unwrap_or("").trim();
    if environment.chars().count() > MAX_ENVIRONMENT_CHARS || environment.contains('\0') {
        return Err(SessionRejection::Environment);
    }
    Ok(environment.to_owned())
}

fn bucket(started: DateTime<Utc>, now: DateTime<Utc>) -> Result<DateTime<Utc>, SessionRejection> {
    if started < now - MAX_SESSION_AGE || started > now + MAX_SESSION_SKEW {
        return Err(SessionRejection::Timestamp);
    }
    started
        .duration_trunc(TimeDelta::hours(1))
        .map_err(|_| SessionRejection::Timestamp)
}

/// A distinct id Relay would count: the nil UUID some SDKs send for "no
/// user" is not a user.
fn user_id(distinct_id: Option<&String>) -> Option<&str> {
    let id = distinct_id?.trim();
    if id.is_empty() || uuid::Uuid::parse_str(id).is_ok_and(|u| u.is_nil()) {
        return None;
    }
    Some(id)
}

/// The delta of one session, following Relay's session metrics: `init`
/// starts a session, an update with errors or an error status marks it
/// errored (once per session id), crashed and abnormal are terminal
/// statuses counted as they arrive, and every update counts its user.
fn delta_of<S: SessionLike>(
    key: SessionKey,
    session: &S,
    errored_session: Option<u64>,
) -> SessionDelta {
    let user = user_id(session.distinct_id()).map(hash_id);
    let (errored_session, errored_exact) = match session.all_errors() {
        Some(SessionErrored::Individual(_)) => (errored_session, 0),
        Some(SessionErrored::Aggregated(count)) => (None, count),
        None => (None, 0),
    };
    let crashed = session.crashed_count();
    SessionDelta {
        key,
        started: session.total_count(),
        crashed,
        abnormal: session.abnormal_count(),
        errored_exact,
        errored_session,
        crashed_user: if crashed > 0 { user } else { None },
        user,
    }
}

/// The delta of a single session update.
pub fn delta_from_update(
    project_id: i32,
    update: &SessionUpdate,
    now: DateTime<Utc>,
) -> Result<SessionDelta, SessionRejection> {
    let key = SessionKey {
        project_id,
        release: clean_release(&update.attributes)?,
        environment: clean_environment(&update.attributes)?,
        bucket_start: bucket(update.started, now)?,
    };
    let session_hash = hash_id(&update.session_id.to_string());
    Ok(delta_of(key, update, Some(session_hash)))
}

/// The deltas of a sessions item (server-mode SDKs send counts per minute
/// instead of one update per session). A bad item rejects only itself.
pub fn deltas_from_aggregates(
    project_id: i32,
    aggregates: &SessionAggregates,
    now: DateTime<Utc>,
) -> Vec<Result<SessionDelta, SessionRejection>> {
    let release = clean_release(&aggregates.attributes);
    let environment = clean_environment(&aggregates.attributes);
    aggregates
        .aggregates
        .iter()
        .map(|item| {
            let key = SessionKey {
                project_id,
                release: release.clone()?,
                environment: environment.clone()?,
                bucket_start: bucket(item.started, now)?,
            };
            Ok(delta_of(key, item, None))
        })
        .collect()
}

/// Counters of the recorder, read by the status endpoint and tests.
#[derive(Debug, Default)]
pub struct RecorderStats {
    pub queued: AtomicU64,
    /// Deltas dropped because the queue was full.
    pub dropped_queue_full: AtomicU64,
    pub rejected: AtomicU64,
    pub rows_written: AtomicU64,
    /// Keys given up after [`MAX_FLUSH_ATTEMPTS`] failed writes, or because
    /// too many keys were pending to keep a failed one.
    pub lost_on_flush: AtomicU64,
}

/// What recording one envelope's sessions did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecordOutcome {
    pub queued: u64,
    pub dropped: u64,
    pub rejected: u64,
}

/// The request-path side: a bounded sender and counters. Cloning is cheap.
#[derive(Clone)]
pub struct SessionRecorder {
    tx: mpsc::Sender<SessionDelta>,
    stats: Arc<RecorderStats>,
}

impl SessionRecorder {
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<SessionDelta>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                stats: Arc::new(RecorderStats::default()),
            },
            rx,
        )
    }

    pub fn stats(&self) -> &RecorderStats {
        &self.stats
    }

    pub(crate) fn stats_arc(&self) -> Arc<RecorderStats> {
        self.stats.clone()
    }

    /// Queue deltas without waiting. A full queue (or a stopped flusher)
    /// drops the delta: release health degrades, ingest never blocks.
    pub fn record(
        &self,
        deltas: impl IntoIterator<Item = Result<SessionDelta, SessionRejection>>,
    ) -> RecordOutcome {
        let mut outcome = RecordOutcome::default();
        for delta in deltas {
            match delta {
                Ok(delta) => match self.tx.try_send(delta) {
                    Ok(()) => outcome.queued += 1,
                    Err(_) => outcome.dropped += 1,
                },
                Err(_) => outcome.rejected += 1,
            }
        }
        self.stats
            .queued
            .fetch_add(outcome.queued, Ordering::Relaxed);
        self.stats
            .dropped_queue_full
            .fetch_add(outcome.dropped, Ordering::Relaxed);
        self.stats
            .rejected
            .fetch_add(outcome.rejected, Ordering::Relaxed);
        if outcome.dropped > 0 {
            warn!(
                dropped = outcome.dropped,
                capacity = QUEUE_CAPACITY,
                "Release health queue full; session counts dropped"
            );
        }
        outcome
    }
}

/// The sums pending for one key.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PendingCounts {
    pub started: i64,
    pub crashed: i64,
    pub abnormal: i64,
    pub errored_exact: i64,
    pub errored_sessions: Option<Hll>,
    pub users: Option<Hll>,
    pub crashed_users: Option<Hll>,
    attempts: u8,
}

fn insert_into(sketch: &mut Option<Hll>, hash: Option<u64>) {
    if let Some(hash) = hash {
        sketch.get_or_insert_with(Hll::default).insert_hash(hash);
    }
}

fn merge_into(sketch: &mut Option<Hll>, other: Option<Hll>) {
    match (sketch.as_mut(), other) {
        (Some(mine), Some(theirs)) => mine.merge(&theirs),
        (None, other) => *sketch = other,
        (Some(_), None) => {}
    }
}

impl PendingCounts {
    pub fn add(&mut self, delta: &SessionDelta) {
        self.started += i64::from(delta.started);
        self.crashed += i64::from(delta.crashed);
        self.abnormal += i64::from(delta.abnormal);
        self.errored_exact += i64::from(delta.errored_exact);
        insert_into(&mut self.errored_sessions, delta.errored_session);
        insert_into(&mut self.users, delta.user);
        insert_into(&mut self.crashed_users, delta.crashed_user);
    }

    fn absorb(&mut self, other: PendingCounts) {
        self.started += other.started;
        self.crashed += other.crashed;
        self.abnormal += other.abnormal;
        self.errored_exact += other.errored_exact;
        merge_into(&mut self.errored_sessions, other.errored_sessions);
        merge_into(&mut self.users, other.users);
        merge_into(&mut self.crashed_users, other.crashed_users);
        self.attempts = self.attempts.max(other.attempts);
    }
}

/// Sums deltas in memory.
#[derive(Debug, Default)]
pub struct PendingBuckets {
    pending: HashMap<SessionKey, PendingCounts>,
}

impl PendingBuckets {
    pub fn add(&mut self, delta: SessionDelta) {
        self.pending
            .entry(delta.key.clone())
            .or_default()
            .add(&delta);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn get(&self, key: &SessionKey) -> Option<&PendingCounts> {
        self.pending.get(key)
    }

    fn take(&mut self) -> Vec<(SessionKey, PendingCounts)> {
        self.pending.drain().collect()
    }

    /// Put back a chunk that failed to write; returns the keys given up.
    fn restore(&mut self, failed: Vec<(SessionKey, PendingCounts)>) -> u64 {
        let mut lost = 0;
        for (key, mut counts) in failed {
            counts.attempts += 1;
            if counts.attempts >= MAX_FLUSH_ATTEMPTS
                || (self.pending.len() >= MAX_PENDING_KEYS && !self.pending.contains_key(&key))
            {
                lost += 1;
                continue;
            }
            match self.pending.get_mut(&key) {
                Some(existing) => existing.absorb(counts),
                None => {
                    self.pending.insert(key, counts);
                }
            }
        }
        lost
    }
}

const UPSERT_COLUMNS: usize = 11;

fn upsert_statement(rows: &[(SessionKey, PendingCounts)]) -> Statement {
    let mut values: Vec<Value> = Vec::with_capacity(rows.len() * UPSERT_COLUMNS);
    let mut tuples = Vec::with_capacity(rows.len());
    for (index, (key, counts)) in rows.iter().enumerate() {
        let base = index * UPSERT_COLUMNS;
        tuples.push(format!(
            "(${}::int, ${}::text, ${}::text, ${}::timestamptz, ${}::bigint, ${}::bigint, \
             ${}::bigint, ${}::bigint, ${}::bytea, ${}::bytea, ${}::bytea)",
            base + 1,
            base + 2,
            base + 3,
            base + 4,
            base + 5,
            base + 6,
            base + 7,
            base + 8,
            base + 9,
            base + 10,
            base + 11,
        ));
        let bytes = |sketch: &Option<Hll>| -> Value {
            sketch
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| s.as_bytes().to_vec())
                .into()
        };
        values.extend([
            key.project_id.into(),
            key.environment.clone().into(),
            key.release.clone().into(),
            key.bucket_start.into(),
            counts.started.into(),
            counts.crashed.into(),
            counts.abnormal.into(),
            counts.errored_exact.into(),
            bytes(&counts.errored_sessions),
            bytes(&counts.users),
            bytes(&counts.crashed_users),
        ]);
    }
    // The join drops the counts of a project deleted since they were
    // queued instead of failing the whole chunk on the foreign key.
    let sql = format!(
        "INSERT INTO release_health_hourly AS h (project_id, environment, release, \
             bucket_start, started, crashed, abnormal, errored_exact, errored_sessions, users, \
             crashed_users, updated_at) \
         SELECT v.project_id, v.environment, v.release, v.bucket_start, v.started, v.crashed, \
                v.abnormal, v.errored_exact, v.errored_sessions, v.users, v.crashed_users, NOW() \
         FROM (VALUES {}) AS v(project_id, environment, release, bucket_start, started, crashed, \
                abnormal, errored_exact, errored_sessions, users, crashed_users) \
         JOIN projects p ON p.id = v.project_id \
         ON CONFLICT (project_id, environment, release, bucket_start) DO UPDATE SET \
             started = h.started + EXCLUDED.started, \
             crashed = h.crashed + EXCLUDED.crashed, \
             abnormal = h.abnormal + EXCLUDED.abnormal, \
             errored_exact = h.errored_exact + EXCLUDED.errored_exact, \
             errored_sessions = temps_hll_merge(h.errored_sessions, EXCLUDED.errored_sessions), \
             users = temps_hll_merge(h.users, EXCLUDED.users), \
             crashed_users = temps_hll_merge(h.crashed_users, EXCLUDED.crashed_users), \
             updated_at = NOW()",
        tuples.join(", ")
    );
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

/// The background side: drains the queue and writes the sums.
pub struct SessionFlusher {
    db: Arc<DatabaseConnection>,
    rx: mpsc::Receiver<SessionDelta>,
    stats: Arc<RecorderStats>,
    pending: PendingBuckets,
}

impl SessionFlusher {
    pub fn new(
        db: Arc<DatabaseConnection>,
        rx: mpsc::Receiver<SessionDelta>,
        stats: Arc<RecorderStats>,
    ) -> Self {
        Self {
            db,
            rx,
            stats,
            pending: PendingBuckets::default(),
        }
    }

    /// Runs until every sender is gone, then writes what is pending.
    pub async fn run(mut self) {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                delta = self.rx.recv() => match delta {
                    Some(delta) => {
                        self.pending.add(delta);
                        if self.pending.len() >= MAX_PENDING_KEYS {
                            self.flush().await;
                        }
                    }
                    None => {
                        self.flush().await;
                        debug!("Release health flusher stopped");
                        return;
                    }
                },
                _ = tick.tick() => self.flush().await,
            }
        }
    }

    /// Write every pending key. A failed chunk is kept for the next flush,
    /// at most [`MAX_FLUSH_ATTEMPTS`] times.
    pub async fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let rows = self.pending.take();
        let mut failed = Vec::new();
        for chunk in rows.chunks(FLUSH_CHUNK) {
            match self.db.execute(upsert_statement(chunk)).await {
                Ok(result) => {
                    self.stats
                        .rows_written
                        .fetch_add(result.rows_affected(), Ordering::Relaxed);
                }
                Err(err) => {
                    error!(
                        keys = chunk.len(),
                        error = %err,
                        "Could not write release health counters; retrying on the next flush"
                    );
                    failed.extend(chunk.iter().cloned());
                }
            }
        }
        if !failed.is_empty() {
            let lost = self.pending.restore(failed);
            if lost > 0 {
                self.stats.lost_on_flush.fetch_add(lost, Ordering::Relaxed);
                error!(
                    keys = lost,
                    "Release health counters given up after repeated write failures"
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> &PendingBuckets {
        &self.pending
    }

    #[cfg(test)]
    pub(crate) fn add(&mut self, delta: SessionDelta) {
        self.pending.add(delta);
    }
}
