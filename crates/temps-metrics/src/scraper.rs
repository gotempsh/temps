// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Background metrics scraper.
//!
//! [`MetricsScraper`] runs in a background task and periodically collects
//! metrics from all `external_services` rows where `metrics_enabled = true`
//! and `status = 'running'`.
//!
//! # Architecture
//!
//! Each scrape cycle:
//! 1. Reads `scrape_interval_secs` from [`ConfigService`] (re-read every cycle
//!    so live settings changes take effect without a restart).
//! 2. Queries `external_services` for enabled services.
//! 3. Spawns one `tokio::task` per service (parallel, non-blocking).
//!    Each task has a 5-second timeout (configurable via
//!    [`CollectorConfig::timeout`]).
//! 4. Collects the raw cumulative metric values from the appropriate
//!    engine-specific [`Collector`] implementation.
//! 5. Applies delta computation for [`MetricKind::Counter`] metrics using the
//!    in-memory `last_scalar_values` map.  Counter resets are detected when
//!    the current raw value is less than the previous one — in that case the
//!    raw value is used as the delta directly.
//! 6. Calls [`MetricsStore::write_batch`].  A write failure is logged as a
//!    warning but never propagates — the scraper loop continues regardless.
//!
//! # Counter baseline
//!
//! `last_scalar_values` is in-memory only.  On restart (or first scrape of a
//! newly-enabled service) the baseline is unknown: we skip writing the delta
//! for that cycle rather than emitting a spurious spike.  This is the
//! documented behaviour from the `MetricPoint::value` safety contract.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::sync::Mutex as StdMutex;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};

use temps_config::ConfigService;
use temps_core::EncryptionService;
use temps_entities::external_services;

use crate::collector::mongodb::MongoCollector;
use crate::collector::postgres::PostgresCollector;
use crate::collector::redis::RedisCollector;
use crate::collector::s3::S3Collector;
use crate::collector::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint, MetricsStore, SourceKind};

/// Opens a connection to the current primary of an HA (cluster-topology)
/// PostgreSQL service.
///
/// A cluster has no single host/port in its stored config: the primary is
/// whichever data member pg_auto_failover elected, possibly on another node,
/// and it changes on failover. Resolving it needs the cluster's monitor and
/// member records, which live in the providers layer, so the scraper takes
/// this as an injected capability rather than depending on that crate.
#[async_trait::async_trait]
pub trait ClusterPrimaryConnector: Send + Sync {
    /// Connect to `service_id`'s current primary. The returned client's
    /// connection driver must already be running and stop when the client is
    /// dropped.
    async fn connect_cluster_primary(
        &self,
        service_id: i32,
    ) -> Result<ClusterPrimaryConnection, MetricsError>;
}

/// A connection to an HA cluster's current primary.
pub struct ClusterPrimaryConnection {
    pub client: tokio_postgres::Client,
    /// Stable identity of the member that is primary (its container name).
    /// Cumulative counters are per server: when this changes, the previous
    /// primary's readings are no baseline for the new one's.
    pub member: String,
}

/// Last primary seen per cluster service, keyed by service ID.
type ClusterPrimaries = Arc<StdMutex<HashMap<i32, String>>>;

/// Minimum scrape interval enforced at runtime regardless of configuration.
const MIN_INTERVAL_SECS: u64 = 10;

/// Timeout given to each per-service collector task.
const COLLECTOR_TIMEOUT_SECS: u64 = 5;

/// Maximum number of collector tasks that may run concurrently.
/// Caps the thundering-herd on DB pool connections when many services are
/// enabled simultaneously.  Choose a value ≤ half the Sea-ORM pool size.
const MAX_CONCURRENT_COLLECTORS: usize = 20;

/// Last raw counter value per `(source_id, metric_name, labels_key)`, used to
/// compute scrape-to-scrape deltas. The `labels_key` component (see
/// [`labels_key`]) keeps distinct label-series for the same metric name
/// independent — Postgres emits e.g. `pg.commits_total` once per `datname`
/// plus one unlabelled instance-wide aggregate; without it they would share a
/// baseline and produce garbage deltas.
type CounterBaselines = Arc<RwLock<HashMap<(i32, String, String), f64>>>;

