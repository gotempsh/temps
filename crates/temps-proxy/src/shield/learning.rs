// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fleet learning (ADR 066, D9 and I9): what one ingress saw counts on every
//! ingress.
//!
//! Every [`PROMOTE_INTERVAL`], while `shield.share_observations` is on, one
//! control plane (a transaction-scoped advisory lock picks it) reads the
//! last [`WINDOW`] of `shield_events`, from ClickHouse when it is configured
//! and from Postgres otherwise, and upserts `shield_observed`:
//!
//! | Evidence in the last hour | Promoted | For |
//! |---|---|---|
//! | 1 or more exploit hits from an address | address → `block` | 7 days |
//! | 3 or more failed or abandoned challenges from an address | address → `challenge` | 24 hours |
//! | One JA4 with exploit hits from 20 or more addresses, not a browser's | fingerprint → `challenge` | 24 hours |
//!
//! An exploit hit is any event with a signature (D8), probes for leaked
//! secrets included. A challenge is abandoned when no solution followed it
//! from the same address within [`ABANDONED_AFTER`]. Only the address or
//! fingerprint and a short reason are stored, never a path or a host.
//!
//! The rules are [`promotions`], a pure function of per-subject aggregates;
//! the SQL only counts. Every pass then runs the compiler, which builds a new
//! snapshot revision if the observed rows or the operator's rules changed,
//! so a scanner blocked on one ingress is blocked on all of them within
//! about a minute.
//!
//! Every [`PRUNE_INTERVAL`], under its own lock, expired observed rows,
//! Postgres events older than [`EVENT_RETENTION`], snapshot revisions past
//! the [`KEEP_SNAPSHOTS`] newest and expired `challenge_sessions` are
//! deleted.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_shield::{browsers, signatures, ListVerdict};
use tracing::{debug, info, warn};

use super::sources::{ShieldSources, SourceError, KEEP_SNAPSHOTS};

pub const PROMOTE_INTERVAL: Duration = Duration::from_secs(30);
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How far back the evidence for a promotion reaches.
pub const WINDOW: Duration = Duration::from_secs(60 * 60);
/// A challenge with no solution from the same address this long after it
/// was served was abandoned.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(5 * 60);
pub const EXPLOIT_BLOCK_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const CHALLENGE_FAILURES: u64 = 3;
pub const CHALLENGE_FOR: Duration = Duration::from_secs(24 * 60 * 60);
pub const FINGERPRINT_ADDRESSES: u64 = 20;
pub const FINGERPRINT_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// How long the Postgres `shield_events` table keeps a row. ClickHouse
/// keeps its own for 30 days by TTL.
pub const EVENT_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A promote pass, events query included, is abandoned after this, so the
/// lock and its connection are never held into the next pass.
const PASS_TIMEOUT: Duration = Duration::from_secs(25);
const PRUNE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Most addresses one pass promotes, the most exploit hits first.
const MAX_ADDRESSES: i64 = 10_000;
/// Most fingerprints one pass considers, the most addresses first.
const MAX_FINGERPRINTS: i64 = 100;
/// Most (fingerprint, address) pairs read for those fingerprints.
const MAX_SIGHTINGS: i64 = 50_000;
/// Most stored rows read to tell new evidence from what was counted.
const MAX_STORED: i64 = 200_000;
/// Timestamps of issued and solved challenges kept per address in
/// ClickHouse; far more than the threshold needs.
const MAX_CHALLENGES_PER_ADDRESS: u32 = 1_000;
const UPSERT_CHUNK: usize = 500;
const PRUNE_BATCH: i64 = 5_000;
/// At most this many batches per prune, a million rows; the rest wait an
/// hour rather than keep the job running.
const MAX_PRUNE_BATCHES: usize = 200;
const PROMOTE_LOCK: &str = "temps_shield_promote";
const PRUNE_LOCK: &str = "temps_shield_prune";
/// `shield_observed.subject` is a `VARCHAR(64)`.
const MAX_SUBJECT_CHARS: usize = 64;
const MAX_REASON_CHARS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    #[error("database: {0}")]
    Database(#[from] DbErr),
    #[error("ClickHouse shield_events {operation}: {reason}")]
    ClickHouse {
        operation: &'static str,
        reason: String,
    },
    #[error("could not read the platform Shield settings: {0}")]
    Settings(String),
    #[error("compile: {0}")]
    Compile(#[from] SourceError),
    #[error("Shield {job} took longer than {secs} s and was abandoned")]
    Timeout { job: &'static str, secs: u64 },
}

/// Where the events are: the same choice the request-log storage makes.
pub enum EventStore {
    Postgres,
    ClickHouse(Box<::clickhouse::Client>),
}

impl EventStore {
    /// ClickHouse when `TEMPS_CLICKHOUSE_*` is configured, else Postgres.
    pub fn from_config(
        config: &temps_config::ServerConfig,
        compression: temps_clickhouse::ClickHouseCompression,
    ) -> Self {
        if !config.is_clickhouse_enabled() {
            return Self::Postgres;
        }
        Self::ClickHouse(Box::new(temps_clickhouse::build_client(
            config.clickhouse_url.as_deref().unwrap_or_default(),
            config.clickhouse_database.as_deref().unwrap_or_default(),
            config.clickhouse_user.as_deref().unwrap_or_default(),
            config.clickhouse_password.as_deref().unwrap_or_default(),
            compression,
        )))
    }
}

/// What `shield_observed.kind` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObservedKind {
    Ip,
    Ja4,
}

impl ObservedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::Ja4 => "ja4",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "ip" => Some(Self::Ip),
            "ja4" => Some(Self::Ja4),
            _ => None,
        }
    }
}

/// What the window's events say about one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEvidence {
    pub ip: IpAddr,
    pub exploits: u64,
    /// The newest exploit signature, as events record it (`CVE-…` or the
    /// signature id).
    pub signature: Option<String>,
    pub failed_challenges: u64,
    pub abandoned_challenges: u64,
    /// Evidence events newer than the stored row's `last_seen`: what this
    /// pass adds to `hits`.
    pub new_hits: u64,
    /// Unix milliseconds of the oldest and newest evidence event.
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

/// One address seen with one fingerprint in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sighting {
    pub ja4: String,
    pub ip: IpAddr,
    pub exploits: u64,
    /// Exploit events newer than the fingerprint row's `last_seen`.
    pub new_exploits: u64,
    /// The address solved a challenge with this fingerprint.
    pub solved: bool,
    /// A User-Agent the address sent with this fingerprint in a request
    /// that was not an exploit.
    pub user_agent: Option<String>,
    pub signature: Option<String>,
    /// Unix milliseconds of its oldest and newest exploit event.
    pub first_seen_ms: Option<i64>,
    pub last_seen_ms: Option<i64>,
}

/// What the window's events say about one fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintEvidence {
    pub ja4: String,
    /// Addresses that sent an exploit with it.
    pub exploit_addresses: u64,
    /// Addresses with no exploit hit that sent it with a User-Agent that
    /// claims a browser.
    pub browser_addresses: u64,
    /// Addresses that solved a challenge with it: only a browser runs the
    /// challenge page.
    pub solvers: u64,
    pub signature: Option<String>,
    pub new_hits: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

/// Fold the sightings of each fingerprint into its evidence.
pub fn fingerprints(sightings: &[Sighting]) -> Vec<FingerprintEvidence> {
    let mut by_ja4: BTreeMap<&str, Vec<&Sighting>> = BTreeMap::new();
    for sighting in sightings {
        by_ja4.entry(&sighting.ja4).or_default().push(sighting);
    }
    by_ja4
        .into_iter()
        .map(|(ja4, sightings)| {
            let exploiting: Vec<&&Sighting> = sightings.iter().filter(|s| s.exploits > 0).collect();
            let exploit_ips: HashSet<IpAddr> = exploiting.iter().map(|s| s.ip).collect();
            let browser_ips: HashSet<IpAddr> = sightings
                .iter()
                .filter(|s| !exploit_ips.contains(&s.ip))
                .filter(|s| {
                    s.user_agent
                        .as_deref()
                        .is_some_and(|ua| browsers::claimed_browser(ua).is_some())
                })
                .map(|s| s.ip)
                .collect();
            let solver_ips: HashSet<IpAddr> = sightings
                .iter()
                .filter(|s| s.solved)
                .map(|s| s.ip)
                .collect();
            let newest = exploiting
                .iter()
                .filter(|s| s.signature.is_some())
                .max_by_key(|s| s.last_seen_ms);
            FingerprintEvidence {
                ja4: ja4.to_string(),
                exploit_addresses: exploit_ips.len() as u64,
                browser_addresses: browser_ips.len() as u64,
                solvers: solver_ips.len() as u64,
                signature: newest.and_then(|s| s.signature.clone()),
                new_hits: exploiting.iter().map(|s| s.new_exploits).sum(),
                first_seen_ms: exploiting
                    .iter()
                    .filter_map(|s| s.first_seen_ms)
                    .min()
                    .unwrap_or(0),
                last_seen_ms: exploiting
                    .iter()
                    .filter_map(|s| s.last_seen_ms)
                    .max()
                    .unwrap_or(0),
            }
        })
        .collect()
}

