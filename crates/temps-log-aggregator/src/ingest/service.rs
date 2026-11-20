// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The HTTP log ingest path (ADR 108 D1): authenticate the key, parse the
//! batch, charge the project's ingestion budget, write the lines.
//!
//! Lines go through the same [`ChunkWriterService::write_line`] a container's
//! lines do, so they share the head buffers, WAL, sealing, line index,
//! retention and purge; nothing here batches separately or writes to the
//! database per line.
//!
//! ## Streams
//!
//! A chunk carries one set of labels, so each `(key, service)` pair is its
//! own stream, `ingest-<key id>-<service>`. A key may open at most
//! [`MAX_SERVICES_PER_KEY`] distinct services per process; lines naming a
//! further service are stored under the key's default service with the name
//! they sent kept as the `ingest_service` attribute. That bounds the head
//! buffers one key can hold open no matter what a client sends.
//!
//! ## Budget
//!
//! One limiter per key, with the per-container budget and the project's
//! override a container gets ([`IngestBudget`]): a key is budgeted like one
//! container, as each container of a project is. A batch is admitted whole or refused
//! whole with `429` (see [`IngestLimiter::admit_batch`]); a refused batch is
//! counted and lists the key on the pipeline status page.
//!
//! ## Memory
//!
//! A request holds at most one parsed batch ([`ndjson::MAX_BODY_BYTES`] of
//! input, [`ndjson::MAX_LINES_PER_BATCH`] lines). Per key the service keeps a
//! limiter and a set of at most [`MAX_SERVICES_PER_KEY`] names; keys are
//! capped per project. Both maps are touched once per request, never per
//! line.
//!
//! ## Failures and revocation
//!
//! Parsing and the budget are decided for the whole batch before anything is
//! stored. A storage error part-way (a full disk) stores the lines before it
//! and fails the request; collectors deliver at least once and resend the
//! batch, so those lines can be stored twice. Revoking a key waits for its
//! in-flight batches, then seals and closes its streams. Authentication has
//! its own deadline, far shorter than the time a revoked key's gate stays
//! closed, so a check that began before a revoke cannot outlive that gate.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::{debug, error, warn};

use crate::error::LogAggregatorError;
use crate::ingest::keys::{AuthenticatedKey, LogIngestKeyService};
use crate::ingest::ndjson::{self, ParsedRecord};
use crate::services::{Admission, ChunkWriterService, IngestBudget, IngestLimiter, IngestSource};
use crate::types::ContainerContext;

/// Distinct services one key may open in this process.
pub const MAX_SERVICES_PER_KEY: usize = 16;

/// Environment label of a key that is not pinned to an environment.
pub const UNSCOPED_ENV: &str = "external";

/// What an accepted batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOutcome {
    pub accepted: usize,
}

/// Writes the lines a collector sends. Only [`ChunkWriterService`] is
/// needed for storage, so the service can be tested with a fake writer.
#[async_trait::async_trait]
pub trait IngestLineWriter: Send + Sync {
    async fn write(&self, line: crate::types::LogLine) -> Result<(), LogAggregatorError>;
    /// Seal and close every stream whose id starts with `prefix`; returns
    /// how many were closed.
    async fn close_streams(&self, prefix: &str) -> Result<usize, LogAggregatorError>;
}

#[async_trait::async_trait]
impl IngestLineWriter for Arc<ChunkWriterService> {
    async fn write(&self, line: crate::types::LogLine) -> Result<(), LogAggregatorError> {
        self.write_line(line).await
    }
    async fn close_streams(&self, prefix: &str) -> Result<usize, LogAggregatorError> {
        self.remove_streams_with_prefix(prefix).await
    }
}

/// Authenticates the key a request presents. A trait so the ingest path can
/// be tested without a database.
#[async_trait::async_trait]
pub trait IngestKeyResolver: Send + Sync {
    async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError>;
    fn record_usage(&self, key_id: i32, lines: u64, at: DateTime<Utc>);
}