/// Background metrics scraper.
///
/// Spawn via [`MetricsScraper::start`].  The struct is cheaply cloneable
/// through its inner `Arc`-wrapped state.
pub struct MetricsScraper {
    db: Arc<DatabaseConnection>,
    store: Arc<dyn MetricsStore>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
    /// Last raw counter values keyed by `(source_id, metric_name, labels_key)`.
    /// See [`CounterBaselines`].
    last_scalar_values: CounterBaselines,
    /// Tracks which service IDs currently have an in-flight scrape task.
    /// Prevents concurrent scrapes for the same service (double-delta corruption).
    /// Services currently being scraped. Uses std::sync::Mutex (not tokio) so
    /// a Drop guard can release the slot even if the scrape task panics.
    in_flight: Arc<StdMutex<HashSet<i32>>>,
    /// Reaches the primary of HA PostgreSQL clusters. Without it, cluster
    /// services are skipped (with a warning) rather than scraped at an
    /// address no member listens on.
    cluster_primary: Option<Arc<dyn ClusterPrimaryConnector>>,
    /// Which member supplied each cluster's counter baselines.
    cluster_primaries: ClusterPrimaries,
}

impl MetricsScraper {
    /// Create a new scraper.
    ///
    /// - `db`                — shared database connection used to query
    ///   `external_services`.
    /// - `store`             — metrics storage backend that receives the
    ///   collected points.
    /// - `config_service`    — used to read `monitoring.scrape_interval_secs`
    ///   before each cycle.
    /// - `encryption_service`— used to decrypt the service config blobs stored
    ///   in `external_services.config`.
    pub fn new(
        db: Arc<DatabaseConnection>,
        store: Arc<dyn MetricsStore>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
    ) -> Self {
        Self {
            db,
            store,
            config_service,
            encryption_service,
            last_scalar_values: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(StdMutex::new(HashSet::new())),
            cluster_primary: None,
            cluster_primaries: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Scrape HA PostgreSQL clusters through `connector`, which resolves and
    /// dials each cluster's current primary.
    pub fn with_cluster_primary_connector(
        mut self,
        connector: Arc<dyn ClusterPrimaryConnector>,
    ) -> Self {
        self.cluster_primary = Some(connector);
        self
    }

    /// Run the scrape loop forever.  Spawn this on a background task.
    pub async fn start(self: Arc<Self>) {
        info!("MetricsScraper started");

        loop {
            // Re-read interval each cycle — live settings changes take effect
            // without a binary restart.
            let interval_secs = match self.config_service.get_settings().await {
                Ok(settings) => settings
                    .monitoring
                    .scrape_interval_secs
                    .max(MIN_INTERVAL_SECS),
                Err(e) => {
                    warn!("MetricsScraper: failed to read settings, using default 30s: {e}");
                    30
                }
            };

            if let Err(e) = self.run_cycle().await {
                error!("MetricsScraper: cycle failed: {e}");
            }

            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    }

    /// Run one complete scrape cycle over all enabled services.
    async fn run_cycle(&self) -> Result<(), String> {
        // Query external_services with metrics_enabled = true and status = 'running'
        let services = external_services::Entity::find()
            .filter(external_services::Column::MetricsEnabled.eq(true))
            .filter(external_services::Column::Status.eq("running"))
            .all(self.db.as_ref())
            .await
            .map_err(|e| format!("Failed to query external_services: {e}"))?;

        if services.is_empty() {
            debug!("MetricsScraper: no enabled services to scrape");
            // TODO(metrics): Issue C (Correctness Review) — clearing last_scalar_values
            // unconditionally when all services are temporarily disabled (e.g. during
            // a rolling deploy) resets all counter baselines simultaneously.  The first
            // scrape cycle after services come back up will skip all counter deltas,
            // producing a brief gap on counter graphs for every service at once.
            // Consider only clearing entries for services that have been absent for
            // longer than one retention window, or persisting baselines to a DB table.
            self.last_scalar_values.write().await.clear();
            return Ok(());
        }

        debug!("MetricsScraper: scraping {} service(s)", services.len());

        // Collect the active service ID set for stale-entry pruning after the cycle.
        let active_ids: HashSet<i32> = services.iter().map(|s| s.id).collect();

        // Semaphore caps concurrent collector tasks to avoid thundering-herd on
        // the DB pool and TimescaleDB chunk locks.
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_COLLECTORS));

        // Each task returns its collected+delta-applied points.
        let mut join_set: tokio::task::JoinSet<Vec<MetricPoint>> = tokio::task::JoinSet::new();

        for service in services {
            // Per-service deduplication: skip if a task for this service is
            // still running from a previous cycle (prevents double-delta corruption).
            {
                let mut in_flight = self.in_flight.lock().unwrap();
                if !in_flight.insert(service.id) {
                    warn!(
                        service_id = service.id,
                        service_type = service.service_type,
                        "MetricsScraper: skipping scrape — previous cycle still in flight"
                    );
                    continue;
                }
            }

            let encryption = Arc::clone(&self.encryption_service);
            let cluster_primary = self.cluster_primary.clone();
            let cluster_primaries = Arc::clone(&self.cluster_primaries);
            let last_values = Arc::clone(&self.last_scalar_values);
            let in_flight = Arc::clone(&self.in_flight);
            let permit = Arc::clone(&sem)
                .acquire_owned()
                .await
                .expect("semaphore should never be closed");

            join_set.spawn(async move {
                // Permit is held for the lifetime of this task.
                let _permit = permit;
                let service_id = service.id;
                let service_type = service.service_type.clone();

                // Drop guard: removes service_id from in_flight when this
                // scope exits — including on panic. Prevents slot leaks that
                // would silently starve future scrapes for this service.
                struct InFlightGuard {
                    id: i32,
                    set: Arc<StdMutex<HashSet<i32>>>,
                }
                impl Drop for InFlightGuard {
                    fn drop(&mut self) {
                        if let Ok(mut g) = self.set.lock() {
                            g.remove(&self.id);
                        }
                    }
                }
                let _guard = InFlightGuard {
                    id: service_id,
                    set: Arc::clone(&in_flight),
                };

                let result = async {
                    if service.topology == "cluster" {
                        let Some((member, raw_points)) = scrape_cluster(
                            &service,
                            cluster_primary.as_deref(),
                            Duration::from_secs(COLLECTOR_TIMEOUT_SECS),
                        )
                        .await
                        else {
                            return Err(());
                        };
                        // After a failover the new primary's counters are a
                        // different server's: start a fresh baseline rather
                        // than diffing them against the old primary's.
                        if primary_changed(&cluster_primaries, service_id, &member) {
                            reset_baselines(&last_values, service_id).await;
                        }
                        if raw_points.is_empty() {
                            return Err(());
                        }
                        return Ok(apply_delta(&last_values, service_id, raw_points).await);
                    }

                    // Build a connection string from the encrypted service config.
                    let connection_string = build_connection_string(&service, &encryption)
                        .map_err(|e| {
                            warn!(
                                service_id,
                                service_type, "MetricsScraper: cannot build connection string: {e}"
                            );
                        })?;

                    let config =
                        CollectorConfig::new(service_id, SourceKind::Database, connection_string)
                            .with_timeout(Duration::from_secs(COLLECTOR_TIMEOUT_SECS))
                            .with_node_id_opt(service.node_id);

                    // Select collector by service_type.
                    let raw_points: Vec<MetricPoint> = match service_type.to_lowercase().as_str() {
                        "postgres" => collect_with_timeout(PostgresCollector::new(), &config).await,
                        "redis" => collect_with_timeout(RedisCollector::new(), &config).await,
                        "mongodb" => collect_with_timeout(MongoCollector::new(), &config).await,
                        // RustFS is S3-compatible: reuse the same collector.
                        "s3" | "rustfs" => collect_with_timeout(S3Collector::new(), &config).await,
                        _ => {
                            debug!(
                                service_id,
                                service_type,
                                "MetricsScraper: no collector for service type, skipping"
                            );
                            vec![]
                        }
                    };

                    if raw_points.is_empty() {
                        return Err(());
                    }

                    // Apply delta computation for counter metrics.
                    let points = apply_delta(&last_values, service_id, raw_points).await;
                    Ok(points)
                }
                .await;

                result.unwrap_or_default()
            });
        }

        // Collect all metric points from all tasks into a single batch.
        let mut all_points: Vec<MetricPoint> = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(points) => all_points.extend(points),
                Err(e) => warn!("MetricsScraper: a scrape task panicked: {e}"),
            }
        }