/// One row to upsert into `shield_observed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub kind: ObservedKind,
    /// The address, exactly (no network), or the fingerprint.
    pub subject: String,
    /// `Block` or `Challenge`.
    pub verdict: ListVerdict,
    pub reason: String,
    pub hits: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    /// The newest evidence plus the rule's duration, so a pass that finds
    /// nothing new leaves the row as it was.
    pub expires_at_ms: i64,
}

/// What D9 promotes from the window's evidence: one row per subject, the
/// strongest rule winning.
pub fn promotions(
    addresses: &[AddressEvidence],
    fingerprints: &[FingerprintEvidence],
) -> Vec<Promotion> {
    let mut promoted: BTreeMap<(ObservedKind, String), Promotion> = BTreeMap::new();
    let mut keep = |promotion: Promotion| {
        let key = (promotion.kind, promotion.subject.clone());
        match promoted.get_mut(&key) {
            // The same address written two ways by different ingresses.
            Some(existing) => {
                let hits = existing.hits + promotion.hits;
                if promotion.verdict > existing.verdict
                    || (promotion.verdict == existing.verdict
                        && promotion.expires_at_ms > existing.expires_at_ms)
                {
                    *existing = promotion;
                }
                existing.hits = hits;
            }
            None => {
                promoted.insert(key, promotion);
            }
        }
    };
    for evidence in addresses {
        let subject = evidence.ip.to_canonical().to_string();
        let promotion = |verdict, reason: String, ttl: Duration| Promotion {
            kind: ObservedKind::Ip,
            subject: subject.clone(),
            verdict,
            reason: bounded(&reason),
            hits: evidence.new_hits,
            first_seen_ms: evidence.first_seen_ms,
            last_seen_ms: evidence.last_seen_ms,
            expires_at_ms: expiry(evidence.last_seen_ms, ttl),
        };
        let challenges = evidence.failed_challenges + evidence.abandoned_challenges;
        if evidence.exploits > 0 {
            keep(promotion(
                ListVerdict::Block,
                signature_reason(evidence.signature.as_deref()),
                EXPLOIT_BLOCK_FOR,
            ));
        } else if challenges >= CHALLENGE_FAILURES {
            keep(promotion(
                ListVerdict::Challenge,
                format!("{challenges} failed challenges"),
                CHALLENGE_FOR,
            ));
        }
    }
    for evidence in fingerprints {
        let eligible = evidence.exploit_addresses >= FINGERPRINT_ADDRESSES
            && evidence.ja4.chars().count() <= MAX_SUBJECT_CHARS
            && !evidence.ja4.is_empty()
            && !is_browser(evidence);
        if !eligible {
            continue;
        }
        keep(Promotion {
            kind: ObservedKind::Ja4,
            subject: evidence.ja4.clone(),
            verdict: ListVerdict::Challenge,
            reason: bounded(&format!(
                "{} from {} addresses",
                signature_reason(evidence.signature.as_deref()),
                evidence.exploit_addresses
            )),
            hits: evidence.new_hits,
            first_seen_ms: evidence.first_seen_ms,
            last_seen_ms: evidence.last_seen_ms,
            expires_at_ms: expiry(evidence.last_seen_ms, FINGERPRINT_FOR),
        });
    }
    promoted.into_values().collect()
}

/// A fingerprint browsers use is never promoted, however many scanners
/// copy it. The crate's browser list is short, so this also refuses one
/// that any address without exploit hits sent with a User-Agent claiming
/// a browser, or that solved a challenge with, in the window.
fn is_browser(evidence: &FingerprintEvidence) -> bool {
    browsers::is_known_browser(&evidence.ja4)
        || evidence.browser_addresses > 0
        || evidence.solvers > 0
}

/// "exploit CVE-2021-41773", or "probe secrets-probe" for a signature that
/// only looks for something to exploit.
fn signature_reason(signature: Option<&str>) -> String {
    let Some(signature) = signature.filter(|s| !s.is_empty()) else {
        return "exploit".to_string();
    };
    let recon = signatures::SIGNATURES
        .iter()
        .find(|known| known.cve.unwrap_or(known.id) == signature)
        .is_some_and(|known| known.recon());
    if recon {
        format!("probe {signature}")
    } else {
        format!("exploit {signature}")
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_REASON_CHARS).collect()
}

fn expiry(last_seen_ms: i64, ttl: Duration) -> i64 {
    last_seen_ms.saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX))
}

/// A `shield_observed` row seen within the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub verdict: ListVerdict,
    pub last_seen_ms: i64,
    pub expires_at_ms: i64,
}

/// Whether upserting `promotion` would change the row: something new was
/// seen, or the verdict or expiry grows. In the steady state of a long
/// attack every pass finds the same evidence, and writes nothing.
pub fn needs_write(promotion: &Promotion, stored: Option<&Stored>) -> bool {
    match stored {
        None => true,
        Some(stored) => {
            promotion.hits > 0
                || promotion.verdict > stored.verdict
                || promotion.expires_at_ms > stored.expires_at_ms
        }
    }
}

/// What one promote pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Promoted {
    pub addresses: usize,
    pub fingerprints: usize,
    /// Rows inserted or updated.
    pub written: usize,
}

/// What one prune pass deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    pub observed: u64,
    pub events: u64,
    pub snapshots: u64,
    pub challenge_sessions: u64,
}

pub struct ShieldLearning {
    db: Arc<DatabaseConnection>,
    events: EventStore,
    sources: Arc<ShieldSources>,
    config: Option<Arc<temps_config::ConfigService>>,
}

impl ShieldLearning {
    pub fn new(
        db: Arc<DatabaseConnection>,
        events: EventStore,
        sources: Arc<ShieldSources>,
    ) -> Self {
        Self {
            db,
            events,
            sources,
            config: None,
        }
    }

    /// Read `shield.share_observations` from `config`; without it the
    /// setting's default (on) applies.
    pub fn with_config(mut self, config: Arc<temps_config::ConfigService>) -> Self {
        self.config = Some(config);
        self
    }

