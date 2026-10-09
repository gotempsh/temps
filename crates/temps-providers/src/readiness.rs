// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup readiness gate for managed services.
//!
//! Some engines report a running container well before they can serve a
//! request. RustFS answers `/health` with 200 while its storage layer is
//! still initializing (or has failed to), and every authenticated S3 call
//! returns 503 meanwhile. Marking such a service `running` as soon as its
//! container starts tells the operator it is usable when it is not.
//!
//! An engine opts in by returning a [`ReadinessTarget`] from
//! `ExternalService::readiness_target`. The manager then keeps the service
//! `starting` while [`drive_readiness`] polls the target. The first usable
//! answer makes it `running`. A fatal initialization error in the logs, a
//! restart loop, or the deadline makes it `failed`, with an
//! [`InitializationFailure`] that says why, shows the evidence, and lists
//! what the operator can do next.
//!
//! The gate only observes. It never stops, recreates or deletes a container
//! or a volume: recovering from a failed initialization is the operator's
//! call, because the fix (fresh volumes) can destroy data.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{sea_query::Expr, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use temps_entities::external_services;
use tracing::{info, warn};
use utoipa::ToSchema;

/// Key under `external_services.health_metadata` holding the
/// [`ServiceReadiness`] of a service that is `starting` or failed to start.
pub const READINESS_METADATA_KEY: &str = "readiness";

/// Lifecycle status a gated service holds until the gate decides.
pub const STARTING_STATUS: &str = "starting";

/// How long and how hard the gate waits for one engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessPolicy {
    /// Report failure after this long without a usable answer.
    pub deadline: Duration,
    /// Time between observations.
    pub poll_interval: Duration,
    /// Container restarts, counted from when the gate started, that make a
    /// restart loop. Each restart means the engine's process exited.
    pub max_restarts: i64,
}

/// One look at a starting service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadinessObservation {
    /// `None` when the service answered a real, authenticated request.
    /// Otherwise the typed reason it could not (never contains secrets).
    pub not_ready_reason: Option<String>,
    /// Docker's restart count for the container, when it could be read.
    pub restart_count: Option<i64>,
    /// Log lines proving initialization failed for good. Non-empty means
    /// waiting longer cannot help.
    pub fatal_log_lines: Vec<String>,
}

/// Something the gate can watch: one engine's view of one service.
#[async_trait]
pub trait ReadinessTarget: Send + Sync {
    fn policy(&self) -> ReadinessPolicy;

    /// Probe the service once. Must bound its own I/O, so one slow probe
    /// cannot stall the gate past its deadline by much.
    async fn observe(&self) -> ReadinessObservation;

    /// Recent log lines worth showing with a failure. Called once, when the
    /// gate gives up. Must bound its own I/O and redact secrets.
    async fn log_excerpt(&self) -> Vec<String>;
}

/// Where the gate is for a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessPhase {
    /// Container started; waiting for a usable answer.
    Starting,
    /// The gate gave up. See `failure`.
    Failed,
}

/// Why the gate gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InitializationFailureKind {
    /// The engine logged an initialization error it does not recover from.
    StoreInitFailed,
    /// The container kept exiting and restarting.
    RestartLoop,
    /// No usable answer before the deadline.
    Timeout,
}

/// What the operator can do about a failed initialization. Temps never
/// does any of these on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessNextAction {
    /// Read the container's logs.
    ViewLogs,
    /// Start the service again, which restarts the readiness check.
    Retry,
    /// Switch the service to another image version (Upgrade).
    TryAnotherImage,
    /// Delete the service and create it again on fresh volumes. Deleting
    /// removes the service's data volumes, so only do this when they hold
    /// nothing worth keeping.
    RecreateWithFreshVolumes,
}

/// A failed initialization, as shown to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InitializationFailure {
    pub kind: InitializationFailureKind,
    /// What happened, including the last probe's typed reason.
    pub reason: String,
    /// Container log lines that explain the failure, oldest first. Secrets
    /// are redacted. Empty when the logs could not be read.
    pub log_excerpt: Vec<String>,
    /// Suggested next steps, most useful first.
    pub next_actions: Vec<ReadinessNextAction>,
}