        // Single write_batch call for all services — one large INSERT is orders
        // of magnitude faster than N small ones against TimescaleDB.
        if !all_points.is_empty() {
            if let Err(e) = self.store.write_batch(all_points).await {
                warn!("MetricsScraper: write_batch failed (non-fatal): {e}");
            }
        }

        // Prune stale entries from last_scalar_values for services that no longer
        // exist or have metrics disabled.  Avoids unbounded growth with service churn.
        {
            let mut guard = self.last_scalar_values.write().await;
            guard.retain(|(sid, _, _), _| active_ids.contains(sid));
        }
        if let Ok(mut primaries) = self.cluster_primaries.lock() {
            primaries.retain(|sid, _| active_ids.contains(sid));
        }

        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Scrape an HA (cluster-topology) service at its current primary.
///
/// The stored `host`/`port` of a cluster describe no listening member, so the
/// standalone connection-string path can never reach one. Only PostgreSQL
/// clusters exist today; any other cluster engine is skipped.
///
/// Returns the member that answered along with its points, or `None` when no
/// primary could be scraped.
async fn scrape_cluster(
    service: &external_services::Model,
    connector: Option<&dyn ClusterPrimaryConnector>,
    timeout: Duration,
) -> Option<(String, Vec<MetricPoint>)> {
    let service_id = service.id;
    if !service.service_type.eq_ignore_ascii_case("postgres") {
        debug!(
            service_id,
            service_type = service.service_type,
            "MetricsScraper: no cluster collector for service type, skipping"
        );
        return None;
    }
    let Some(connector) = connector else {
        warn!(
            service_id,
            "MetricsScraper: cannot scrape HA cluster: no cluster primary connector is registered"
        );
        return None;
    };

    let primary = match tokio::time::timeout(timeout, connector.connect_cluster_primary(service_id))
        .await
    {
        Ok(Ok(primary)) => primary,
        Ok(Err(e)) => {
            warn!(service_id, error = %e, "MetricsScraper: cluster primary unavailable; skipping scrape");
            return None;
        }
        Err(_elapsed) => {
            warn!(
                service_id,
                timeout_secs = timeout.as_secs(),
                "MetricsScraper: connecting to the cluster primary timed out; skipping scrape"
            );
            return None;
        }
    };

    let config = CollectorConfig::new(service_id, SourceKind::Database, String::new())
        .with_timeout(timeout)
        .with_node_id_opt(service.node_id);
    let points = PostgresCollector::new()
        .collect_from_client(&primary.client, &config)
        .await;
    Some((primary.member, points))
}

/// Record `member` as `service_id`'s primary; `true` when it differs from the
/// member that supplied the service's current counter baselines (including
/// the first time a primary is seen).
fn primary_changed(primaries: &ClusterPrimaries, service_id: i32, member: &str) -> bool {
    let Ok(mut primaries) = primaries.lock() else {
        // A poisoned lock: treat as a change, which only costs one skipped
        // counter interval — never a false spike.
        return true;
    };
    match primaries.get(&service_id) {
        Some(previous) if previous == member => false,
        _ => {
            primaries.insert(service_id, member.to_string());
            true
        }
    }
}

/// Drop every counter baseline of `service_id`, so its next scrape records
/// fresh baselines instead of deltas.
async fn reset_baselines(last_values: &CounterBaselines, service_id: i32) {
    last_values
        .write()
        .await
        .retain(|(sid, _, _), _| *sid != service_id);
}

/// Collect metrics from a concrete [`Collector`] implementation with a
/// per-task timeout.  On timeout or error: log warning, return empty vec.
async fn collect_with_timeout<C: Collector>(
    collector: C,
    config: &CollectorConfig,
) -> Vec<MetricPoint> {
    match tokio::time::timeout(
        Duration::from_secs(COLLECTOR_TIMEOUT_SECS),
        collector.collect(config),
    )
    .await
    {
        Ok(Ok(pts)) => pts,
        Ok(Err(e)) => {
            warn!(
                source_id = config.source_id,
                engine = collector.engine(),
                "MetricsScraper: collector error: {e}"
            );
            vec![]
        }
        Err(_elapsed) => {
            warn!(
                source_id = config.source_id,
                engine = collector.engine(),
                "MetricsScraper: collector timed out after {}s",
                COLLECTOR_TIMEOUT_SECS
            );
            vec![]
        }
    }
}

/// Canonical, order-independent key for a point's label set.
///
/// `HashMap` iteration order is non-deterministic, so two points with the same
/// labels could otherwise produce different key strings on different scrapes
/// and lose their delta baseline. Sorting the pairs makes the key stable. An
/// empty label set yields `""`, distinguishing instance-wide aggregates from
/// per-label series of the same metric name.
fn labels_key(labels: &HashMap<String, String>) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(&String, &String)> = labels.iter().collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Compute counter deltas and update `last_scalar_values`.
///
/// For [`MetricKind::Counter`] points:
/// - If no baseline exists (first scrape): skip writing (return nothing for
///   that point) and record the raw value as the new baseline.
/// - If a reset is detected (`current < previous`): use `current` as the
///   delta directly and update the baseline.
/// - Otherwise: emit `current - previous` as the delta.
///
/// Gauge points pass through unchanged.
async fn apply_delta(
    last_values: &CounterBaselines,
    source_id: i32,
    raw_points: Vec<MetricPoint>,
) -> Vec<MetricPoint> {
    let mut out = Vec::with_capacity(raw_points.len());
    let mut guard = last_values.write().await;

    for mut pt in raw_points {
        if pt.kind == MetricKind::Counter {
            let key = (source_id, pt.name.clone(), labels_key(&pt.labels));
            let raw = pt.value;

            match guard.get(&key).copied() {
                None => {
                    // First scrape for this metric — record baseline, skip write.
                    guard.insert(key, raw);
                    continue;
                }
                Some(prev) => {
                    let delta = if raw < prev {
                        // Counter reset (service restarted).  Use raw value as
                        // delta to avoid a negative or huge spike.
                        raw
                    } else {
                        raw - prev
                    };
                    guard.insert(key, raw);
                    pt.value = delta;
                }
            }
        }
        out.push(pt);
    }

    out
}

/// Build a connection string for the given external service using its
/// encrypted config blob.
///
/// Parses the decrypted JSON parameters and produces an appropriate URI or
/// DSN for the service type.  Returns an error if the config cannot be
/// decrypted or does not contain the required parameters.
fn build_connection_string(
    service: &external_services::Model,
    encryption: &EncryptionService,
) -> Result<String, String> {
    let encrypted_config = service
        .config
        .as_deref()
        .ok_or_else(|| format!("Service {} has no config", service.id))?;

    let config_json = encryption
        .decrypt_string(encrypted_config)
        .map_err(|e| format!("Failed to decrypt config for service {}: {e}", service.id))?;

    let params: HashMap<String, serde_json::Value> = serde_json::from_str(&config_json)
        .map_err(|e| format!("Failed to parse config for service {}: {e}", service.id))?;

    let get_str = |key: &str| -> String {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let host = {
        let h = get_str("host");
        if h.is_empty() {
            "localhost".to_string()
        } else {
            h
        }
    };

    match service.service_type.to_lowercase().as_str() {
        "postgres" => {
            let port = get_str("port");
            let port = if port.is_empty() {
                "5432".to_string()
            } else {
                port
            };
            let username = get_str("username");
            let username = if username.is_empty() {
                "postgres".to_string()
            } else {
                username
            };
            let password = get_str("password");
            let database = get_str("database");
            let database = if database.is_empty() {
                "postgres".to_string()
            } else {
                database
            };

            if password.is_empty() {
                Ok(format!(
                    "postgresql://{}@{}:{}/{}",
                    username, host, port, database
                ))
            } else {
                Ok(format!(
                    "postgresql://{}:{}@{}:{}/{}",
                    urlencoded(&username),
                    urlencoded(&password),
                    host,
                    port,
                    database
                ))
            }
        }
        "redis" => {
            let port = get_str("port");
            let port = if port.is_empty() {
                "6379".to_string()
            } else {
                port
            };
            let password = get_str("password");

            if password.is_empty() {
                Ok(format!("redis://{}:{}", host, port))
            } else {
                Ok(format!(
                    "redis://:{}@{}:{}",
                    urlencoded(&password),
                    host,
                    port
                ))
            }
        }
        "mongodb" => {
            let port = get_str("port");
            let port = if port.is_empty() {
                "27017".to_string()
            } else {
                port
            };
            let username = get_str("username");
            let password = get_str("password");

            if username.is_empty() {
                // No authentication configured — connect without credentials.
                Ok(format!("mongodb://{}:{}/", host, port))
            } else if password.is_empty() {
                // MongoDB authentication requires a password when a username is
                // provided.  A connection string like `mongodb://user:@host/`
                // will silently fail auth on most server versions.
                Err(format!(
                    "Service {}: MongoDB username '{}' requires a non-empty password",
                    service.id, username
                ))
            } else {
                // `authSource=admin` is required because Temps provisions the
                // service user as a root user in the `admin` database. Without
                // it the driver derives authSource from the path database
                // (e.g. `mydatabase`), and SCRAM fails with "Authentication
                // failed." because the user does not exist there.
                //
                // Mirrors the provider's own connection strings in
                // `temps-providers/src/externalsvc/mongodb.rs` (~/?authSource=admin&directConnection=true).
                Ok(format!(
                    "mongodb://{}:{}@{}:{}/?authSource=admin",
                    urlencoded(&username),
                    urlencoded(&password),
                    host,
                    port,
                ))
            }
        }
        // S3 (MinIO) and RustFS both use S3-compatible APIs.
        // The S3Collector expects: region|access_key|secret_key[|endpoint_url]
        // For locally-hosted services the endpoint is http://host:port.
        "s3" | "rustfs" => {
            let port = get_str("port");
            let access_key = get_str("access_key");
            let secret_key = get_str("secret_key");
            let region = {
                let r = get_str("region");
                if r.is_empty() {
                    "us-east-1".to_string()
                } else {
                    r
                }
            };

            if access_key.is_empty() || secret_key.is_empty() {
                return Err(format!(
                    "Service {}: s3/rustfs config missing access_key or secret_key",
                    service.id
                ));
            }

            // Build a MinIO-style endpoint URL when host is not the public AWS S3
            // hostname — i.e. whenever the service runs as a local container.
            if !host.is_empty() && !port.is_empty() {
                Ok(format!(
                    "{}|{}|{}|http://{}:{}",
                    region, access_key, secret_key, host, port
                ))
            } else {
                // Pure AWS S3: no endpoint override, let the SDK use the default.
                Ok(format!("{}|{}|{}", region, access_key, secret_key))
            }
        }

        other => Err(format!(
            "MetricsScraper: no connection string builder for service type '{other}'"
        )),
    }
}

/// Percent-encode a string for use in a URI user-info component (username or
/// password).
///
/// Encodes all characters that are either reserved in the URI user-info
/// position or could break URI parsing if left bare.  Crucially, `%` itself
/// is encoded first so that subsequent passes do not double-encode it.
fn urlencoded(s: &str) -> String {
    // Each byte is encoded independently so multi-byte UTF-8 sequences are
    // handled correctly (each byte gets its own %XX escape).
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.as_bytes() {
        match byte {
            b'%' => out.push_str("%25"),
            b':' => out.push_str("%3A"),
            b'@' => out.push_str("%40"),
            b'/' => out.push_str("%2F"),
            b'#' => out.push_str("%23"),
            b'?' => out.push_str("%3F"),
            b' ' => out.push_str("%20"),
            b'+' => out.push_str("%2B"),
            b'[' => out.push_str("%5B"),
            b']' => out.push_str("%5D"),
            b => out.push(*b as char),
        }
    }
    out
}

/// Extension trait to set `node_id` from an `Option<i32>`.
trait CollectorConfigExt {
    fn with_node_id_opt(self, node_id: Option<i32>) -> Self;
}

impl CollectorConfigExt for CollectorConfig {
    fn with_node_id_opt(self, node_id: Option<i32>) -> Self {
        match node_id {
            Some(id) => self.with_node_id(id),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── scrape_cluster ─────────────────────────────────────────────────────

    fn cluster_service(id: i32, service_type: &str) -> external_services::Model {
        external_services::Model {
            id,
            name: "ha-pg".to_string(),
            service_type: service_type.to_string(),
            version: Some("18".to_string()),
            status: "running".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: Some("ha-pg".to_string()),
            config: None,
            node_id: None,
            topology: "cluster".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: true,
            default_backup_provisioned: false,
            ai_data_access: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
        }
    }

    /// Records which services it was asked to reach, and fails or hangs.
    struct FakeConnector {
        calls: StdMutex<Vec<i32>>,
        hang: bool,
    }

    #[async_trait::async_trait]
    impl ClusterPrimaryConnector for FakeConnector {
        async fn connect_cluster_primary(
            &self,
            service_id: i32,
        ) -> Result<ClusterPrimaryConnection, MetricsError> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(service_id);
            }
            if self.hang {
                std::future::pending::<()>().await;
            }
            Err(MetricsError::CollectorConnectionFailed {
                source_id: service_id,
                engine: "postgres".to_string(),
                reason: "no healthy primary".to_string(),
            })
        }
    }

    fn connector(hang: bool) -> FakeConnector {
        FakeConnector {
            calls: StdMutex::new(Vec::new()),
            hang,
        }
    }

    fn calls(connector: &FakeConnector) -> Vec<i32> {
        connector
            .calls
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    /// Regression: HA cluster metrics were always empty because the scraper
    /// dialled the cluster's stored host/port, where no member listens. A
    /// cluster must be reached through the primary connector instead.
    #[tokio::test]
    async fn postgres_cluster_is_scraped_through_its_primary_connector() {
        let fake = connector(false);
        let points = scrape_cluster(
            &cluster_service(41, "postgres"),
            Some(&fake),
            Duration::from_secs(1),
        )
        .await;

        assert_eq!(
            calls(&fake),
            vec![41],
            "the connector must be asked for service 41"
        );
        assert!(
            points.is_none(),
            "an unavailable primary yields nothing to write, not an error"
        );
    }

    #[tokio::test]
    async fn cluster_primary_connect_is_bounded_by_the_timeout() {
        let fake = connector(true);
        let started = std::time::Instant::now();
        let points = scrape_cluster(
            &cluster_service(42, "postgres"),
            Some(&fake),
            Duration::from_millis(50),
        )
        .await;

        assert!(points.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a hung connector must not stall the scrape cycle"
        );
    }

    #[tokio::test]
    async fn cluster_without_connector_or_postgres_engine_is_skipped() {
        let points = scrape_cluster(
            &cluster_service(43, "postgres"),
            None,
            Duration::from_secs(1),
        )
        .await;
        assert!(points.is_none());

        let fake = connector(false);
        let points = scrape_cluster(
            &cluster_service(44, "redis"),
            Some(&fake),
            Duration::from_secs(1),
        )
        .await;
        assert!(points.is_none());
        assert!(
            calls(&fake).is_empty(),
            "only PostgreSQL clusters are dialled"
        );
    }

    fn commits(value: f64) -> MetricPoint {
        MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 7,
            name: "pg.commits_total".to_string(),
            value,
            kind: MetricKind::Counter,
            engine: Some("postgres".to_string()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        }
    }

    /// Regression: after a failover the new primary's independent counters
    /// were diffed against the old primary's baseline. 100,000 commits on the
    /// old primary then 20,000 on the new one recorded 20,000 commits in one
    /// interval (counter-reset handling) that never happened.
    #[tokio::test]
    async fn failover_starts_a_fresh_counter_baseline() {
        let baselines: CounterBaselines = Arc::new(RwLock::new(HashMap::new()));
        let primaries: ClusterPrimaries = Arc::new(StdMutex::new(HashMap::new()));

        // Two scrapes on the first primary: baseline, then a real delta.
        assert!(primary_changed(&primaries, 7, "ha-pg-1"));
        reset_baselines(&baselines, 7).await;
        assert!(apply_delta(&baselines, 7, vec![commits(100_000.0)])
            .await
            .is_empty());
        assert!(!primary_changed(&primaries, 7, "ha-pg-1"));
        let delta = apply_delta(&baselines, 7, vec![commits(100_050.0)]).await;
        assert_eq!(delta.first().map(|p| p.value), Some(50.0));

        // Failover: the new primary's counters start a fresh baseline.
        assert!(primary_changed(&primaries, 7, "ha-pg-2"));
        reset_baselines(&baselines, 7).await;
        let after_failover = apply_delta(&baselines, 7, vec![commits(20_000.0)]).await;
        assert!(
            after_failover.is_empty(),
            "no delta may be recorded across a change of primary, got {:?}",
            after_failover.iter().map(|p| p.value).collect::<Vec<_>>()
        );
        let next = apply_delta(&baselines, 7, vec![commits(20_010.0)]).await;
        assert_eq!(next.first().map(|p| p.value), Some(10.0));
    }

    #[tokio::test]
    async fn resetting_one_cluster_keeps_other_services_baselines() {
        let baselines: CounterBaselines = Arc::new(RwLock::new(HashMap::new()));
        apply_delta(&baselines, 7, vec![commits(1.0)]).await;
        let mut other = commits(5.0);
        other.source_id = 8;
        apply_delta(&baselines, 8, vec![other]).await;

        reset_baselines(&baselines, 7).await;

        let keys: Vec<i32> = baselines
            .read()
            .await
            .keys()
            .map(|(sid, _, _)| *sid)
            .collect();
        assert_eq!(keys, vec![8]);
    }
    // ── urlencoded ─────────────────────────────────────────────────────────

    #[test]
    fn urlencoded_passthrough_plain() {
        assert_eq!(urlencoded("mypassword"), "mypassword");
    }

    #[test]
    fn urlencoded_encodes_special_chars() {
        assert_eq!(urlencoded("p@ss:word"), "p%40ss%3Aword");
    }

    #[test]
    fn urlencoded_encodes_percent_sign() {
        // A literal % must be encoded as %25 to prevent double-encoding.
        assert_eq!(urlencoded("p%40ss"), "p%2540ss");
    }

    #[test]
    fn urlencoded_encodes_plus_and_brackets() {
        assert_eq!(urlencoded("a+b[c]"), "a%2Bb%5Bc%5D");
    }

    // ── build_connection_string ────────────────────────────────────────────

    // We can't test build_connection_string directly with a real EncryptionService
    // in unit tests because it requires a real encryption key. Instead, we test
    // the urlencoded helper and apply_delta logic directly.

    #[test]
    fn urlencoded_slash() {
        assert_eq!(urlencoded("a/b"), "a%2Fb");
    }

    #[tokio::test]
    async fn apply_delta_skips_first_scrape() {
        let last = Arc::new(RwLock::new(HashMap::new()));
        let pt = MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "pg.blks_read_total".into(),
            value: 1000.0,
            kind: MetricKind::Counter,
            engine: Some("postgres".into()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        let out = apply_delta(&last, 1, vec![pt]).await;
        // First scrape: should be skipped
        assert!(
            out.is_empty(),
            "Expected no output on first counter scrape (no baseline)"
        );

        let guard = last.read().await;
        assert_eq!(
            *guard
                .get(&(1, "pg.blks_read_total".to_string(), String::new()))
                .unwrap(),
            1000.0
        );
    }

    #[tokio::test]
    async fn apply_delta_computes_delta_on_second_scrape() {
        let last = Arc::new(RwLock::new(HashMap::new()));

        let make_pt = |val: f64| MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "pg.blks_read_total".into(),
            value: val,
            kind: MetricKind::Counter,
            engine: Some("postgres".into()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        // First scrape: sets baseline
        let _ = apply_delta(&last, 1, vec![make_pt(1000.0)]).await;

        // Second scrape: compute delta
        let out = apply_delta(&last, 1, vec![make_pt(1250.0)]).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, 250.0);
    }

    #[tokio::test]
    async fn apply_delta_detects_counter_reset() {
        let last = Arc::new(RwLock::new(HashMap::new()));

        let make_pt = |val: f64| MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "redis.evicted_keys_total".into(),
            value: val,
            kind: MetricKind::Counter,
            engine: Some("redis".into()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        // Baseline
        let _ = apply_delta(&last, 1, vec![make_pt(5000.0)]).await;

        // Counter reset: service restarted, value now 50
        let out = apply_delta(&last, 1, vec![make_pt(50.0)]).await;
        assert_eq!(out.len(), 1);
        // Reset detected: use raw value 50 as delta, not 50 - 5000 = -4950
        assert_eq!(out[0].value, 50.0);
    }

    #[tokio::test]
    async fn apply_delta_passes_gauges_through() {
        let last = Arc::new(RwLock::new(HashMap::new()));

        let pt = MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 1,
            name: "pg.connections_active".into(),
            value: 42.0,
            kind: MetricKind::Gauge,
            engine: Some("postgres".into()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        // Gauges always pass through on first and subsequent scrapes
        let out = apply_delta(&last, 1, vec![pt]).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, 42.0);
    }

    #[tokio::test]
    async fn apply_delta_no_baseline_pollution_across_sources() {
        // Counter for source_id=1 and source_id=2 should be tracked separately
        let last = Arc::new(RwLock::new(HashMap::new()));

        let make_pt = |source_id: i32, val: f64| MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id,
            name: "pg.blks_read_total".into(),
            value: val,
            kind: MetricKind::Counter,
            engine: Some("postgres".into()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        // Set baseline for source 1
        let _ = apply_delta(&last, 1, vec![make_pt(1, 100.0)]).await;
        // Set baseline for source 2
        let _ = apply_delta(&last, 2, vec![make_pt(2, 200.0)]).await;

        // Second scrape for source 1
        let out1 = apply_delta(&last, 1, vec![make_pt(1, 150.0)]).await;
        assert_eq!(out1[0].value, 50.0, "source 1 delta should be 50");

        // Second scrape for source 2
        let out2 = apply_delta(&last, 2, vec![make_pt(2, 250.0)]).await;
        assert_eq!(out2[0].value, 50.0, "source 2 delta should be 50");
    }

    #[tokio::test]
    async fn apply_delta_keeps_label_series_independent() {
        // Same metric name, different label sets (two databases + one
        // unlabelled instance aggregate) must each track their own baseline.
        // Before the labels-aware key they shared (source_id, name) and
        // clobbered each other's previous value every scrape.
        let last = Arc::new(RwLock::new(HashMap::new()));

        let make_pt = |datname: Option<&str>, val: f64| {
            let mut labels = HashMap::new();
            if let Some(d) = datname {
                labels.insert("datname".to_string(), d.to_string());
            }
            MetricPoint {
                time: chrono::Utc::now(),
                source_kind: SourceKind::Database,
                source_id: 1,
                name: "pg.commits_total".into(),
                value: val,
                kind: MetricKind::Counter,
                engine: Some("postgres".into()),
                environment: None,
                node_id: None,
                labels,
            }
        };

        // First scrape: baselines for db "app" (100), db "other" (10), and the
        // unlabelled instance aggregate (110 = 100 + 10).
        let out = apply_delta(
            &last,
            1,
            vec![
                make_pt(Some("app"), 100.0),
                make_pt(Some("other"), 10.0),
                make_pt(None, 110.0),
            ],
        )
        .await;
        assert!(out.is_empty(), "first scrape sets baselines, emits nothing");

        // Second scrape: each series advances independently.
        let out = apply_delta(
            &last,
            1,
            vec![
                make_pt(Some("app"), 130.0),  // +30
                make_pt(Some("other"), 12.0), // +2
                make_pt(None, 142.0),         // +32
            ],
        )
        .await;

        let by_db = |d: &str| {
            out.iter()
                .find(|p| p.labels.get("datname").map(String::as_str) == Some(d))
                .map(|p| p.value)
        };
        let instance = out.iter().find(|p| p.labels.is_empty()).map(|p| p.value);

        assert_eq!(by_db("app"), Some(30.0), "app db delta");
        assert_eq!(by_db("other"), Some(2.0), "other db delta");
        assert_eq!(instance, Some(32.0), "instance aggregate delta");
    }

    #[test]
    fn labels_key_is_order_independent() {
        let mut a = HashMap::new();
        a.insert("x".to_string(), "1".to_string());
        a.insert("y".to_string(), "2".to_string());
        let mut b = HashMap::new();
        b.insert("y".to_string(), "2".to_string());
        b.insert("x".to_string(), "1".to_string());
        assert_eq!(labels_key(&a), labels_key(&b));
        assert_eq!(labels_key(&HashMap::new()), "");
    }
}