#[async_trait::async_trait]
impl IngestKeyResolver for LogIngestKeyService {
    async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
        self.authenticate(presented).await
    }
    fn record_usage(&self, key_id: i32, lines: u64, at: DateTime<Utc>) {
        LogIngestKeyService::record_usage(self, key_id, lines, at)
    }
}

pub struct LogIngestService {
    keys: Arc<dyn IngestKeyResolver>,
    writer: Arc<dyn IngestLineWriter>,
    budget: Arc<IngestBudget>,
    limiters: Mutex<HashMap<i32, IngestLimiter>>,
    services: Mutex<HashMap<i32, HashSet<String>>>,
    /// When each key with state in this process last sent a batch, so
    /// [`LogIngestService::retire_idle_keys`] can release keys nobody uses,
    /// including those whose project or environment was deleted.
    last_used: Mutex<HashMap<i32, tokio::time::Instant>>,
    /// The revocation gates; see [`GateRegistry`].
    gates: Mutex<GateRegistry>,
}

/// How long a key may send nothing before its streams are closed and its
/// limiter and service names forgotten. The next batch reopens them.
pub const IDLE_KEY_RETIREMENT: Duration = Duration::from_secs(30 * 60);

/// How long a revoked key's closed gate is kept. Revoking drops the key from
/// the resolver's cache at once, so only an authentication already in flight
/// at the revoke can still reach the gate; this is far longer than one.
pub const CLOSED_GATE_RETENTION: Duration = Duration::from_secs(300);
/// Longest an ingest request may spend authenticating its key. A revoke
/// closes the key's gate and keeps it closed for [`CLOSED_GATE_RETENTION`];
/// an authentication still running past that could reach a fresh, open
/// gate with a key that is no longer active. This deadline keeps every
/// authentication far inside that retention, whatever the database does.
pub const AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(30);
fn authentication_timed_out() -> LogAggregatorError {
    LogAggregatorError::OperationTimedOut {
        operation: "authenticate log ingest key",
        target: format!("deadline {}s", AUTHENTICATION_DEADLINE.as_secs()),
    }
}

/// How often the gate registry is pruned, at most.
const GATE_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

type Gate = Arc<tokio::sync::RwLock<bool>>;

/// One gate per key in use. A batch holds its key's gate for reading while
/// it writes; revoking takes it for writing, so it waits for the key's
/// in-flight batches, marks it closed (`true`) and only then closes its
/// streams. A batch that authenticated just before the revoke and reaches
/// the gate after it is refused instead of reopening a stream.
///
/// The map stays bounded however many keys are rotated: at most once per
/// [`GATE_PRUNE_INTERVAL`], an open gate no batch holds is dropped (the next
/// batch makes a new one, and an open gate carries no state), and a closed
/// gate is dropped [`CLOSED_GATE_RETENTION`] after its revoke.
struct GateRegistry {
    gates: HashMap<i32, (Gate, Option<Instant>)>,
    last_prune: Instant,
}

impl GateRegistry {
    fn new(now: Instant) -> Self {
        Self {
            gates: HashMap::new(),
            last_prune: now,
        }
    }

    fn gate(&mut self, key_id: i32, now: Instant) -> Gate {
        if now.duration_since(self.last_prune) >= GATE_PRUNE_INTERVAL {
            self.prune(now);
        }
        self.gates.entry(key_id).or_default().0.clone()
    }