/// Startup readiness of a service that is `starting` or failed to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceReadiness {
    pub phase: ReadinessPhase,
    /// When the gate started waiting (ISO 8601, UTC).
    pub started_at: String,
    /// How long the gate waits before reporting failure.
    pub deadline_secs: u64,
    /// Latest reason the service is not usable yet, from the engine's
    /// authenticated probe. Null before the first probe.
    pub reason: Option<String>,
    /// Container restarts since the gate started.
    pub restart_count: i64,
    /// Set when `phase` is `failed`.
    pub failure: Option<InitializationFailure>,
}

impl ServiceReadiness {
    pub fn starting(started_at: DateTime<Utc>, policy: &ReadinessPolicy) -> Self {
        Self {
            phase: ReadinessPhase::Starting,
            started_at: started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            deadline_secs: policy.deadline.as_secs(),
            reason: None,
            restart_count: 0,
            failure: None,
        }
    }

    /// Read the readiness snapshot out of a service's `health_metadata`.
    /// `None` when absent or unparsable (never an error: the snapshot is
    /// advisory, the service row's `status` is authoritative).
    pub fn from_health_metadata(metadata: Option<&serde_json::Value>) -> Option<Self> {
        let value = metadata?.get(READINESS_METADATA_KEY)?;
        match serde_json::from_value(value.clone()) {
            Ok(readiness) => Some(readiness),
            Err(e) => {
                warn!("health_metadata.{READINESS_METADATA_KEY} did not parse: {e}");
                None
            }
        }
    }

    fn started_at_utc(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.started_at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }
}

/// The gate's decision after one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadinessVerdict {
    Starting,
    Ready,
    Failed(InitializationFailure),
}

/// Decide what one observation means. Pure, so every transition is unit
/// tested without a container.
pub(crate) fn evaluate(
    policy: &ReadinessPolicy,
    elapsed: Duration,
    restarts: i64,
    observation: &ReadinessObservation,
) -> ReadinessVerdict {
    let Some(reason) = observation.not_ready_reason.as_deref() else {
        return ReadinessVerdict::Ready;
    };

    if !observation.fatal_log_lines.is_empty() {
        return ReadinessVerdict::Failed(InitializationFailure {
            kind: InitializationFailureKind::StoreInitFailed,
            reason: format!(
                "Initialization failed: the container logged a storage initialization error \
                 it does not recover from ({restarts} restart(s) so far). Last probe: {reason}"
            ),
            log_excerpt: observation.fatal_log_lines.clone(),
            next_actions: vec![
                ReadinessNextAction::ViewLogs,
                ReadinessNextAction::RecreateWithFreshVolumes,
                ReadinessNextAction::TryAnotherImage,
            ],
        });
    }

    if restarts >= policy.max_restarts {
        return ReadinessVerdict::Failed(InitializationFailure {
            kind: InitializationFailureKind::RestartLoop,
            reason: format!(
                "Initialization failed: the container restarted {restarts} time(s) in {}s \
                 without serving an authenticated request. Last probe: {reason}",
                elapsed.as_secs()
            ),
            log_excerpt: Vec::new(),
            next_actions: vec![
                ReadinessNextAction::ViewLogs,
                ReadinessNextAction::TryAnotherImage,
                ReadinessNextAction::RecreateWithFreshVolumes,
            ],
        });
    }

    if elapsed >= policy.deadline {
        return ReadinessVerdict::Failed(InitializationFailure {
            kind: InitializationFailureKind::Timeout,
            reason: format!(
                "Initialization did not finish within {}s: the service never served an \
                 authenticated request ({restarts} restart(s)). Last probe: {reason}",
                policy.deadline.as_secs()
            ),
            log_excerpt: Vec::new(),
            next_actions: vec![
                ReadinessNextAction::ViewLogs,
                ReadinessNextAction::Retry,
                ReadinessNextAction::TryAnotherImage,
            ],
        });
    }

    ReadinessVerdict::Starting
}