    /// Promote every [`PROMOTE_INTERVAL`] and prune every
    /// [`PRUNE_INTERVAL`] on the current runtime, both first a minute after
    /// start. Like the source jobs, the task lives as long as the runtime.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + Duration::from_secs(60);
            let mut promote = tokio::time::interval_at(start, PROMOTE_INTERVAL);
            promote.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut prune = tokio::time::interval_at(start, PRUNE_INTERVAL);
            prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut failures: u64 = 0;
            loop {
                tokio::select! {
                    _ = promote.tick() => {
                        match self.promote_and_compile().await {
                            Ok(()) => failures = 0,
                            Err(e) => {
                                failures += 1;
                                // First failure, then every ten minutes.
                                if failures == 1 || failures.is_multiple_of(20) {
                                    warn!(failed_passes = failures, error = %e, "Shield promotion failed; trying again");
                                }
                            }
                        }
                    }
                    _ = prune.tick() => {
                        match tokio::time::timeout(PRUNE_TIMEOUT, self.prune()).await {
                            Ok(Ok(Some(pruned))) => debug!(?pruned, "Shield tables pruned"),
                            Ok(Ok(None)) => {}
                            Ok(Err(e)) => warn!(error = %e, "Shield prune failed; trying again in an hour"),
                            Err(_) => warn!(secs = PRUNE_TIMEOUT.as_secs(), "Shield prune timed out; trying again in an hour"),
                        }
                    }
                }
            }
        })
    }

    /// One pass: promote if sharing is on, then compile, which picks up
    /// the operator's rule changes too.
    async fn promote_and_compile(&self) -> Result<(), LearningError> {
        if self.share_observations().await? {
            let promoted = tokio::time::timeout(PASS_TIMEOUT, self.promote())
                .await
                .map_err(|_| LearningError::Timeout {
                    job: "promotion",
                    secs: PASS_TIMEOUT.as_secs(),
                })??;
            if let Some(promoted) = promoted.filter(|p| p.written > 0) {
                info!(
                    addresses = promoted.addresses,
                    fingerprints = promoted.fingerprints,
                    written = promoted.written,
                    "Shield observations promoted"
                );
            }
        }
        if let Some(revision) = self.sources.compile().await? {
            info!(revision, "Shield snapshot compiled");
        }
        Ok(())
    }

    async fn share_observations(&self) -> Result<bool, LearningError> {
        match &self.config {
            Some(config) => Ok(config
                .get_settings()
                .await
                .map_err(|e| LearningError::Settings(e.to_string()))?
                .shield
                .share_observations),
            None => Ok(true),
        }
    }

    /// Read the window's evidence and upsert what it promotes. `None` when
    /// another control plane holds the lock.
    pub async fn promote(&self) -> Result<Option<Promoted>, LearningError> {
        let txn = self.db.begin().await?;
        if !try_lock(&txn, PROMOTE_LOCK).await? {
            return Ok(None);
        }
        let now_ms = Utc::now().timestamp_millis();
        let window = Window {
            start_ms: now_ms - millis(WINDOW),
            judged_before_ms: now_ms - millis(ABANDONED_AFTER),
        };
        let stored = stored_rows(&txn, window.start_ms).await?;
        let since = |kind| -> Vec<(String, i64)> {
            stored
                .iter()
                .filter(|((k, _), _)| *k == kind)
                .map(|((_, subject), row)| (subject.clone(), row.last_seen_ms))
                .collect()
        };
        let (addresses, sightings) = match &self.events {
            EventStore::Postgres => (
                postgres::addresses(&txn, &since(ObservedKind::Ip), window).await?,
                postgres::sightings(&txn, &since(ObservedKind::Ja4), window).await?,
            ),
            EventStore::ClickHouse(client) => (
                ch::addresses(client, &since(ObservedKind::Ip), window).await?,
                ch::sightings(client, &since(ObservedKind::Ja4), window).await?,
            ),
        };
        let fingerprints = fingerprints(&sightings);
        let promotions = promotions(&addresses, &fingerprints);
        let writes: Vec<&Promotion> = promotions
            .iter()
            .filter(|p| needs_write(p, stored.get(&(p.kind, p.subject.clone()))))
            .collect();
        for chunk in writes.chunks(UPSERT_CHUNK) {
            upsert(&txn, chunk).await?;
        }
        txn.commit().await?;
        Ok(Some(Promoted {
            addresses: promotions
                .iter()
                .filter(|p| p.kind == ObservedKind::Ip)
                .count(),
            fingerprints: promotions
                .iter()
                .filter(|p| p.kind == ObservedKind::Ja4)
                .count(),
            written: writes.len(),
        }))
    }

    /// Delete what is past its time. `None` when another control plane
    /// holds the lock.
    pub async fn prune(&self) -> Result<Option<Pruned>, LearningError> {
        let txn = self.db.begin().await?;
        if !try_lock(&txn, PRUNE_LOCK).await? {
            return Ok(None);
        }
        let mut pruned = Pruned {
            observed: txn
                .execute(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "DELETE FROM shield_observed WHERE expires_at <= now()",
                ))
                .await?
                .rows_affected(),
            // The compiler keeps the same number; this catches revisions
            // left by a compile that failed between its insert and delete.
            snapshots: txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM shield_snapshots WHERE revision NOT IN \
                     (SELECT revision FROM shield_snapshots ORDER BY revision DESC LIMIT $1)",
                    [KEEP_SNAPSHOTS.into()],
                ))
                .await?
                .rows_affected(),
            ..Pruned::default()
        };
        // Phase 1 retired the table; it stays until a migration drops it.
        let sessions = txn
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT to_regclass('challenge_sessions') IS NOT NULL AS present",
            ))
            .await?
            .map(|row| row.try_get::<bool>("", "present"))
            .transpose()?
            .unwrap_or(false);
        if sessions {
            pruned.challenge_sessions = txn
                .execute(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "DELETE FROM challenge_sessions WHERE expires_at <= now()",
                ))
                .await?
                .rows_affected();
        }
        txn.commit().await?;

        // Old events in batches, each its own short transaction under the
        // lock, so a large backlog never becomes one long delete.
        let retention_secs = EVENT_RETENTION.as_secs() as f64;
        for _ in 0..MAX_PRUNE_BATCHES {
            let txn = self.db.begin().await?;
            if !try_lock(&txn, PRUNE_LOCK).await? {
                break;
            }
            let deleted = txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM shield_events WHERE id IN (SELECT id FROM shield_events \
                     WHERE timestamp < now() - make_interval(secs => $1) LIMIT $2)",
                    [retention_secs.into(), PRUNE_BATCH.into()],
                ))
                .await?
                .rows_affected();
            txn.commit().await?;
            pruned.events += deleted;
            if deleted < PRUNE_BATCH as u64 {
                break;
            }
        }
        Ok(Some(pruned))
    }
}

/// The window a pass reads, in Unix milliseconds.
#[derive(Debug, Clone, Copy)]
struct Window {
    start_ms: i64,
    /// Challenges served after this are too recent to call abandoned.
    judged_before_ms: i64,
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

async fn try_lock(txn: &DatabaseTransaction, name: &str) -> Result<bool, DbErr> {
    Ok(txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_try_advisory_xact_lock(hashtext($1)) AS acquired",
            [name.into()],
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "acquired"))
        .transpose()?
        .unwrap_or(false))
}

/// Unexpired rows seen within the window: the ones whose `last_seen` tells
/// a new event from one already counted. Rows seen earlier than the window
/// predate every event in it.
async fn stored_rows(
    txn: &DatabaseTransaction,
    window_start_ms: i64,
) -> Result<HashMap<(ObservedKind, String), Stored>, DbErr> {
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT kind, subject, verdict, \
             floor(extract(epoch FROM last_seen) * 1000)::bigint AS last_seen_ms, \
             floor(extract(epoch FROM expires_at) * 1000)::bigint AS expires_at_ms \
             FROM shield_observed \
             WHERE expires_at > now() \
               AND last_seen >= to_timestamp(0) + $1 * interval '1 millisecond' \
             ORDER BY last_seen DESC LIMIT $2",
            [window_start_ms.into(), MAX_STORED.into()],
        ))
        .await?;
    let mut stored = HashMap::with_capacity(rows.len());
    for row in rows {
        let kind: String = row.try_get("", "kind")?;
        let verdict: String = row.try_get("", "verdict")?;
        let (Some(kind), Some(verdict)) =
            (ObservedKind::parse(&kind), ListVerdict::parse(&verdict))
        else {
            continue;
        };
        stored.insert(
            (kind, row.try_get::<String>("", "subject")?),
            Stored {
                verdict,
                last_seen_ms: row.try_get("", "last_seen_ms")?,
                expires_at_ms: row.try_get("", "expires_at_ms")?,
            },
        );
    }
    Ok(stored)
}

#[derive(Serialize)]
struct UpsertRow<'a> {
    kind: &'static str,
    subject: &'a str,
    verdict: &'static str,
    reason: &'a str,
    first_seen: i64,
    last_seen: i64,
    hits: i64,
    expires_at: i64,
}