    /// Record that `gate`, now closed, belongs to a revoked key.
    fn mark_closed(&mut self, key_id: i32, gate: &Gate, now: Instant) {
        if let Some((current, closed_at)) = self.gates.get_mut(&key_id) {
            if Arc::ptr_eq(current, gate) {
                *closed_at = Some(now);
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.last_prune = now;
        // The registry lock is held, so no batch can take a gate meanwhile:
        // a strong count of one means nobody holds it.
        self.gates.retain(|_, (gate, closed_at)| match closed_at {
            Some(at) => now.duration_since(*at) < CLOSED_GATE_RETENTION,
            None => Arc::strong_count(gate) > 1,
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.gates.len()
    }
}

impl LogIngestService {
    pub fn new(
        keys: Arc<dyn IngestKeyResolver>,
        writer: Arc<dyn IngestLineWriter>,
        budget: Arc<IngestBudget>,
    ) -> Self {
        Self {
            keys,
            writer,
            budget,
            limiters: Mutex::new(HashMap::new()),
            services: Mutex::new(HashMap::new()),
            last_used: Mutex::new(HashMap::new()),
            gates: Mutex::new(GateRegistry::new(Instant::now())),
        }
    }

    /// The registry, even if a panic poisoned its lock: it holds no
    /// invariant a panic could have broken, and a revoke must reach the same
    /// gate the key's batches hold.
    fn gates(&self) -> std::sync::MutexGuard<'_, GateRegistry> {
        self.gates.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn gate(&self, key_id: i32) -> Gate {
        self.gates().gate(key_id, Instant::now())
    }

    /// After a key is revoked: wait for its in-flight batches, refuse any
    /// later one, close its streams and forget its limiter and services.
    /// Returns how many streams were closed.
    pub async fn close_key(&self, key_id: i32) -> Result<usize, LogAggregatorError> {
        let gate = self.gate(key_id);
        let mut closed = gate.write().await;
        *closed = true;
        self.gates().mark_closed(key_id, &gate, Instant::now());
        if let Ok(mut limiters) = self.limiters.lock() {
            limiters.remove(&key_id);
        }
        if let Ok(mut services) = self.services.lock() {
            services.remove(&key_id);
        }
        if let Ok(mut last_used) = self.last_used.lock() {
            last_used.remove(&key_id);
        }
        self.writer.close_streams(&stream_prefix(key_id)).await
    }

    /// Close the streams of every key that has sent nothing for `idle` and
    /// forget its limiter and service names. A key whose project or
    /// environment was deleted is never revoked, so without this its open
    /// streams (a head buffer and WAL file each) would stay until restart.
    /// Unlike a revoke the key stays usable: its next batch reopens them.
    /// Returns how many keys were retired.
    pub async fn retire_idle_keys(&self, idle: Duration) -> usize {
        let now = tokio::time::Instant::now();
        let due: Vec<i32> = match self.last_used.lock() {
            Ok(last_used) => last_used
                .iter()
                .filter(|(_, at)| now.duration_since(**at) >= idle)
                .map(|(key_id, _)| *key_id)
                .collect(),
            Err(_) => return 0,
        };
        let mut retired = 0;
        for key_id in due {
            let gate = self.gate(key_id);
            // Waits for the key's batches in flight; one that arrives now
            // waits for this and then reopens what it needs.
            let _exclusive = gate.write().await;
            let still_idle = self.last_used.lock().is_ok_and(|mut last_used| {
                let idle_now = last_used
                    .get(&key_id)
                    .is_some_and(|at| now.duration_since(*at) >= idle);
                if idle_now {
                    last_used.remove(&key_id);
                }
                idle_now
            });
            if !still_idle {
                continue;
            }
            if let Ok(mut limiters) = self.limiters.lock() {
                limiters.remove(&key_id);
            }
            if let Ok(mut services) = self.services.lock() {
                services.remove(&key_id);
            }
            match self.writer.close_streams(&stream_prefix(key_id)).await {
                Ok(closed) => {
                    debug!(
                        key_id,
                        closed, "Closed the streams of an idle log ingest key"
                    );
                    retired += 1;
                }
                Err(e) => {
                    warn!(key_id, error = %e, "Closing an idle log ingest key's streams failed");
                }
            }
        }
        retired
    }

    /// Resolve the presented key within [`AUTHENTICATION_DEADLINE`]. A slow
    /// check is refused with a retryable `503` rather than admitted late.
    async fn authenticate(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
        match tokio::time::timeout(AUTHENTICATION_DEADLINE, self.keys.resolve(presented)).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    deadline_secs = AUTHENTICATION_DEADLINE.as_secs(),
                    "Authenticating a log ingest key timed out; the batch was refused"
                );
                Err(authentication_timed_out())
            }
        }
    }

    /// Ingest one request body sent with `presented` as its key.
    pub async fn ingest(
        &self,
        presented: &str,
        body: &[u8],
    ) -> Result<IngestOutcome, LogAggregatorError> {
        let started = tokio::time::Instant::now();
        let key = self.authenticate(presented).await?;
        let gate = self.gate(key.key_id);
        let closed = gate.read().await;
        if *closed {
            return Err(LogAggregatorError::IngestKeyRejected);
        }
        // The timeout above can still yield a key read before a revoke if
        // this task was starved past the deadline. Checked after the gate
        // was looked up: a revoke since `started` left a closed gate that
        // outlives the deadline, so a fresh check means the lookup saw it.
        if started.elapsed() >= AUTHENTICATION_DEADLINE {
            return Err(authentication_timed_out());
        }
        if let Ok(mut last_used) = self.last_used.lock() {
            last_used.insert(key.key_id, tokio::time::Instant::now());
        }
        let now = Utc::now();
        let records = ndjson::parse_batch(body, now)?;
        if records.is_empty() {
            return Ok(IngestOutcome { accepted: 0 });
        }

        let (admission, notice) = self.charge(&key, records.len() as u64, body.len() as u64, now);
        if let Some(notice) = notice {
            // Explains an earlier refused batch where its gap is seen.
            if let Err(e) = self.writer.write(notice).await {
                error!(key_id = key.key_id, error = %e, "Failed to write a log ingest drop notice");
            }
        }
        if let Admission::Drop(reason) = admission {
            return Err(LogAggregatorError::IngestBudgetExceeded {
                project_id: key.project_id,
                accepted: 0,
                total: records.len(),
                reason: reason.to_string(),
            });
        }

        let total = records.len();
        let lines = self.label(&key, records);
        for (written, line) in (0_u64..).zip(lines) {
            if let Err(e) = self.writer.write(line).await {
                // The lines before this one are stored; count them, so a
                // key's usage matches what it delivered. The collector will
                // retry the whole batch, which may store that prefix again.
                if written > 0 {
                    self.keys.record_usage(key.key_id, written, now);
                }
                error!(
                    key_id = key.key_id,
                    project_id = key.project_id,
                    written,
                    total,
                    error = %e,
                    "Storing a log ingest batch failed part-way"
                );
                return Err(e);
            }
        }
        drop(closed);
        self.keys.record_usage(key.key_id, total as u64, now);
        debug!(
            key_id = key.key_id,
            project_id = key.project_id,
            lines = total,
            "Ingested a log batch"
        );
        Ok(IngestOutcome { accepted: total })
    }

    /// Charge the key's limiter for a batch, and collect a pending drop
    /// notice. The limiter map is locked once for the whole decision.
    fn charge(
        &self,
        key: &AuthenticatedKey,
        lines: u64,
        bytes: u64,
        now: DateTime<Utc>,
    ) -> (Admission, Option<crate::types::LogLine>) {
        let Ok(mut limiters) = self.limiters.lock() else {
            error!(
                key_id = key.key_id,
                "log ingest limiter registry is poisoned; admitting the batch"
            );
            return (Admission::Admit, None);
        };
        let limiter = limiters
            .entry(key.key_id)
            .or_insert_with(|| self.budget.limiter(source_for(key)));
        let admission = limiter.admit_batch(lines, bytes, now);
        (admission, limiter.take_drop_notice())
    }

    /// Give each record its stream's labels.
    fn label(
        &self,
        key: &AuthenticatedKey,
        records: Vec<ParsedRecord>,
    ) -> Vec<crate::types::LogLine> {
        let mut services = self.services.lock().ok();
        let mut contexts: HashMap<String, ContainerContext> = HashMap::new();
        records
            .into_iter()
            .map(|mut record| {
                let requested = record.service.take();
                let service = match (&requested, services.as_mut()) {
                    (Some(name), Some(services)) => {
                        let seen = services.entry(key.key_id).or_default();
                        if seen.contains(name) || seen.len() < MAX_SERVICES_PER_KEY {
                            seen.insert(name.clone());
                            name.clone()
                        } else {
                            record.object.insert(
                                "ingest_service".to_string(),
                                serde_json::Value::String(name.clone()),
                            );
                            key.default_service.clone()
                        }
                    }
                    (Some(name), None) => name.clone(),
                    (None, _) => key.default_service.clone(),
                };
                let ctx = contexts
                    .entry(service.clone())
                    .or_insert_with(|| context_for(key, &service));
                record.into_line(ctx)
            })
            .collect()
    }
}

/// Stream id of a key's service. Safe as a WAL file name: the service is
/// already reduced to `[a-z0-9._-]`.
pub fn stream_id(key_id: i32, service: &str) -> String {
    format!("{}{service}", stream_prefix(key_id))
}

/// The prefix every stream of `key_id` starts with. Ends in `-`, so key 1
/// never matches key 12's streams.
pub fn stream_prefix(key_id: i32) -> String {
    format!("ingest-{key_id}-")
}

fn context_for(key: &AuthenticatedKey, service: &str) -> ContainerContext {
    ContainerContext {
        project_id: key.project_id,
        external_service_id: None,
        env: key
            .environment_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| UNSCOPED_ENV.to_string()),
        service: service.to_string(),
        container_id: stream_id(key.key_id, service),
        deploy_id: None,
    }
}