/// Restarts seen since the gate started. Docker's count is per container,
/// and the container can be replaced mid-gate (applying the metrics ingest
/// key recreates it), which resets the count; so only increases count.
#[derive(Debug, Default)]
struct RestartCounter {
    last_seen: Option<i64>,
    total: i64,
}

impl RestartCounter {
    fn resume(total: i64) -> Self {
        Self {
            last_seen: None,
            total,
        }
    }

    fn observe(&mut self, restart_count: Option<i64>) -> i64 {
        if let Some(count) = restart_count {
            if let Some(last) = self.last_seen {
                self.total += (count - last).max(0);
            }
            self.last_seen = Some(count);
        }
        self.total
    }
}

/// Where the gate records what it sees. Every method reports whether the
/// service is still waiting on this gate; `false` (stopped, deleted, or
/// restarted by someone else) ends the watch without a verdict.
#[async_trait]
pub(crate) trait ReadinessSink: Send + Sync {
    async fn still_starting(&self) -> bool;
    async fn starting(&self, readiness: &ServiceReadiness) -> bool;
    async fn ready(&self) -> bool;
    async fn failed(&self, readiness: &ServiceReadiness) -> bool;
}

/// How a watch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadinessOutcome {
    Ready,
    Failed(ServiceReadiness),
    /// The service left `starting` for another reason, or a newer watch
    /// took over.
    Abandoned,
}

/// Poll `target` until it is ready or the policy gives up, reporting every
/// change to `sink`. `prior` resumes a watch interrupted by a restart of
/// Temps, so its start time and restart count carry over.
pub(crate) async fn drive_readiness(
    target: &dyn ReadinessTarget,
    sink: &dyn ReadinessSink,
    prior: ServiceReadiness,
    superseded: impl Fn() -> bool + Send + Sync,
) -> ReadinessOutcome {
    let policy = target.policy();
    let started_at = prior.started_at_utc().unwrap_or_else(Utc::now);
    let mut restarts = RestartCounter::resume(prior.restart_count);
    let mut readiness = prior;
    let mut recorded: Option<(Option<String>, i64)> = None;

    loop {
        if superseded() || !sink.still_starting().await {
            return ReadinessOutcome::Abandoned;
        }

        let observation = target.observe().await;
        let elapsed = (Utc::now() - started_at).to_std().unwrap_or_default();
        let restart_total = restarts.observe(observation.restart_count);

        match evaluate(&policy, elapsed, restart_total, &observation) {
            ReadinessVerdict::Ready => {
                return if sink.ready().await {
                    ReadinessOutcome::Ready
                } else {
                    ReadinessOutcome::Abandoned
                };
            }
            ReadinessVerdict::Failed(mut failure) => {
                if failure.log_excerpt.is_empty() {
                    failure.log_excerpt = target.log_excerpt().await;
                }
                readiness.phase = ReadinessPhase::Failed;
                readiness.reason = observation.not_ready_reason;
                readiness.restart_count = restart_total;
                readiness.failure = Some(failure);
                return if sink.failed(&readiness).await {
                    ReadinessOutcome::Failed(readiness)
                } else {
                    ReadinessOutcome::Abandoned
                };
            }
            ReadinessVerdict::Starting => {
                // Write only on change: a healthy start writes once or twice,
                // not once per poll.
                let snapshot = (observation.not_ready_reason.clone(), restart_total);
                if recorded.as_ref() != Some(&snapshot) {
                    readiness.reason = observation.not_ready_reason;
                    readiness.restart_count = restart_total;
                    if !sink.starting(&readiness).await {
                        return ReadinessOutcome::Abandoned;
                    }
                    recorded = Some(snapshot);
                }
            }
        }

        tokio::time::sleep(policy.poll_interval).await;
    }
}

/// Live watches, keyed by service id, holding the generation of the watch
/// that owns each service. Process-wide rather than per manager: several
/// `ExternalServiceManager` instances can exist in one process, and two
/// watches on one service would only race each other.
static WATCHES: LazyLock<Mutex<HashMap<i32, u64>>> = LazyLock::new(Default::default);
static NEXT_WATCH_GENERATION: AtomicU64 = AtomicU64::new(1);