/// Insert or extend the rows. An unexpired `block` is never turned into a
/// `challenge`, and keeps its reason; an expired row starts over.
async fn upsert(txn: &DatabaseTransaction, promotions: &[&Promotion]) -> Result<(), DbErr> {
    let rows: Vec<UpsertRow<'_>> = promotions
        .iter()
        .map(|p| UpsertRow {
            kind: p.kind.as_str(),
            subject: &p.subject,
            verdict: p.verdict.as_str(),
            reason: &p.reason,
            first_seen: p.first_seen_ms,
            last_seen: p.last_seen_ms,
            hits: i64::try_from(p.hits).unwrap_or(i64::MAX),
            expires_at: p.expires_at_ms,
        })
        .collect();
    let rows = serde_json::to_value(&rows).map_err(|e| DbErr::Custom(e.to_string()))?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO shield_observed AS o \
             (kind, subject, verdict, reason, first_seen, last_seen, hits, expires_at) \
         SELECT p.kind, p.subject, p.verdict, p.reason, \
             to_timestamp(0) + p.first_seen * interval '1 millisecond', \
             to_timestamp(0) + p.last_seen * interval '1 millisecond', \
             p.hits, \
             to_timestamp(0) + p.expires_at * interval '1 millisecond' \
         FROM jsonb_to_recordset($1::jsonb) AS p(kind text, subject text, verdict text, \
             reason text, first_seen bigint, last_seen bigint, hits bigint, expires_at bigint) \
         ON CONFLICT (kind, subject) DO UPDATE SET \
             verdict = CASE WHEN o.expires_at > now() AND o.verdict = 'block' \
                 THEN o.verdict ELSE EXCLUDED.verdict END, \
             reason = CASE WHEN o.expires_at > now() AND o.verdict = 'block' \
                 AND EXCLUDED.verdict <> 'block' THEN o.reason ELSE EXCLUDED.reason END, \
             first_seen = CASE WHEN o.expires_at > now() \
                 THEN LEAST(o.first_seen, EXCLUDED.first_seen) ELSE EXCLUDED.first_seen END, \
             last_seen = CASE WHEN o.expires_at > now() \
                 THEN GREATEST(o.last_seen, EXCLUDED.last_seen) ELSE EXCLUDED.last_seen END, \
             hits = CASE WHEN o.expires_at > now() \
                 THEN o.hits + EXCLUDED.hits ELSE EXCLUDED.hits END, \
             expires_at = CASE WHEN o.expires_at > now() \
                 THEN GREATEST(o.expires_at, EXCLUDED.expires_at) ELSE EXCLUDED.expires_at END",
        [rows.into()],
    ))
    .await?;
    Ok(())
}

/// An address as an ingress wrote it, in canonical form.
fn parse_ip(text: &str) -> Option<IpAddr> {
    text.trim()
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical())
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.is_empty())
}

/// The evidence queries without ClickHouse.
mod postgres {
    use super::*;

    #[derive(Serialize)]
    struct Since<'a> {
        subject: &'a str,
        at: i64,
    }

    fn since_json(since: &[(String, i64)]) -> Result<serde_json::Value, DbErr> {
        let rows: Vec<Since<'_>> = since
            .iter()
            .map(|(subject, at)| Since { subject, at: *at })
            .collect();
        serde_json::to_value(rows).map_err(|e| DbErr::Custom(e.to_string()))
    }

    pub(super) async fn addresses(
        txn: &DatabaseTransaction,
        since: &[(String, i64)],
        window: Window,
    ) -> Result<Vec<AddressEvidence>, DbErr> {
        let rows = txn
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH since AS ( \
                     SELECT subject, at FROM jsonb_to_recordset($1::jsonb) AS w(subject text, at bigint) \
                 ), events AS ( \
                     SELECT e.ip, e.signature, \
                         floor(extract(epoch FROM e.timestamp) * 1000)::bigint AS ms, \
                         (e.kind = 'verdict' AND COALESCE(e.signature, '') <> '') AS exploit, \
                         e.kind = 'challenge_failed' AS failed, \
                         (e.kind = 'challenge_issued' \
                             AND e.timestamp <= to_timestamp(0) + $3 * interval '1 millisecond' \
                             AND NOT EXISTS (SELECT 1 FROM shield_events s \
                                 WHERE s.ip = e.ip AND s.kind = 'challenge_solved' \
                                   AND s.timestamp >= e.timestamp \
                                   AND s.timestamp <= e.timestamp + interval '5 minutes')) AS abandoned, \
                         COALESCE(w.at, -1) AS since \
                     FROM shield_events e LEFT JOIN since w ON w.subject = e.ip \
                     WHERE e.timestamp > to_timestamp(0) + $2 * interval '1 millisecond' \
                       AND ((e.kind = 'verdict' AND COALESCE(e.signature, '') <> '') \
                            OR e.kind IN ('challenge_failed', 'challenge_issued')) \
                 ) \
                 SELECT ip, \
                     count(*) FILTER (WHERE exploit) AS exploits, \
                     (array_agg(signature ORDER BY ms DESC) FILTER (WHERE exploit))[1] AS signature, \
                     count(*) FILTER (WHERE failed) AS failed, \
                     count(*) FILTER (WHERE abandoned) AS abandoned, \
                     count(*) FILTER (WHERE (exploit OR failed OR abandoned) AND ms > since) AS new_hits, \
                     min(ms) FILTER (WHERE exploit OR failed OR abandoned) AS first_seen_ms, \
                     max(ms) FILTER (WHERE exploit OR failed OR abandoned) AS last_seen_ms \
                 FROM events GROUP BY ip \
                 HAVING count(*) FILTER (WHERE exploit) > 0 \
                     OR count(*) FILTER (WHERE failed OR abandoned) >= $4 \
                 ORDER BY count(*) FILTER (WHERE exploit) DESC, ip LIMIT $5",
                [
                    since_json(since)?.into(),
                    window.start_ms.into(),
                    window.judged_before_ms.into(),
                    (CHALLENGE_FAILURES as i64).into(),
                    MAX_ADDRESSES.into(),
                ],
            ))
            .await?;
        let mut evidence = Vec::with_capacity(rows.len());
        for row in rows {
            let ip: String = row.try_get("", "ip")?;
            let Some(ip) = parse_ip(&ip) else {
                continue;
            };
            let count = |column: &str| -> Result<u64, DbErr> {
                Ok(u64::try_from(row.try_get::<i64>("", column)?).unwrap_or(0))
            };
            evidence.push(AddressEvidence {
                ip,
                exploits: count("exploits")?,
                signature: non_empty(row.try_get("", "signature")?),
                failed_challenges: count("failed")?,
                abandoned_challenges: count("abandoned")?,
                new_hits: count("new_hits")?,
                first_seen_ms: row
                    .try_get::<Option<i64>>("", "first_seen_ms")?
                    .unwrap_or(0),
                last_seen_ms: row.try_get::<Option<i64>>("", "last_seen_ms")?.unwrap_or(0),
            });
        }
        Ok(evidence)
    }

    pub(super) async fn sightings(
        txn: &DatabaseTransaction,
        since: &[(String, i64)],
        window: Window,
    ) -> Result<Vec<Sighting>, DbErr> {
        let rows = txn
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH since AS ( \
                     SELECT subject, at FROM jsonb_to_recordset($1::jsonb) AS w(subject text, at bigint) \
                 ), events AS ( \
                     SELECT ja4, ip, kind, user_agent, signature, \
                         floor(extract(epoch FROM timestamp) * 1000)::bigint AS ms, \
                         (kind = 'verdict' AND COALESCE(signature, '') <> '') AS exploit \
                     FROM shield_events \
                     WHERE timestamp > to_timestamp(0) + $2 * interval '1 millisecond' \
                       AND COALESCE(ja4, '') <> '' \
                 ), candidates AS ( \
                     SELECT ja4 FROM events WHERE exploit GROUP BY ja4 \
                     HAVING count(DISTINCT ip) >= $3 \
                     ORDER BY count(DISTINCT ip) DESC, ja4 LIMIT $4 \
                 ) \
                 SELECT e.ja4, e.ip, \
                     count(*) FILTER (WHERE e.exploit) AS exploits, \
                     count(*) FILTER (WHERE e.exploit AND e.ms > COALESCE(w.at, -1)) AS new_exploits, \
                     bool_or(e.kind = 'challenge_solved') AS solved, \
                     (array_agg(e.user_agent) FILTER (WHERE NOT e.exploit))[1] AS user_agent, \
                     (array_agg(e.signature ORDER BY e.ms DESC) FILTER (WHERE e.exploit))[1] AS signature, \
                     min(e.ms) FILTER (WHERE e.exploit) AS first_seen_ms, \
                     max(e.ms) FILTER (WHERE e.exploit) AS last_seen_ms \
                 FROM events e JOIN candidates c ON c.ja4 = e.ja4 \
                 LEFT JOIN since w ON w.subject = e.ja4 \
                 GROUP BY e.ja4, e.ip ORDER BY e.ja4, e.ip LIMIT $5",
                [
                    since_json(since)?.into(),
                    window.start_ms.into(),
                    (FINGERPRINT_ADDRESSES as i64).into(),
                    MAX_FINGERPRINTS.into(),
                    MAX_SIGHTINGS.into(),
                ],
            ))
            .await?;
        let mut sightings = Vec::with_capacity(rows.len());
        for row in rows {
            let ip: String = row.try_get("", "ip")?;
            let Some(ip) = parse_ip(&ip) else {
                continue;
            };
            let count = |column: &str| -> Result<u64, DbErr> {
                Ok(u64::try_from(row.try_get::<i64>("", column)?).unwrap_or(0))
            };
            sightings.push(Sighting {
                ja4: row.try_get("", "ja4")?,
                ip,
                exploits: count("exploits")?,
                new_exploits: count("new_exploits")?,
                solved: row.try_get::<Option<bool>>("", "solved")?.unwrap_or(false),
                user_agent: non_empty(row.try_get("", "user_agent")?),
                signature: non_empty(row.try_get("", "signature")?),
                first_seen_ms: row.try_get("", "first_seen_ms")?,
                last_seen_ms: row.try_get("", "last_seen_ms")?,
            });
        }
        Ok(sightings)
    }
}