/// The budget source for a key: its default stream, so the pipeline page
/// names the key's stream and a drop notice lands in it.
fn source_for(key: &AuthenticatedKey) -> IngestSource {
    IngestSource {
        ctx: context_for(key, &key.default_service),
        node_id: None,
        node_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::IngestLimits;
    use crate::types::LogLine;

    struct FixedKey(AuthenticatedKey, Mutex<Vec<u64>>);

    #[async_trait::async_trait]
    impl IngestKeyResolver for FixedKey {
        async fn resolve(&self, presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
            if presented == "good" {
                Ok(self.0.clone())
            } else {
                Err(LogAggregatorError::IngestKeyRejected)
            }
        }
        fn record_usage(&self, _key_id: i32, lines: u64, _at: DateTime<Utc>) {
            self.1.lock().expect("usage lock").push(lines);
        }
    }

    /// Stored lines, closed stream prefixes, and the number of writes that
    /// succeed before every later one fails (`None`: never fails).
    #[derive(Default)]
    struct Recorded(Mutex<Vec<LogLine>>, Mutex<Vec<String>>, Option<usize>);

    #[async_trait::async_trait]
    impl IngestLineWriter for Recorded {
        async fn write(&self, line: LogLine) -> Result<(), LogAggregatorError> {
            let mut lines = self.0.lock().expect("lines lock");
            if self.2.is_some_and(|limit| lines.len() >= limit) {
                return Err(LogAggregatorError::Validation {
                    message: "disk full in this test".to_string(),
                });
            }
            lines.push(line);
            Ok(())
        }
        async fn close_streams(&self, prefix: &str) -> Result<usize, LogAggregatorError> {
            self.1.lock().expect("closed lock").push(prefix.to_string());
            Ok(1)
        }
    }

    fn key() -> AuthenticatedKey {
        AuthenticatedKey {
            key_id: 5,
            project_id: 7,
            environment_id: Some(3),
            default_service: "external".to_string(),
        }
    }

    fn service(limits: IngestLimits) -> (LogIngestService, Arc<Recorded>) {
        let writer = Arc::new(Recorded::default());
        let service = LogIngestService::new(
            Arc::new(FixedKey(key(), Mutex::default())),
            writer.clone(),
            Arc::new(IngestBudget::new(limits)),
        );
        (service, writer)
    }

    #[tokio::test]
    async fn a_revoked_key_is_refused_and_its_streams_are_closed() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        service
            .ingest("good", b"{\"msg\":\"a\"}")
            .await
            .expect("accepted");
        let closed = service.close_key(5).await.expect("closed");
        assert_eq!(closed, 1);
        assert_eq!(
            *writer.1.lock().expect("closed lock"),
            vec!["ingest-5-".to_string()]
        );
        // A batch that authenticated before the revoke (the resolver here
        // still accepts the key) is refused at the gate, not written into a
        // reopened stream.
        assert!(matches!(
            service.ingest("good", b"{\"msg\":\"b\"}").await,
            Err(LogAggregatorError::IngestKeyRejected)
        ));
        assert_eq!(writer.0.lock().expect("lines lock").len(), 1);
    }

    /// A resolver whose check never finishes, standing in for a stalled
    /// database.
    struct StalledKey;

    #[async_trait::async_trait]
    impl IngestKeyResolver for StalledKey {
        async fn resolve(&self, _presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
            std::future::pending().await
        }
        fn record_usage(&self, _key_id: i32, _lines: u64, _at: DateTime<Utc>) {}
    }

    /// A resolver that answers only once the deadline and a closed gate's
    /// retention have both passed, as a starved task would.
    struct LateKey(AuthenticatedKey);

    #[async_trait::async_trait]
    impl IngestKeyResolver for LateKey {
        async fn resolve(&self, _presented: &str) -> Result<AuthenticatedKey, LogAggregatorError> {
            tokio::time::advance(CLOSED_GATE_RETENTION + Duration::from_secs(1)).await;
            Ok(self.0.clone())
        }
        fn record_usage(&self, _key_id: i32, _lines: u64, _at: DateTime<Utc>) {}
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_resolved_after_the_deadline_is_refused() {
        let writer = Arc::new(Recorded::default());
        let service = LogIngestService::new(
            Arc::new(LateKey(key())),
            writer.clone(),
            Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
        );
        let result = service.ingest("good", b"{\"msg\":\"a\"}").await;
        assert!(matches!(
            result,
            Err(LogAggregatorError::OperationTimedOut { .. })
        ));
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_key_check_is_refused_before_a_closed_gate_retires() {
        let writer = Arc::new(Recorded::default());
        let service = LogIngestService::new(
            Arc::new(StalledKey),
            writer.clone(),
            Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
        );
        assert!(AUTHENTICATION_DEADLINE < CLOSED_GATE_RETENTION / 2);
        let started = tokio::time::Instant::now();
        let result = service.ingest("good", b"{\"msg\":\"a\"}").await;
        assert!(matches!(
            result,
            Err(LogAggregatorError::OperationTimedOut { operation, .. })
                if operation == "authenticate log ingest key"
        ));
        assert!(started.elapsed() >= AUTHENTICATION_DEADLINE);
        assert!(started.elapsed() < CLOSED_GATE_RETENTION);
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_key_is_retired_and_can_send_again() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        service
            .ingest("good", b"{\"msg\":\"a\"}")
            .await
            .expect("accepted");
        assert_eq!(service.retire_idle_keys(IDLE_KEY_RETIREMENT).await, 0);
        assert!(writer.1.lock().expect("closed lock").is_empty());

        tokio::time::advance(IDLE_KEY_RETIREMENT).await;
        assert_eq!(service.retire_idle_keys(IDLE_KEY_RETIREMENT).await, 1);
        assert_eq!(
            *writer.1.lock().expect("closed lock"),
            vec!["ingest-5-".to_string()]
        );
        assert!(service.limiters.lock().expect("limiters").is_empty());
        assert!(service.services.lock().expect("services").is_empty());
        assert_eq!(
            service.retire_idle_keys(IDLE_KEY_RETIREMENT).await,
            0,
            "a retired key is not retired twice"
        );

        // Not a revoke: the key's next batch is stored.
        service
            .ingest("good", b"{\"msg\":\"b\"}")
            .await
            .expect("accepted after retirement");
        assert_eq!(writer.0.lock().expect("lines lock").len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_in_use_is_not_retired() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        service
            .ingest("good", b"{\"msg\":\"a\"}")
            .await
            .expect("accepted");
        tokio::time::advance(IDLE_KEY_RETIREMENT / 2).await;
        service
            .ingest("good", b"{\"msg\":\"b\"}")
            .await
            .expect("accepted");
        tokio::time::advance(IDLE_KEY_RETIREMENT / 2).await;
        assert_eq!(service.retire_idle_keys(IDLE_KEY_RETIREMENT).await, 0);
        assert!(writer.1.lock().expect("closed lock").is_empty());
    }

    #[tokio::test]
    async fn a_revoke_waits_for_a_batch_in_flight() {
        let (service, _writer) = service(IngestLimits::UNLIMITED);
        let gate = service.gate(5);
        let in_flight = gate.read().await;
        let service = Arc::new(service);
        let closing = tokio::spawn({
            let service = service.clone();
            async move { service.close_key(5).await }
        });
        tokio::task::yield_now().await;
        assert!(!closing.is_finished(), "the revoke did not wait");
        drop(in_flight);
        closing.await.expect("join").expect("closed");
    }

    #[tokio::test]
    async fn a_closed_gate_refuses_until_it_is_retired() {
        let start = Instant::now();
        let mut registry = GateRegistry::new(start);
        let gate = registry.gate(5, start);
        *gate.write().await = true;
        registry.mark_closed(5, &gate, start);
        drop(gate);

        // Pruned within the retention: the key's gate is still closed.
        let soon = start + GATE_PRUNE_INTERVAL;
        assert!(*registry.gate(5, soon).read().await, "still refused");
        assert_eq!(registry.len(), 1);

        // Pruned after it: the entry is gone, and a new gate is open.
        let later = soon + CLOSED_GATE_RETENTION;
        registry.prune(later);
        assert_eq!(registry.len(), 0, "the revoked key's gate is retired");
    }

    #[test]
    fn rotating_keys_does_not_grow_the_gate_registry() {
        let start = Instant::now();
        let mut registry = GateRegistry::new(start);
        let held = registry.gate(1, start);
        // Many keys each used once and let go, as rotations would.
        for key_id in 2..1_000 {
            drop(registry.gate(key_id, start));
        }
        assert_eq!(registry.len(), 999);
        // The next lookup after the interval prunes: only the gate a batch
        // still holds survives, plus the one just asked for.
        let _again = registry.gate(2, start + GATE_PRUNE_INTERVAL);
        assert_eq!(registry.len(), 2);
        assert!(Arc::ptr_eq(
            &held,
            &registry.gate(1, start + GATE_PRUNE_INTERVAL)
        ));
    }

    #[test]
    fn one_keys_prefix_never_matches_another_keys_streams() {
        assert!(stream_id(12, "web").starts_with(&stream_prefix(12)));
        assert!(!stream_id(12, "web").starts_with(&stream_prefix(1)));
    }

    #[tokio::test]
    async fn a_batch_that_fails_part_way_counts_the_lines_it_stored() {
        let writer = Arc::new(Recorded(Mutex::default(), Mutex::default(), Some(2)));
        let keys = Arc::new(FixedKey(key(), Mutex::default()));
        let service = LogIngestService::new(
            keys.clone(),
            writer.clone(),
            Arc::new(IngestBudget::new(IngestLimits::UNLIMITED)),
        );
        let body = b"{\"msg\":\"a\"}\n{\"msg\":\"b\"}\n{\"msg\":\"c\"}\n";
        assert!(service.ingest("good", body).await.is_err());
        assert_eq!(writer.0.lock().expect("lines lock").len(), 2);
        assert_eq!(*keys.1.lock().expect("usage lock"), vec![2]);
    }

    #[tokio::test]
    async fn a_batch_is_stored_under_the_keys_project_environment_and_service_streams() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body = b"{\"msg\":\"a\",\"service\":\"web\"}\n{\"msg\":\"b\"}\n";
        let outcome = service.ingest("good", body).await.expect("accepted");
        assert_eq!(outcome.accepted, 2);
        let lines = writer.0.lock().expect("lines lock");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].project_id, 7);
        assert_eq!(lines[0].env, "3");
        assert_eq!(lines[0].service, "web");
        assert_eq!(lines[0].container_id, "ingest-5-web");
        assert_eq!(lines[1].service, "external");
        assert_eq!(lines[1].container_id, "ingest-5-external");
        assert!(lines
            .iter()
            .all(|l| l.deploy_id.is_none() && l.external_service_id.is_none()));
    }

    #[tokio::test]
    async fn a_line_cannot_choose_another_project() {
        // The project, environment and stream come from the key; a record
        // carrying owner-shaped keys keeps them as attributes only.
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body = br#"{"msg":"x","project_id":99,"env":"1","container_id":"abc"}"#;
        service.ingest("good", body).await.expect("accepted");
        let lines = writer.0.lock().expect("lines lock");
        assert_eq!(lines[0].project_id, 7);
        assert_eq!(lines[0].env, "3");
        assert_eq!(lines[0].container_id, "ingest-5-external");
    }

    #[tokio::test]
    async fn an_unknown_key_stores_nothing() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        assert!(matches!(
            service.ingest("bad", b"{\"msg\":\"a\"}").await,
            Err(LogAggregatorError::IngestKeyRejected)
        ));
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test]
    async fn an_invalid_line_stores_nothing_of_its_batch() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let result = service.ingest("good", b"{\"msg\":\"a\"}\nnope\n").await;
        assert!(matches!(
            result,
            Err(LogAggregatorError::IngestLineInvalid { line: 2, .. })
        ));
        assert!(writer.0.lock().expect("lines lock").is_empty());
    }

    #[tokio::test]
    async fn an_over_budget_batch_is_refused_whole_and_the_gap_is_explained_later() {
        // 2 lines/s with a 10 s burst: 20 lines fit.
        let (service, writer) = service(IngestLimits {
            lines_per_sec: 2,
            bytes_per_sec: 0,
        });
        let batch = "{\"msg\":\"a\"}\n".repeat(20);
        service
            .ingest("good", batch.as_bytes())
            .await
            .expect("first batch fits");
        let refused = service.ingest("good", batch.as_bytes()).await;
        match refused {
            Err(LogAggregatorError::IngestBudgetExceeded {
                project_id,
                accepted,
                total,
                ..
            }) => assert_eq!((project_id, accepted, total), (7, 0, 20)),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            writer.0.lock().expect("lines lock").len(),
            20,
            "nothing of the refused batch was stored"
        );
    }

    #[tokio::test]
    async fn a_key_cannot_open_unbounded_streams() {
        let (service, writer) = service(IngestLimits::UNLIMITED);
        let body: String = (0..MAX_SERVICES_PER_KEY + 5)
            .map(|i| format!("{{\"msg\":\"m\",\"service\":\"svc{i}\"}}\n"))
            .collect();
        service
            .ingest("good", body.as_bytes())
            .await
            .expect("accepted");
        let lines = writer.0.lock().expect("lines lock");
        let streams: HashSet<&str> = lines.iter().map(|l| l.container_id.as_str()).collect();
        assert_eq!(
            streams.len(),
            MAX_SERVICES_PER_KEY + 1,
            "the cap plus the default stream"
        );
        let folded = lines.iter().filter(|l| l.service == "external").count();
        assert_eq!(folded, 5);
        let kept = lines
            .iter()
            .find(|l| l.service == "external")
            .and_then(|l| l.fields.as_ref())
            .and_then(|f| f.get("ingest_service"))
            .cloned();
        assert!(kept.is_some(), "the requested name is kept as an attribute");
    }

    #[test]
    fn stream_ids_are_file_name_safe() {
        let id = stream_id(12, "nginx.access");
        assert_eq!(id, "ingest-12-nginx.access");
        assert!(!id.contains('/'));
    }
}