fn watches() -> std::sync::MutexGuard<'static, HashMap<i32, u64>> {
    // The map holds plain integers, so a panic elsewhere cannot leave it
    // inconsistent; recover from poisoning instead of propagating it.
    WATCHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ownership of the readiness watch for one service. Dropping it releases
/// the service, unless a newer watch has already taken over.
pub(crate) struct WatchRegistration {
    service_id: i32,
    generation: u64,
}

impl WatchRegistration {
    /// Claim the watch for `service_id`. A fresh start (`supersede`) always
    /// wins and makes any older watch stand down; a resume only claims a
    /// service nobody is watching.
    pub(crate) fn claim(service_id: i32, supersede: bool) -> Option<Self> {
        let mut watches = watches();
        if !supersede && watches.contains_key(&service_id) {
            return None;
        }
        let generation = NEXT_WATCH_GENERATION.fetch_add(1, Ordering::Relaxed);
        watches.insert(service_id, generation);
        Some(Self {
            service_id,
            generation,
        })
    }

    /// Whether this is still the watch that owns the service.
    pub(crate) fn is_current(&self) -> bool {
        watches().get(&self.service_id) == Some(&self.generation)
    }
}

impl Drop for WatchRegistration {
    fn drop(&mut self) {
        let mut watches = watches();
        if watches.get(&self.service_id) == Some(&self.generation) {
            watches.remove(&self.service_id);
        }
    }
}

/// [`ReadinessSink`] backed by the service's `external_services` row. Every
/// write is conditional on `status = 'starting'`, so a stop, delete or
/// restart that lands mid-watch always wins over a late verdict.
pub(crate) struct DbReadinessSink {
    pub db: Arc<DatabaseConnection>,
    pub service_id: i32,
}

impl DbReadinessSink {
    async fn current(&self) -> Option<external_services::Model> {
        match external_services::Entity::find_by_id(self.service_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(row) => row,
            Err(e) => {
                warn!(
                    "Readiness watch for service {} could not read its row: {}",
                    self.service_id, e
                );
                None
            }
        }
    }

    /// Conditionally update the row; `true` when it was still `starting`.
    async fn update_if_starting(
        &self,
        status: &str,
        error_message: Option<String>,
        readiness: Option<&ServiceReadiness>,
    ) -> bool {
        let Some(row) = self.current().await else {
            return false;
        };
        if row.status != STARTING_STATUS {
            return false;
        }
        let metadata = with_readiness_metadata(row.health_metadata.as_ref(), readiness);
        let result = external_services::Entity::update_many()
            .col_expr(external_services::Column::Status, Expr::value(status))
            .col_expr(
                external_services::Column::ErrorMessage,
                Expr::value(error_message),
            )
            .col_expr(
                external_services::Column::HealthMetadata,
                Expr::value(metadata),
            )
            .col_expr(
                external_services::Column::UpdatedAt,
                Expr::value(Utc::now()),
            )
            .filter(external_services::Column::Id.eq(self.service_id))
            .filter(external_services::Column::Status.eq(STARTING_STATUS))
            .exec(self.db.as_ref())
            .await;
        match result {
            Ok(res) => res.rows_affected > 0,
            Err(e) => {
                warn!(
                    "Readiness watch for service {} could not record status '{}': {}",
                    self.service_id, status, e
                );
                // Keep watching: the next poll retries the write.
                true
            }
        }
    }
}

#[async_trait]
impl ReadinessSink for DbReadinessSink {
    async fn still_starting(&self) -> bool {
        self.current()
            .await
            .is_some_and(|row| row.status == STARTING_STATUS)
    }

    async fn starting(&self, readiness: &ServiceReadiness) -> bool {
        self.update_if_starting(STARTING_STATUS, None, Some(readiness))
            .await
    }