/// The evidence queries against ClickHouse's `shield_events`, where `ip`
/// is an `IPv6` and IPv4 is stored mapped (`::ffff:a.b.c.d`).
mod ch {
    use super::*;

    fn error(operation: &'static str) -> impl Fn(::clickhouse::error::Error) -> LearningError {
        move |e| LearningError::ClickHouse {
            operation,
            reason: e.to_string(),
        }
    }

    /// The stored subjects and their `last_seen`, as two arrays the query
    /// looks up by index. A first, empty entry keeps either array from
    /// being empty, whose type ClickHouse cannot infer; an address on no
    /// row finds index 0, which reads as 0.
    fn since_arrays(since: &[(String, i64)], as_ip: bool) -> (Vec<String>, Vec<i64>) {
        let mut subjects = vec![String::new()];
        let mut at = vec![0];
        for (subject, last_seen) in since {
            let subject = match (as_ip, subject.parse::<IpAddr>()) {
                (true, Ok(IpAddr::V4(v4))) => v4.to_ipv6_mapped().to_string(),
                (true, Ok(IpAddr::V6(v6))) => v6.to_string(),
                (true, Err(_)) => continue,
                (false, _) => subject.clone(),
            };
            subjects.push(subject);
            at.push(*last_seen);
        }
        (subjects, at)
    }

    #[derive(::clickhouse::Row, Deserialize)]
    struct AddressRow {
        address: String,
        exploits: u64,
        last_signature: String,
        failed: u64,
        abandoned: u64,
        new_hits: u64,
        first_seen_ms: i64,
        last_seen_ms: i64,
    }

    pub(super) async fn addresses(
        client: &::clickhouse::Client,
        since: &[(String, i64)],
        window: Window,
    ) -> Result<Vec<AddressEvidence>, LearningError> {
        let (subjects, at) = since_arrays(since, true);
        let exploit = "(kind = 'verdict' AND signature != '')";
        let direct = format!("(kind = 'challenge_failed' OR {exploit})");
        let sql = format!(
            "WITH ? AS since_subjects, ? AS since_ms, ? AS judged_before \
             SELECT address, exploits, last_signature, failed, abandoned, new_hits, \
                 first_seen_ms, last_seen_ms \
             FROM ( \
             SELECT address, exploits, last_signature, failed, \
                 arrayFilter(t -> t <= judged_before \
                     AND NOT arrayExists(s -> s >= t AND s <= t + {abandon_ms}, solved), issued) \
                     AS abandoned_at, \
                 toUInt64(length(abandoned_at)) AS abandoned, \
                 toUInt64(new_direct + arrayCount(t -> t > since, abandoned_at)) AS new_hits, \
                 toInt64(arrayMin(arrayConcat(direct_first, abandoned_at))) AS first_seen_ms, \
                 toInt64(arrayMax(arrayConcat(direct_last, abandoned_at))) AS last_seen_ms \
             FROM ( \
                 SELECT IPv6NumToString(ip) AS address, \
                     since_ms[indexOf(since_subjects, IPv6NumToString(ip))] AS since, \
                     countIf({exploit}) AS exploits, \
                     toString(argMaxIf(signature, timestamp, {exploit})) AS last_signature, \
                     countIf(kind = 'challenge_failed') AS failed, \
                     countIf({direct} AND toUnixTimestamp64Milli(timestamp) > since) AS new_direct, \
                     groupArrayIf({max_challenges})(toUnixTimestamp64Milli(timestamp), \
                         kind = 'challenge_issued') AS issued, \
                     groupArrayIf({max_challenges})(toUnixTimestamp64Milli(timestamp), \
                         kind = 'challenge_solved') AS solved, \
                     if(countIf({direct}) > 0, \
                         [minIf(toUnixTimestamp64Milli(timestamp), {direct})], []) AS direct_first, \
                     if(countIf({direct}) > 0, \
                         [maxIf(toUnixTimestamp64Milli(timestamp), {direct})], []) AS direct_last \
                 FROM shield_events \
                 WHERE timestamp > fromUnixTimestamp64Milli(?) \
                   AND (kind IN ('challenge_issued', 'challenge_solved', 'challenge_failed') \
                        OR {exploit}) \
                 GROUP BY ip \
             ) \
             ) \
             WHERE exploits > 0 OR failed + abandoned >= ? \
             ORDER BY exploits DESC, address \
             LIMIT ?",
            abandon_ms = millis(ABANDONED_AFTER),
            max_challenges = MAX_CHALLENGES_PER_ADDRESS,
        );
        let rows = client
            .query(&sql)
            .bind(subjects)
            .bind(at)
            .bind(window.judged_before_ms)
            .bind(window.start_ms)
            .bind(CHALLENGE_FAILURES)
            .bind(MAX_ADDRESSES)
            .fetch_all::<AddressRow>()
            .await
            .map_err(error("address evidence"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(AddressEvidence {
                    ip: parse_ip(&row.address)?,
                    exploits: row.exploits,
                    signature: non_empty(Some(row.last_signature)),
                    failed_challenges: row.failed,
                    abandoned_challenges: row.abandoned,
                    new_hits: row.new_hits,
                    first_seen_ms: row.first_seen_ms,
                    last_seen_ms: row.last_seen_ms,
                })
            })
            .collect())
    }

    #[derive(::clickhouse::Row, Deserialize)]
    struct SightingRow {
        fingerprint: String,
        address: String,
        exploits: u64,
        new_exploits: u64,
        solved: u8,
        sample_agent: String,
        last_signature: String,
        first_seen_ms: i64,
        last_seen_ms: i64,
    }

    pub(super) async fn sightings(
        client: &::clickhouse::Client,
        since: &[(String, i64)],
        window: Window,
    ) -> Result<Vec<Sighting>, LearningError> {
        let (subjects, at) = since_arrays(since, false);
        let exploit = "(kind = 'verdict' AND signature != '')";
        let sql = format!(
            "WITH ? AS since_subjects, ? AS since_ms \
             SELECT toString(ja4) AS fingerprint, IPv6NumToString(ip) AS address, \
                 countIf({exploit}) AS exploits, \
                 countIf({exploit} AND toUnixTimestamp64Milli(timestamp) > \
                     since_ms[indexOf(since_subjects, toString(ja4))]) AS new_exploits, \
                 toUInt8(countIf(kind = 'challenge_solved') > 0) AS solved, \
                 toString(anyIf(user_agent, NOT {exploit})) AS sample_agent, \
                 toString(argMaxIf(signature, timestamp, {exploit})) AS last_signature, \
                 toInt64(minIf(toUnixTimestamp64Milli(timestamp), {exploit})) AS first_seen_ms, \
                 toInt64(maxIf(toUnixTimestamp64Milli(timestamp), {exploit})) AS last_seen_ms \
             FROM shield_events \
             WHERE timestamp > fromUnixTimestamp64Milli(?) \
               AND ja4 IN ( \
                   SELECT ja4 FROM shield_events \
                   WHERE timestamp > fromUnixTimestamp64Milli(?) AND {exploit} AND ja4 != '' \
                   GROUP BY ja4 HAVING uniqExact(ip) >= ? \
                   ORDER BY uniqExact(ip) DESC, ja4 LIMIT ?) \
             GROUP BY ja4, ip \
             ORDER BY fingerprint, address \
             LIMIT ?"
        );
        let rows = client
            .query(&sql)
            .bind(subjects)
            .bind(at)
            .bind(window.start_ms)
            .bind(window.start_ms)
            .bind(FINGERPRINT_ADDRESSES)
            .bind(MAX_FINGERPRINTS)
            .bind(MAX_SIGHTINGS)
            .fetch_all::<SightingRow>()
            .await
            .map_err(error("fingerprint evidence"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let exploited = row.exploits > 0;
                Some(Sighting {
                    ja4: row.fingerprint,
                    ip: parse_ip(&row.address)?,
                    exploits: row.exploits,
                    new_exploits: row.new_exploits,
                    solved: row.solved > 0,
                    user_agent: non_empty(Some(row.sample_agent)),
                    signature: non_empty(Some(row.last_signature)),
                    first_seen_ms: exploited.then_some(row.first_seen_ms),
                    last_seen_ms: exploited.then_some(row.last_seen_ms),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_800_000_000_000;
    const HOUR_MS: i64 = 60 * 60 * 1000;
    const DAY_MS: i64 = 24 * HOUR_MS;
    const SCANNER_JA4: &str = "t13d1311h2_f57a46bbacb6_ab7e3b40a677";
    const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn address(text: &str) -> AddressEvidence {
        AddressEvidence {
            ip: ip(text),
            exploits: 0,
            signature: None,
            failed_challenges: 0,
            abandoned_challenges: 0,
            new_hits: 1,
            first_seen_ms: T - HOUR_MS / 2,
            last_seen_ms: T,
        }
    }

    fn exploiting(text: &str, signature: &str) -> AddressEvidence {
        AddressEvidence {
            exploits: 1,
            signature: Some(signature.to_string()),
            ..address(text)
        }
    }

    fn sighting(ja4: &str, n: u8) -> Sighting {
        Sighting {
            ja4: ja4.to_string(),
            ip: ip(&format!("203.0.113.{n}")),
            exploits: 2,
            new_exploits: 2,
            solved: false,
            user_agent: None,
            signature: Some("CVE-2021-41773".to_string()),
            first_seen_ms: Some(T - HOUR_MS / 2 + i64::from(n)),
            last_seen_ms: Some(T - 1000 + i64::from(n)),
        }
    }

    fn scanners(ja4: &str, count: u8) -> Vec<Sighting> {
        (1..=count).map(|n| sighting(ja4, n)).collect()
    }

    #[test]
    fn one_exploit_hit_blocks_the_address_for_a_week() {
        let promoted = promotions(&[exploiting("198.51.100.7", "CVE-2021-41773")], &[]);
        assert_eq!(
            promoted,
            [Promotion {
                kind: ObservedKind::Ip,
                subject: "198.51.100.7".to_string(),
                verdict: ListVerdict::Block,
                reason: "exploit CVE-2021-41773".to_string(),
                hits: 1,
                first_seen_ms: T - HOUR_MS / 2,
                last_seen_ms: T,
                expires_at_ms: T + 7 * DAY_MS,
            }]
        );
    }

    /// D8: a probe for leaked secrets is not an exploit, but it counts.
    #[test]
    fn a_secrets_probe_counts_and_says_it_was_a_probe() {
        let promoted = promotions(&[exploiting("198.51.100.7", "secrets-probe")], &[]);
        assert_eq!(promoted[0].verdict, ListVerdict::Block);
        assert_eq!(promoted[0].reason, "probe secrets-probe");
    }

    /// An address is stored exactly and in one form, however the ingress
    /// wrote it.
    #[test]
    fn addresses_are_stored_exactly_in_canonical_form() {
        let promoted = promotions(
            &[
                exploiting("::ffff:198.51.100.7", "CVE-2021-41773"),
                exploiting("198.51.100.7", "CVE-2021-44228"),
                exploiting("2001:db8::7", "CVE-2021-44228"),
            ],
            &[],
        );
        let subjects: Vec<&str> = promoted.iter().map(|p| p.subject.as_str()).collect();
        assert_eq!(subjects, ["198.51.100.7", "2001:db8::7"]);
        assert_eq!(promoted[0].hits, 2);
    }

    #[test]
    fn three_failed_or_abandoned_challenges_challenge_the_address_for_a_day() {
        let two = AddressEvidence {
            failed_challenges: 1,
            abandoned_challenges: 1,
            ..address("198.51.100.8")
        };
        assert!(promotions(std::slice::from_ref(&two), &[]).is_empty());
        let three = AddressEvidence {
            abandoned_challenges: 2,
            ..two
        };
        let promoted = promotions(&[three], &[]);
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].verdict, ListVerdict::Challenge);
        assert_eq!(promoted[0].reason, "3 failed challenges");
        assert_eq!(promoted[0].expires_at_ms, T + DAY_MS);
    }

    /// One row per address: an exploit outweighs failed challenges.
    #[test]
    fn the_strongest_rule_wins_for_an_address() {
        let both = AddressEvidence {
            failed_challenges: 5,
            ..exploiting("198.51.100.9", "CVE-2021-41773")
        };
        let promoted = promotions(&[both], &[]);
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].verdict, ListVerdict::Block);
    }

    #[test]
    fn a_fingerprint_is_promoted_from_twenty_exploiting_addresses() {
        let nineteen = fingerprints(&scanners(SCANNER_JA4, 19));
        assert_eq!(nineteen[0].exploit_addresses, 19);
        assert!(promotions(&[], &nineteen).is_empty());

        let twenty = fingerprints(&scanners(SCANNER_JA4, 20));
        assert_eq!(twenty[0].new_hits, 40);
        let promoted = promotions(&[], &twenty);
        assert_eq!(
            promoted,
            [Promotion {
                kind: ObservedKind::Ja4,
                subject: SCANNER_JA4.to_string(),
                verdict: ListVerdict::Challenge,
                reason: "exploit CVE-2021-41773 from 20 addresses".to_string(),
                hits: 40,
                first_seen_ms: T - HOUR_MS / 2 + 1,
                last_seen_ms: T - 1000 + 20,
                expires_at_ms: T - 1000 + 20 + DAY_MS,
            }]
        );
    }

    #[test]
    fn a_fingerprint_browsers_use_is_never_promoted() {
        // On the built-in list.
        let chrome = browsers::KNOWN_BROWSERS[0].0;
        assert!(promotions(&[], &fingerprints(&scanners(chrome, 40))).is_empty());

        // Sent with a browser's User-Agent by an address with no exploit.
        let mut seen = scanners(SCANNER_JA4, 25);
        seen.push(Sighting {
            exploits: 0,
            new_exploits: 0,
            user_agent: Some(CHROME.to_string()),
            first_seen_ms: None,
            last_seen_ms: None,
            ..sighting(SCANNER_JA4, 200)
        });
        let evidence = fingerprints(&seen);
        assert_eq!(evidence[0].browser_addresses, 1);
        assert!(promotions(&[], &evidence).is_empty());

        // Used to solve a challenge, which only a browser does.
        let mut seen = scanners(SCANNER_JA4, 25);
        seen[3].solved = true;
        assert!(promotions(&[], &fingerprints(&seen)).is_empty());
    }

    /// Scanners claim to be browsers too; what they send alongside their
    /// exploits does not make their fingerprint a browser's.
    #[test]
    fn a_scanner_that_claims_a_browser_is_still_promoted() {
        let seen: Vec<Sighting> = scanners(SCANNER_JA4, 20)
            .into_iter()
            .map(|s| Sighting {
                user_agent: Some(CHROME.to_string()),
                ..s
            })
            .collect();
        let promoted = promotions(&[], &fingerprints(&seen));
        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0].kind, ObservedKind::Ja4);
    }

    #[test]
    fn a_pass_that_finds_nothing_new_writes_nothing() {
        let promotion = promotions(&[exploiting("198.51.100.7", "CVE-2021-41773")], &[])
            .pop()
            .unwrap();
        let stored = Stored {
            verdict: ListVerdict::Block,
            last_seen_ms: T,
            expires_at_ms: T + 7 * DAY_MS,
        };
        assert!(needs_write(&promotion, None));
        assert!(needs_write(&promotion, Some(&stored)));
        let unchanged = Promotion {
            hits: 0,
            ..promotion.clone()
        };
        assert!(!needs_write(&unchanged, Some(&stored)));
        // A stronger verdict is written even with nothing new counted.
        let was_challenged = Stored {
            verdict: ListVerdict::Challenge,
            expires_at_ms: T + DAY_MS,
            ..stored
        };
        assert!(needs_write(&unchanged, Some(&was_challenged)));
    }

    // ── Against the databases ───────────────────────────────────────────

    use crate::shield::ShieldSources;
    use sea_orm::{EntityTrait, QueryOrder};
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_entities::shield_snapshots;
    use temps_shield::{Action, Index, ShieldSnapshot};

    const BROWSER_LIKE_JA4: &str = "t13d1715h2_5b57614c22b0_3d5424432f57";

    /// One event, `secs_ago` before now.
    struct Event {
        secs_ago: i64,
        kind: &'static str,
        ip: String,
        ja4: Option<&'static str>,
        signature: Option<&'static str>,
        user_agent: &'static str,
    }

    fn event(secs_ago: i64, kind: &'static str, ip: &str) -> Event {
        Event {
            secs_ago,
            kind,
            ip: ip.to_string(),
            ja4: None,
            signature: None,
            user_agent: "scanner/1.0",
        }
    }

    fn exploit(secs_ago: i64, ip: &str, ja4: Option<&'static str>) -> Event {
        Event {
            ja4,
            signature: Some("CVE-2021-41773"),
            ..event(secs_ago, "verdict", ip)
        }
    }

    /// The window as every ingress would have reported it.
    fn scenario() -> Vec<Event> {
        let mut events = vec![
            // One exploit: blocked.
            exploit(600, "45.33.40.7", None),
            // Three failed challenges: challenged.
            event(600, "challenge_failed", "45.33.40.8"),
            event(540, "challenge_failed", "45.33.40.8"),
            event(480, "challenge_failed", "45.33.40.8"),
            // Three challenges never answered: challenged.
            event(1200, "challenge_issued", "45.33.40.9"),
            event(900, "challenge_issued", "45.33.40.9"),
            event(600, "challenge_issued", "45.33.40.9"),
            // An exploit from before the window: nothing.
            exploit(2 * 3600, "45.33.40.12", None),
        ];
        // Three challenges, each solved a minute later: nothing.
        for secs_ago in [1200, 900, 600] {
            events.push(event(secs_ago, "challenge_issued", "45.33.40.10"));
            events.push(event(secs_ago - 60, "challenge_solved", "45.33.40.10"));
        }
        // Three challenges served too recently to call abandoned: nothing.
        for secs_ago in [120, 60, 30] {
            events.push(event(secs_ago, "challenge_issued", "45.33.40.11"));
        }
        // Twenty addresses exploiting with one fingerprint: it is
        // challenged, and each address blocked.
        for n in 1..=20 {
            events.push(exploit(300, &format!("5.188.10.{n}"), Some(SCANNER_JA4)));
        }
        // The same with a fingerprint a browser-claiming visitor without
        // exploits also used: the addresses only.
        for n in 1..=20 {
            events.push(exploit(
                300,
                &format!("5.188.11.{n}"),
                Some(BROWSER_LIKE_JA4),
            ));
        }
        events.push(Event {
            ja4: Some(BROWSER_LIKE_JA4),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64) Gecko/20100101 Firefox/131.0",
            ..event(300, "verdict", "5.188.12.1")
        });
        events
    }

    /// What `scenario` promotes, as (kind, subject, verdict, reason).
    fn expected() -> Vec<(ObservedKind, String, ListVerdict, String)> {
        let mut expected = vec![
            (
                ObservedKind::Ip,
                "45.33.40.7".to_string(),
                ListVerdict::Block,
                "exploit CVE-2021-41773".to_string(),
            ),
            (
                ObservedKind::Ip,
                "45.33.40.8".to_string(),
                ListVerdict::Challenge,
                "3 failed challenges".to_string(),
            ),
            (
                ObservedKind::Ip,
                "45.33.40.9".to_string(),
                ListVerdict::Challenge,
                "3 failed challenges".to_string(),
            ),
            (
                ObservedKind::Ja4,
                SCANNER_JA4.to_string(),
                ListVerdict::Challenge,
                "exploit CVE-2021-41773 from 20 addresses".to_string(),
            ),
        ];
        for block in [10, 11] {
            for n in 1..=20 {
                expected.push((
                    ObservedKind::Ip,
                    format!("5.188.{block}.{n}"),
                    ListVerdict::Block,
                    "exploit CVE-2021-41773".to_string(),
                ));
            }
        }
        expected.sort();
        expected
    }

    fn summary(promotions: &[Promotion]) -> Vec<(ObservedKind, String, ListVerdict, String)> {
        let mut summary: Vec<_> = promotions
            .iter()
            .map(|p| (p.kind, p.subject.clone(), p.verdict, p.reason.clone()))
            .collect();
        summary.sort();
        summary
    }

    async fn insert_postgres(db: &DatabaseConnection, events: &[Event]) {
        for e in events {
            db.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO shield_events (timestamp, project_id, environment_id, request_id, \
                     kind, ip, ja4, verdict, mode, reasons, signature, host, path, user_agent) \
                 VALUES (now() - make_interval(secs => $1), 1, 1, 'req', $2, $3, $4, \
                     CASE WHEN $2 = 'verdict' THEN 'block' ELSE 'challenge' END, 'selective', \
                     '[]'::jsonb, $5, 'app.example.test', '/x', $6)",
                [
                    (e.secs_ago as f64).into(),
                    e.kind.into(),
                    e.ip.clone().into(),
                    e.ja4.map(str::to_string).into(),
                    e.signature.map(str::to_string).into(),
                    e.user_agent.into(),
                ],
            ))
            .await
            .unwrap();
        }
    }

    async fn latest_index(db: &DatabaseConnection) -> Index {
        let row = shield_snapshots::Entity::find()
            .order_by_desc(shield_snapshots::Column::Revision)
            .one(db)
            .await
            .unwrap()
            .unwrap();
        Index::new(&ShieldSnapshot::decode(&row.body).unwrap())
    }

    fn action(index: &Index, ip: &str, ja4: Option<&str>) -> Action {
        index
            .evaluate_client(
                None,
                Some(ip.parse().unwrap()),
                ja4,
                1,
                Utc::now().timestamp(),
            )
            .verdict
            .action
    }

    async fn observed_row(
        db: &DatabaseConnection,
        subject: &str,
    ) -> Option<(String, String, i64, i64)> {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT verdict, reason, hits, \
                 floor(extract(epoch FROM expires_at - now()))::bigint AS expires_in \
             FROM shield_observed WHERE subject = $1",
            [subject.into()],
        ))
        .await
        .unwrap()
        .map(|row| {
            (
                row.try_get("", "verdict").unwrap(),
                row.try_get("", "reason").unwrap(),
                row.try_get("", "hits").unwrap(),
                row.try_get("", "expires_in").unwrap(),
            )
        })
    }

    /// Events become observed rows, the rows become a revision every
    /// ingress acts on, and operator rules, expiry, the sharing setting
    /// and pruning all reach it.
    #[tokio::test]
    async fn promoted_observations_reach_the_snapshot() -> anyhow::Result<()> {
        let database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping Shield learning integration: Docker runtime unavailable");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let db = database.connection_arc();
        insert_postgres(&db, &scenario()).await;
        let sources = Arc::new(ShieldSources::with_feeds(db.clone(), &[]).unwrap());
        let learning = ShieldLearning::new(db.clone(), EventStore::Postgres, sources.clone());

        // The rules as the pure function applies them, on Postgres's counts.
        let now_ms = Utc::now().timestamp_millis();
        let window = Window {
            start_ms: now_ms - millis(WINDOW),
            judged_before_ms: now_ms - millis(ABANDONED_AFTER),
        };
        let txn = db.begin().await?;
        let addresses = postgres::addresses(&txn, &[], window).await?;
        let sightings = postgres::sightings(&txn, &[], window).await?;
        txn.commit().await?;
        let promoted = promotions(&addresses, &fingerprints(&sightings));
        assert_eq!(summary(&promoted), expected());

        let first = learning.promote().await?.unwrap();
        assert_eq!(first.written, expected().len());
        let (verdict, reason, hits, expires_in) = observed_row(&db, "45.33.40.7").await.unwrap();
        assert_eq!(
            (verdict.as_str(), reason.as_str(), hits),
            ("block", "exploit CVE-2021-41773", 1)
        );
        // Seven days from the hit, ten minutes ago.
        assert!(
            (7 * 86_400 - 700..7 * 86_400 - 500).contains(&expires_in),
            "{expires_in}"
        );
        let (verdict, _, hits, _) = observed_row(&db, SCANNER_JA4).await.unwrap();
        assert_eq!((verdict.as_str(), hits), ("challenge", 20));
        for absent in [
            "45.33.40.10",
            "45.33.40.11",
            "45.33.40.12",
            "5.188.12.1",
            BROWSER_LIKE_JA4,
        ] {
            assert_eq!(observed_row(&db, absent).await, None, "{absent}");
        }

        // The same evidence again changes nothing; a new hit adds one.
        assert_eq!(learning.promote().await?.unwrap().written, 0);
        insert_postgres(&db, &[exploit(5, "45.33.40.7", None)]).await;
        assert_eq!(learning.promote().await?.unwrap().written, 1);
        assert_eq!(observed_row(&db, "45.33.40.7").await.unwrap().2, 2);

        // A blocked address that then fails challenges stays blocked.
        db.execute_unprepared(
            "INSERT INTO shield_observed (kind, subject, verdict, reason, first_seen, last_seen, \
                 hits, expires_at) \
             VALUES ('ip', '45.33.40.20', 'block', 'exploit CVE-2021-44228', \
                 now() - interval '2 hours', now() - interval '2 hours', 1, now() + interval '6 days')",
        )
        .await?;
        insert_postgres(
            &db,
            &[
                event(300, "challenge_failed", "45.33.40.20"),
                event(200, "challenge_failed", "45.33.40.20"),
                event(100, "challenge_failed", "45.33.40.20"),
            ],
        )
        .await;
        assert_eq!(learning.promote().await?.unwrap().written, 1);
        let (verdict, reason, hits, expires_in) = observed_row(&db, "45.33.40.20").await.unwrap();
        assert_eq!(
            (verdict.as_str(), reason.as_str(), hits),
            ("block", "exploit CVE-2021-44228", 4)
        );
        assert!(expires_in > 5 * 86_400, "{expires_in}");

        // Compiled, every ingress acts on them.
        assert!(sources.compile_with(true).await?.is_some());
        let index = latest_index(&db).await;
        assert_eq!(action(&index, "45.33.40.7", None), Action::Block);
        assert_eq!(action(&index, "45.33.40.8", None), Action::Challenge);
        assert_eq!(
            action(&index, "8.8.4.4", Some(SCANNER_JA4)),
            Action::Challenge
        );
        assert_eq!(
            action(&index, "8.8.4.4", Some(BROWSER_LIKE_JA4)),
            Action::Pass
        );
        // Nothing new: no revision.
        assert_eq!(learning.promote().await?.unwrap().written, 0);
        assert_eq!(sources.compile_with(true).await?, None);

        // An operator allow, platform-wide, wins; removing it is a change.
        db.execute_unprepared(
            "INSERT INTO shield_operator_entries (scope, network, verdict) \
             VALUES ('platform', '45.33.40.7/32', 'allow')",
        )
        .await?;
        assert!(sources.compile_with(true).await?.is_some());
        assert_eq!(
            action(&latest_index(&db).await, "45.33.40.7", None),
            Action::Pass
        );
        db.execute_unprepared("DELETE FROM shield_operator_entries")
            .await?;
        assert!(sources.compile_with(true).await?.is_some());
        assert_eq!(
            action(&latest_index(&db).await, "45.33.40.7", None),
            Action::Block
        );

        // An expired row leaves the next revision.
        db.execute_unprepared(
            "UPDATE shield_observed SET expires_at = now() - interval '1 second' \
             WHERE subject = '45.33.40.8'",
        )
        .await?;
        assert!(sources.compile_with(true).await?.is_some());
        assert_eq!(
            action(&latest_index(&db).await, "45.33.40.8", None),
            Action::Pass
        );

        // With sharing off, nothing learned applies.
        assert!(sources.compile_with(false).await?.is_some());
        let index = latest_index(&db).await;
        assert_eq!(action(&index, "45.33.40.7", None), Action::Pass);
        assert_eq!(action(&index, "8.8.4.4", Some(SCANNER_JA4)), Action::Pass);

        // Pruning drops the expired row and the events past retention only.
        insert_postgres(&db, &[exploit(8 * 86_400, "45.33.40.30", None)]).await;
        let pruned = learning.prune().await?.unwrap();
        assert_eq!((pruned.observed, pruned.events), (1, 1));
        assert_eq!(observed_row(&db, "45.33.40.8").await, None);
        assert!(observed_row(&db, "45.33.40.7").await.is_some());
        let left: i64 = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS left FROM shield_events",
            ))
            .await?
            .unwrap()
            .try_get("", "left")?;
        assert_eq!(left, scenario().len() as i64 + 4);
        Ok(())
    }

    /// The ClickHouse queries count what the Postgres ones do.
    #[tokio::test]
    async fn clickhouse_evidence_matches_the_rules() -> anyhow::Result<()> {
        use testcontainers::{
            core::{wait::HttpWaitStrategy, ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };
        let image = GenericImage::new("clickhouse/clickhouse-server", "26.2.5")
            .with_exposed_port(ContainerPort::Tcp(8123))
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/ping")
                    .with_port(ContainerPort::Tcp(8123))
                    .with_expected_status_code(200u16),
            ))
            .with_env_var("CLICKHOUSE_DB", "shield_test")
            .with_env_var("CLICKHOUSE_PASSWORD", "test");
        let container = match image.start().await {
            Ok(container) => container,
            Err(error) => {
                eprintln!(
                    "Skipping ClickHouse Shield learning test: cannot start container ({error})"
                );
                return Ok(());
            }
        };
        let port = container.get_host_port_ipv4(8123).await?;
        let client = temps_clickhouse::build_client(
            &format!("http://127.0.0.1:{port}"),
            "shield_test",
            "default",
            "test",
            temps_clickhouse::ClickHouseCompression::default(),
        );
        let mut ready = false;
        for _ in 0..50 {
            if client.query("SELECT 1").execute().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(ready, "ClickHouse never became ready");
        // The table as the event writer creates it.
        client
            .query(
                "CREATE TABLE shield_events (
                   timestamp DateTime64(3),
                   project_id Int32,
                   environment_id Int32,
                   node_id Nullable(Int32),
                   request_id String,
                   kind LowCardinality(String),
                   ip IPv6,
                   ja4 LowCardinality(String) DEFAULT '',
                   verdict Enum8('pass' = 0, 'log' = 1, 'challenge' = 2, 'block' = 3),
                   mode LowCardinality(String),
                   reasons Array(LowCardinality(String)),
                   signature LowCardinality(String) DEFAULT '',
                   host String,
                   path String,
                   user_agent String,
                   difficulty Nullable(UInt8),
                   solve_ms Nullable(UInt32)
                 ) ENGINE = MergeTree
                 ORDER BY (environment_id, timestamp)
                 TTL toDateTime(timestamp) + INTERVAL 30 DAY",
            )
            .execute()
            .await?;
        for e in scenario() {
            client
                .query(
                    "INSERT INTO shield_events (timestamp, project_id, environment_id, request_id, \
                         kind, ip, ja4, verdict, mode, reasons, signature, host, path, user_agent) \
                     SELECT now64(3) - toIntervalSecond(?), 1, 1, 'req', ?, toIPv6(?), ?, \
                         if(? = 'verdict', 'block', 'challenge'), 'selective', [], ?, \
                         'app.example.test', '/x', ?",
                )
                .bind(e.secs_ago)
                .bind(e.kind)
                .bind(&e.ip)
                .bind(e.ja4.unwrap_or(""))
                .bind(e.kind)
                .bind(e.signature.unwrap_or(""))
                .bind(e.user_agent)
                .execute()
                .await?;
        }

        let now_ms = Utc::now().timestamp_millis();
        let window = Window {
            start_ms: now_ms - millis(WINDOW),
            judged_before_ms: now_ms - millis(ABANDONED_AFTER),
        };
        let addresses = ch::addresses(&client, &[], window).await?;
        let sightings = ch::sightings(&client, &[], window).await?;
        let promoted = promotions(&addresses, &fingerprints(&sightings));
        assert_eq!(summary(&promoted), expected());
        let abandoned = addresses.iter().find(|a| a.ip == ip("45.33.40.9")).unwrap();
        assert_eq!((abandoned.abandoned_challenges, abandoned.new_hits), (3, 3));

        // What a stored row already counted is not new.
        let blocked = addresses.iter().find(|a| a.ip == ip("45.33.40.7")).unwrap();
        let scanner = fingerprints(&sightings)
            .into_iter()
            .find(|f| f.ja4 == SCANNER_JA4)
            .unwrap();
        let since_ip = [("45.33.40.7".to_string(), blocked.last_seen_ms)];
        let again = ch::addresses(&client, &since_ip, window).await?;
        let again = again.iter().find(|a| a.ip == blocked.ip).unwrap();
        assert_eq!((again.exploits, again.new_hits), (1, 0));
        let since_ja4 = [(SCANNER_JA4.to_string(), scanner.last_seen_ms)];
        let again = fingerprints(&ch::sightings(&client, &since_ja4, window).await?);
        let again = again.iter().find(|f| f.ja4 == SCANNER_JA4).unwrap();
        assert_eq!((again.exploit_addresses, again.new_hits), (20, 0));
        Ok(())
    }
}