    async fn ready(&self) -> bool {
        let ready = self.update_if_starting("running", None, None).await;
        if ready {
            info!(
                "Service {} passed its readiness check and is now running",
                self.service_id
            );
        }
        ready
    }

    async fn failed(&self, readiness: &ServiceReadiness) -> bool {
        let reason = readiness
            .failure
            .as_ref()
            .map(|f| f.reason.clone())
            .unwrap_or_else(|| "Initialization failed".to_string());
        warn!(
            "Service {} failed its readiness check: {}",
            self.service_id, reason
        );
        self.update_if_starting("failed", Some(reason), Some(readiness))
            .await
    }
}

/// `health_metadata` with the readiness key set to `readiness`, or removed
/// when `None`. Sibling keys are preserved.
pub(crate) fn with_readiness_metadata(
    existing: Option<&serde_json::Value>,
    readiness: Option<&ServiceReadiness>,
) -> Option<serde_json::Value> {
    let mut map = match existing {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    match readiness.and_then(|r| serde_json::to_value(r).ok()) {
        Some(value) => {
            map.insert(READINESS_METADATA_KEY.to_string(), value);
        }
        None => {
            map.remove(READINESS_METADATA_KEY);
        }
    }
    (!map.is_empty()).then_some(serde_json::Value::Object(map))
}

/// Test doubles shared with engine tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Sink recording every report, for driver tests.
    #[derive(Default)]
    pub(crate) struct RecordingSink {
        pub starting: Mutex<Vec<ServiceReadiness>>,
        pub ready: Mutex<bool>,
        pub failed: Mutex<Option<ServiceReadiness>>,
        pub stop_after_starting_reports: Option<usize>,
    }

    #[async_trait]
    impl ReadinessSink for RecordingSink {
        async fn still_starting(&self) -> bool {
            match self.stop_after_starting_reports {
                Some(limit) => self.starting.lock().unwrap().len() < limit,
                None => true,
            }
        }
        async fn starting(&self, readiness: &ServiceReadiness) -> bool {
            self.starting.lock().unwrap().push(readiness.clone());
            true
        }
        async fn ready(&self) -> bool {
            *self.ready.lock().unwrap() = true;
            true
        }
        async fn failed(&self, readiness: &ServiceReadiness) -> bool {
            *self.failed.lock().unwrap() = Some(readiness.clone());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::RecordingSink;
    use super::*;

    fn policy() -> ReadinessPolicy {
        ReadinessPolicy {
            deadline: Duration::from_secs(180),
            poll_interval: Duration::from_secs(3),
            max_restarts: 3,
        }
    }

    fn not_ready(reason: &str) -> ReadinessObservation {
        ReadinessObservation {
            not_ready_reason: Some(reason.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_usable_answer_is_ready() {
        let verdict = evaluate(
            &policy(),
            Duration::from_secs(1),
            0,
            &ReadinessObservation::default(),
        );
        assert_eq!(verdict, ReadinessVerdict::Ready);
    }

    #[test]
    fn a_503_within_the_deadline_keeps_starting() {
        let verdict = evaluate(
            &policy(),
            Duration::from_secs(30),
            2,
            &not_ready("ListBuckets failed with HTTP 503"),
        );
        assert_eq!(verdict, ReadinessVerdict::Starting);
    }

    #[test]
    fn the_deadline_fails_with_the_last_probe_reason() {
        let verdict = evaluate(
            &policy(),
            Duration::from_secs(180),
            0,
            &not_ready("ListBuckets failed with HTTP 503: storage layer is not ready"),
        );
        let ReadinessVerdict::Failed(failure) = verdict else {
            panic!("expected failure, got {verdict:?}");
        };
        assert_eq!(failure.kind, InitializationFailureKind::Timeout);
        assert!(failure.reason.contains("within 180s"), "{}", failure.reason);
        assert!(failure.reason.contains("HTTP 503"), "{}", failure.reason);
        assert_eq!(failure.next_actions[0], ReadinessNextAction::ViewLogs);
    }

    #[test]
    fn a_restart_loop_fails_before_the_deadline() {
        let verdict = evaluate(
            &policy(),
            Duration::from_secs(40),
            3,
            &not_ready("ListBuckets got no response from RustFS"),
        );
        let ReadinessVerdict::Failed(failure) = verdict else {
            panic!("expected failure, got {verdict:?}");
        };
        assert_eq!(failure.kind, InitializationFailureKind::RestartLoop);
        assert!(
            failure.reason.contains("restarted 3 time(s)"),
            "{}",
            failure.reason
        );
        assert!(failure
            .next_actions
            .contains(&ReadinessNextAction::TryAnotherImage));
    }

    #[test]
    fn a_fatal_log_line_fails_at_once_and_is_the_excerpt() {
        let line = "Server runtime failed: store init failed: init retry budget exhausted";
        let observation = ReadinessObservation {
            not_ready_reason: Some("ListBuckets failed with HTTP 503".to_string()),
            restart_count: Some(1),
            fatal_log_lines: vec![line.to_string()],
        };
        let verdict = evaluate(&policy(), Duration::from_secs(5), 1, &observation);
        let ReadinessVerdict::Failed(failure) = verdict else {
            panic!("expected failure, got {verdict:?}");
        };
        assert_eq!(failure.kind, InitializationFailureKind::StoreInitFailed);
        assert_eq!(failure.log_excerpt, vec![line.to_string()]);
        assert!(failure
            .next_actions
            .contains(&ReadinessNextAction::RecreateWithFreshVolumes));
    }

    #[test]
    fn restart_counter_ignores_a_replaced_container() {
        let mut counter = RestartCounter::default();
        assert_eq!(counter.observe(Some(0)), 0);
        assert_eq!(counter.observe(Some(2)), 2);
        // Container recreated: its count starts over.
        assert_eq!(counter.observe(Some(0)), 2);
        assert_eq!(counter.observe(None), 2);
        assert_eq!(counter.observe(Some(1)), 3);
    }

    #[test]
    fn restart_counter_starts_from_the_first_observation() {
        // An old container that already restarted before the gate started
        // must not count those restarts against this start.
        let mut counter = RestartCounter::default();
        assert_eq!(counter.observe(Some(7)), 0);
        assert_eq!(counter.observe(Some(8)), 1);
    }

    #[test]
    fn readiness_metadata_round_trips_and_keeps_siblings() {
        let existing = serde_json::json!({ "postgres_wal": { "warnings": [] } });
        let readiness = ServiceReadiness::starting(Utc::now(), &policy());
        let merged = with_readiness_metadata(Some(&existing), Some(&readiness)).unwrap();
        assert!(merged.get("postgres_wal").is_some());
        assert_eq!(
            ServiceReadiness::from_health_metadata(Some(&merged)),
            Some(readiness)
        );
        let cleared = with_readiness_metadata(Some(&merged), None).unwrap();
        assert!(cleared.get(READINESS_METADATA_KEY).is_none());
        assert!(cleared.get("postgres_wal").is_some());
        assert_eq!(
            with_readiness_metadata(Some(&serde_json::json!({})), None),
            None
        );
    }

    #[test]
    fn started_at_is_iso_8601_utc() {
        let readiness = ServiceReadiness::starting(Utc::now(), &policy());
        assert!(
            readiness.started_at.ends_with('Z'),
            "{}",
            readiness.started_at
        );
        assert!(readiness.started_at_utc().is_some());
    }

    /// Target replaying scripted observations, then repeating the last one.
    struct ScriptedTarget {
        policy: ReadinessPolicy,
        script: Mutex<Vec<ReadinessObservation>>,
    }

    #[async_trait]
    impl ReadinessTarget for ScriptedTarget {
        fn policy(&self) -> ReadinessPolicy {
            self.policy
        }
        async fn observe(&self) -> ReadinessObservation {
            let mut script = self.script.lock().unwrap();
            if script.len() > 1 {
                script.remove(0)
            } else {
                script[0].clone()
            }
        }
        async fn log_excerpt(&self) -> Vec<String> {
            vec!["last log line".to_string()]
        }
    }

    fn fast_policy() -> ReadinessPolicy {
        ReadinessPolicy {
            deadline: Duration::from_millis(300),
            poll_interval: Duration::from_millis(10),
            max_restarts: 3,
        }
    }

    #[tokio::test]
    async fn driver_reports_each_restart_and_a_restart_loop_failure() {
        let observe = |restarts| ReadinessObservation {
            not_ready_reason: Some("ListBuckets got no response from RustFS".to_string()),
            restart_count: Some(restarts),
            fatal_log_lines: Vec::new(),
        };
        let target = ScriptedTarget {
            policy: fast_policy(),
            script: Mutex::new(vec![observe(0), observe(1), observe(2), observe(3)]),
        };
        let sink = RecordingSink::default();
        let prior = ServiceReadiness::starting(Utc::now(), &target.policy);

        let outcome = drive_readiness(&target, &sink, prior, || false).await;

        let ReadinessOutcome::Failed(readiness) = outcome else {
            panic!("expected failure, got {outcome:?}");
        };
        let failure = readiness.failure.unwrap();
        assert_eq!(failure.kind, InitializationFailureKind::RestartLoop);
        assert_eq!(failure.log_excerpt, vec!["last log line".to_string()]);
        assert_eq!(readiness.restart_count, 3);
        let reported: Vec<i64> = sink
            .starting
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.restart_count)
            .collect();
        assert_eq!(reported, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn driver_resumes_the_prior_start_time_and_restart_count() {
        let target = ScriptedTarget {
            policy: fast_policy(),
            script: Mutex::new(vec![not_ready("HTTP 503")]),
        };
        let sink = RecordingSink::default();
        // Started long ago: the deadline has already passed.
        let mut prior =
            ServiceReadiness::starting(Utc::now() - chrono::Duration::seconds(10), &target.policy);
        prior.restart_count = 2;

        let outcome = drive_readiness(&target, &sink, prior, || false).await;

        let ReadinessOutcome::Failed(readiness) = outcome else {
            panic!("expected failure, got {outcome:?}");
        };
        assert_eq!(readiness.restart_count, 2);
        assert_eq!(
            readiness.failure.unwrap().kind,
            InitializationFailureKind::Timeout
        );
        assert!(sink.starting.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn driver_stops_without_a_verdict_when_the_service_leaves_starting() {
        let target = ScriptedTarget {
            policy: fast_policy(),
            script: Mutex::new(vec![not_ready("HTTP 503")]),
        };
        let sink = RecordingSink {
            stop_after_starting_reports: Some(1),
            ..Default::default()
        };
        let prior = ServiceReadiness::starting(Utc::now(), &target.policy);

        let outcome = drive_readiness(&target, &sink, prior, || false).await;

        assert_eq!(outcome, ReadinessOutcome::Abandoned);
        assert!(!*sink.ready.lock().unwrap());
        assert!(sink.failed.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn driver_stops_when_superseded_by_a_newer_watch() {
        let target = ScriptedTarget {
            policy: fast_policy(),
            script: Mutex::new(vec![not_ready("HTTP 503")]),
        };
        let sink = RecordingSink::default();
        let prior = ServiceReadiness::starting(Utc::now(), &target.policy);

        let outcome = drive_readiness(&target, &sink, prior, || true).await;

        assert_eq!(outcome, ReadinessOutcome::Abandoned);
        assert!(sink.starting.lock().unwrap().is_empty());
    }

    #[test]
    fn a_fresh_start_supersedes_a_running_watch_and_a_resume_does_not() {
        // Ids far from anything another test would use.
        let first = WatchRegistration::claim(-9001, true).unwrap();
        assert!(first.is_current());
        assert!(WatchRegistration::claim(-9001, false).is_none());

        let second = WatchRegistration::claim(-9001, true).unwrap();
        assert!(!first.is_current());
        assert!(second.is_current());

        // The superseded watch ending must not release the new one.
        drop(first);
        assert!(second.is_current());
        drop(second);
        assert!(WatchRegistration::claim(-9001, false).is_some());
    }
}
