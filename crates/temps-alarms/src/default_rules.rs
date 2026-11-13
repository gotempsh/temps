// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Built-in alert rule sets and their idempotent seeding into
//! `monitoring_alert_rules`.
//!
//! Lives here rather than beside `temps-monitoring`'s `AlertEvaluator` so that
//! crates enabling metrics on a service can install the defaults without
//! inheriting the evaluator's container-runtime dependency. The evaluator reads
//! the seed lists back to decide which rules keep the default volume policy.

use sea_orm::DatabaseConnection;
#[cfg(test)]
use sea_orm::Set;
#[cfg(test)]
use temps_entities::monitoring_alert_rules;
use temps_metrics::MetricsError;
use tracing::{debug, info};

/// Install all missing built-in alert rules atomically within the service budget.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(service_id, metric_name)` so concurrent calls are safe — no TOCTOU race.
///
/// # Engines supported
///
/// - `"postgres"` — 8 default rules covering connections, cache hit ratio,
///   replication lag, deadlocks, failing WAL archiving and WAL backlog.
/// - `"redis"` — 4 default rules covering memory fragmentation, eviction,
///   client count and keyspace hit ratio. The two ratio rules are only
///   evaluated once the instance has real volume (see [`volume_gate`]), so an
///   empty instance does not alert.
/// - `"mariadb"` — 10 default rules covering connection capacity and refused
///   connections, buffer pool hit ratio, replication lag and stopped
///   replication threads, deadlocks, long-running transactions, redo log waits
///   and open file exhaustion.
/// - `"mongodb"` — 6 default rules covering connections, queued operations,
///   WiredTiger cache pressure, replication buffer pressure, and asserts.
/// - `"rustfs"` — 3 default rules covering offline nodes and free-capacity
///   pressure, tied to the Prometheus metrics emitted by RustFS / MinIO.
/// - `"s3"` — no defaults: the AWS S3 collector only emits `s3.bucket_count`,
///   which is informational, not actionable. Self-hosted deployments use the
///   `"rustfs"` service_type and get real alerts via the Prometheus scrape.
/// - Anything else — no rules inserted.
pub const MAX_ALERT_RULES_PER_SERVICE: u64 = 100;

/// Failure to install a complete set of built-in service alerts.
#[derive(Debug, thiserror::Error)]
pub enum DefaultRuleSeedError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Service {service_id} has {existing} alert rules and needs {missing} additional built-in rules; the limit is {limit}. Remove enough custom rules before enabling metrics.")]
    Capacity {
        service_id: i32,
        existing: u64,
        missing: u64,
        limit: u64,
    },
}

pub async fn seed_default_rules(
    db: &DatabaseConnection,
    service_id: i32,
    engine: &str,
) -> Result<(), DefaultRuleSeedError> {
    let seeds: Vec<RuleSeed> = match engine.to_lowercase().as_str() {
        "postgres" => postgres_default_seeds(),
        "redis" => redis_default_seeds(),
        "mongodb" => mongodb_default_seeds(),
        "mariadb" => mariadb_default_seeds(),
        "rustfs" => rustfs_default_seeds(),
        _ => {
            debug!(
                service_id,
                engine, "seed_default_rules: no defaults for engine, skipping"
            );
            return Ok(());
        }
    };

    use sea_orm::{
        ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QuerySelect, TransactionTrait,
    };
    let transaction = db.begin().await.map_err(DefaultRuleSeedError::Database)?;
    if temps_entities::external_services::Entity::find_by_id(service_id)
        .lock_exclusive()
        .one(&transaction)
        .await
        .map_err(DefaultRuleSeedError::Database)?
        .is_none()
    {
        transaction
            .rollback()
            .await
            .map_err(DefaultRuleSeedError::Database)?;
        return Ok(());
    }
    let count = temps_entities::monitoring_alert_rules::Entity::find()
        .filter(temps_entities::monitoring_alert_rules::Column::ServiceId.eq(service_id))
        .count(&transaction)
        .await
        .map_err(DefaultRuleSeedError::Database)?;

    let existing_metrics = temps_entities::monitoring_alert_rules::Entity::find()
        .filter(temps_entities::monitoring_alert_rules::Column::ServiceId.eq(service_id))
        .all(&transaction)
        .await?;
    let missing = seeds
        .iter()
        .filter(|seed| {
            !existing_metrics
                .iter()
                .any(|rule| rule.metric_name == seed.metric_name)
        })
        .map(|seed| seed.metric_name)
        .collect::<std::collections::HashSet<_>>()
        .len() as u64;
    if count.saturating_add(missing) > MAX_ALERT_RULES_PER_SERVICE {
        transaction.rollback().await?;
        return Err(DefaultRuleSeedError::Capacity {
            service_id,
            existing: count,
            missing,
            limit: MAX_ALERT_RULES_PER_SERVICE,
        });
    }

    // Insert each rule individually with ON CONFLICT DO NOTHING so concurrent
    // invocations (e.g. rapid retries or parallel API calls) are safely handled
    // by the unique index on (service_id, metric_name).
    use sea_orm::ConnectionTrait;
    for seed in &seeds {
        let sql = format!(
            "INSERT INTO monitoring_alert_rules \
             (service_id, deployment_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled) \
             VALUES ({service_id}, NULL, '{name}', '{metric_name}', {threshold}, '{comparator}', '{severity}', {for_duration}, true) \
             ON CONFLICT (service_id, metric_name) WHERE service_id IS NOT NULL DO NOTHING",
            service_id = service_id,
            name = seed.name.replace('\'', "''"),
            metric_name = seed.metric_name.replace('\'', "''"),
            threshold = seed.threshold,
            comparator = seed.comparator.replace('\'', "''"),
            severity = seed.severity.replace('\'', "''"),
            for_duration = seed.for_duration_secs,
        );

        transaction
            .execute(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql,
            ))
            .await
            .map_err(DefaultRuleSeedError::Database)?;
    }
    transaction
        .commit()
        .await
        .map_err(DefaultRuleSeedError::Database)?;

    info!(
        service_id,
        engine, "seed_default_rules: seeded default alert rules (ON CONFLICT DO NOTHING)"
    );

    Ok(())
}

/// Insert default container alert rules for `deployment_id` if none exist yet.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(deployment_id, metric_name)` so concurrent calls are safe.
pub async fn seed_default_container_rules(
    db: &DatabaseConnection,
    deployment_id: i32,
) -> Result<(), MetricsError> {
    let seeds = container_default_seeds();

    use sea_orm::ConnectionTrait;
    for seed in &seeds {
        let sql = format!(
            "INSERT INTO monitoring_alert_rules \
             (service_id, deployment_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled) \
             VALUES (NULL, {deployment_id}, '{name}', '{metric_name}', {threshold}, '{comparator}', '{severity}', {for_duration}, true) \
             ON CONFLICT (deployment_id, metric_name) WHERE deployment_id IS NOT NULL DO NOTHING",
            deployment_id = deployment_id,
            name = seed.name.replace('\'', "''"),
            metric_name = seed.metric_name.replace('\'', "''"),
            threshold = seed.threshold,
            comparator = seed.comparator.replace('\'', "''"),
            severity = seed.severity.replace('\'', "''"),
            for_duration = seed.for_duration_secs,
        );

        db.execute(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            sql,
        ))
        .await
        .map_err(MetricsError::DatabaseError)?;
    }

    info!(
        deployment_id,
        "seed_default_container_rules: seeded default container alert rules (ON CONFLICT DO NOTHING)"
    );

    Ok(())
}

/// Synthetic node ID of the control plane (mirrors `CONTROL_PLANE_NODE_ID`
/// in temps-deployments' nodes handler and the proxy metrics sampler).
const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Insert default proxy alert rules for the control-plane node if none exist.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(node_id, metric_name)` so concurrent calls are safe.
pub async fn seed_default_proxy_rules(db: &DatabaseConnection) -> Result<(), MetricsError> {
    seed_node_rules(
        db,
        "seed_default_proxy_rules",
        CONTROL_PLANE_NODE_ID,
        &proxy_default_seeds(),
    )
    .await
}

/// Insert a default alert rule that fires when OTLP ingest requests are being
/// rate-limited at a sustained high rate, indicating either a noisy client or
/// a quota that is too low for legitimate traffic.
///
/// The metric `otel.rate_limited_requests` is written to the metrics store by
/// the OTel plugin's background pipeline-stats sampler (SourceKind::Node,
/// node_id 0) every 60 seconds as a delta counter. The default threshold of
/// 10 rejected requests per 60-second sample fires a warning if any project
/// is being consistently turned away. Operators can tighten or loosen this by
/// editing the seeded rule.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(node_id, metric_name)` so concurrent calls are safe.
pub async fn seed_default_otel_rate_limit_rules(
    db: &DatabaseConnection,
) -> Result<(), MetricsError> {
    seed_node_rules(
        db,
        "seed_default_otel_rate_limit_rules",
        CONTROL_PLANE_NODE_ID,
        &otel_rate_limit_default_seeds(),
    )
    .await
}

/// Insert default file-descriptor/socket exhaustion alert rules for the
/// control-plane node if none exist.
///
/// `NodeMetricsSampler` writes `node.fd_percent` (system-wide) and
/// `node.process_fd_percent` (this process's own `RLIMIT_NOFILE`) points for
/// node 0 on every install where the proxy runs, so the defaults always have
/// data to evaluate. Uses the same `(node_id, metric_name)` unique index as
/// [`seed_default_proxy_rules`], so this is safe to call alongside it.
///
/// Also seeds the disk-health defaults ([`node_disk_health_default_seeds`]):
/// a degraded RAID array, a worn-out drive and drive media errors.
pub async fn seed_default_node_resource_rules(db: &DatabaseConnection) -> Result<(), MetricsError> {
    let mut seeds = node_fd_default_seeds();
    seeds.extend(node_disk_health_default_seeds());
    seed_node_rules(
        db,
        "seed_default_node_resource_rules",
        CONTROL_PLANE_NODE_ID,
        &seeds,
    )
    .await
}

/// Shared `INSERT … ON CONFLICT DO NOTHING` for node-scoped default rules,
/// keyed on the `(node_id, metric_name)` unique index.
///
/// All of `seeds` go in **one** multi-row statement, so a node gets its
/// whole default set or none of it. That matters for worker nodes:
/// `sync_worker_node_rules` treats a worker holding *any* disk-health rule as
/// already seeded (so a rule the operator deleted stays deleted), and a seed
/// that stopped after the first row would leave the rest missing for good.
///
/// Every `RuleSeed` field is a `&'static str`/literal defined in this file
/// (see `proxy_default_seeds`/`node_fd_default_seeds`), never user input, but
/// this still binds every value rather than `format!()`-ing the SQL — the
/// project's raw-query rule (CLAUDE.md: `$1`, `$2` parameter binding) applies
/// regardless of whether today's callers happen to be safe, since a future
/// caller passing an operator-supplied `String` would silently inherit
/// whatever escaping this function does or doesn't do.
///
/// `caller` names the public wrapper that supplied `seeds` and is emitted as
/// the log line's prefix, so operators grepping for `seed_default_proxy_rules`
/// (the pre-refactor log text) still match.
pub async fn seed_node_rules<C: sea_orm::ConnectionTrait>(
    db: &C,
    caller: &str,
    node_id: i32,
    seeds: &[RuleSeed],
) -> Result<(), MetricsError> {
    let Some(statement) = seed_node_rules_statement(node_id, seeds) else {
        return Ok(());
    };
    db.execute(statement)
        .await
        .map_err(MetricsError::DatabaseError)?;

    info!(
        rule_count = seeds.len(),
        node_id, "{caller}: seeded default node alert rules (ON CONFLICT DO NOTHING)"
    );

    Ok(())
}

/// The single statement [`seed_node_rules`] runs; `None` for no seeds.
fn seed_node_rules_statement(node_id: i32, seeds: &[RuleSeed]) -> Option<sea_orm::Statement> {
    if seeds.is_empty() {
        return None;
    }
    const COLUMNS_PER_ROW: usize = 7;
    let mut values: Vec<sea_orm::Value> = Vec::with_capacity(seeds.len() * COLUMNS_PER_ROW);
    let rows: Vec<String> = seeds
        .iter()
        .enumerate()
        .map(|(row, seed)| {
            values.extend([
                node_id.into(),
                seed.name.into(),
                seed.metric_name.into(),
                seed.threshold.into(),
                seed.comparator.into(),
                seed.severity.into(),
                seed.for_duration_secs.into(),
            ]);
            let placeholders: Vec<String> = (1..=COLUMNS_PER_ROW)
                .map(|column| format!("${}", row * COLUMNS_PER_ROW + column))
                .collect();
            format!("(NULL, NULL, {}, true)", placeholders.join(", "))
        })
        .collect();
    Some(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        format!(
            "INSERT INTO monitoring_alert_rules \
             (service_id, deployment_id, node_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled) \
             VALUES {} \
             ON CONFLICT (node_id, metric_name) WHERE node_id IS NOT NULL DO NOTHING",
            rows.join(", ")
        ),
        values,
    ))
}

// ── Default rule builders ─────────────────────────────────────────────────────

/// Plain-data representation of a rule definition used in seed functions.
/// Avoids Sea-ORM `ActiveValue` wrappers in seed SQL generation.
pub struct RuleSeed {
    pub name: &'static str,
    pub metric_name: &'static str,
    pub threshold: f64,
    pub comparator: &'static str,
    pub severity: &'static str,
    pub for_duration_secs: i32,
}

#[cfg(test)]
fn make_service_rule(
    service_id: i32,
    name: &str,
    metric_name: &str,
    threshold: f64,
    comparator: &str,
    severity: &str,
    for_duration_secs: i32,
) -> monitoring_alert_rules::ActiveModel {
    monitoring_alert_rules::ActiveModel {
        service_id: Set(Some(service_id)),
        deployment_id: Set(None),
        name: Set(name.to_string()),
        metric_name: Set(metric_name.to_string()),
        threshold: Set(threshold),
        comparator: Set(comparator.to_string()),
        severity: Set(severity.to_string()),
        for_duration_secs: Set(for_duration_secs),
        enabled: Set(true),
        silenced_until: Set(None),
        ..Default::default()
    }
}

#[cfg(test)]
fn make_deployment_rule(
    deployment_id: i32,
    name: &str,
    metric_name: &str,
    threshold: f64,
    comparator: &str,
    severity: &str,
    for_duration_secs: i32,
) -> monitoring_alert_rules::ActiveModel {
    monitoring_alert_rules::ActiveModel {
        service_id: Set(None),
        deployment_id: Set(Some(deployment_id)),
        name: Set(name.to_string()),
        metric_name: Set(metric_name.to_string()),
        threshold: Set(threshold),
        comparator: Set(comparator.to_string()),
        severity: Set(severity.to_string()),
        for_duration_secs: Set(for_duration_secs),
        enabled: Set(true),
        silenced_until: Set(None),
        ..Default::default()
    }
}

pub fn postgres_default_seeds() -> Vec<RuleSeed> {
    // NOTE(metrics-correctness-A): The Postgres collector emits
    // `pg.replication_replay_lag_seconds` and `pg.replication_write_lag_seconds`
    // (per-replica Gauge metrics).  Earlier versions of this seed used the
    // non-existent name `pg.replication_lag_seconds` which caused the rules to
    // silently never fire.  We use `pg.replication_replay_lag_seconds` here
    // because replay lag is the operationally critical measure (it reflects
    // how far behind standby databases are from the primary).
    //
    // If `label`-based filtering (e.g. per `replica_addr`) is required in
    // future, the alert rule schema must be extended to support JSONB label
    // matchers.  For now, `query_latest` returns the latest value across all
    // replicas regardless of label, which fires if ANY replica breaches the
    // threshold.
    vec![
        RuleSeed {
            name: "High active connections (warning)",
            metric_name: "pg.connections_active",
            threshold: 80.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "High active connections (critical)",
            metric_name: "pg.connections_active",
            threshold: 95.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
        RuleSeed {
            name: "Low cache hit ratio",
            metric_name: "pg.cache_hit_ratio",
            threshold: 0.90,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Replication replay lag (warning)",
            metric_name: "pg.replication_replay_lag_seconds",
            threshold: 30.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Replication replay lag (critical)",
            metric_name: "pg.replication_replay_lag_seconds",
            threshold: 120.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
        RuleSeed {
            name: "Deadlocks detected",
            metric_name: "pg.deadlocks_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
        // WAL archiving: for 10 minutes, either the most recent
        // `archive_command` attempt failed with nothing archived since, or a
        // segment has waited 5 minutes or more to be archived (WAL-G retrying
        // an unreachable endpoint records no failure). PostgreSQL keeps every
        // segment it cannot archive, so this is the early warning of a disk
        // that will fill. A success resets the gauge to 0 and resolves it.
        // Always 0 while archiving is off.
        RuleSeed {
            name: PG_ARCHIVING_FAILING_RULE,
            metric_name: "pg.archiver_failing",
            threshold: 0.5,
            comparator: ">",
            severity: "critical",
            for_duration_secs: PG_ARCHIVING_FAILING_FOR_SECS,
        },
        // `pg_wal` more than 4x `max_wal_size` for 15 minutes. A checkpoint
        // brings a healthy server back under ~1x within minutes; staying
        // above means WAL is not being recycled (failing archiving, a stale
        // replication slot).
        RuleSeed {
            name: PG_WAL_BACKLOG_RULE,
            metric_name: "pg.wal_dir_to_max_wal_size_ratio",
            threshold: PG_WAL_BACKLOG_RATIO,
            comparator: ">",
            severity: "critical",
            for_duration_secs: PG_WAL_BACKLOG_FOR_SECS,
        },
    ]
}

/// Name of the default "WAL archiving failing" rule.
pub const PG_ARCHIVING_FAILING_RULE: &str = "WAL archiving failing";
/// How long archiving must keep failing or stalled before it fires.
pub const PG_ARCHIVING_FAILING_FOR_SECS: i32 = 600;
/// Name of the default "WAL backlog" rule.
pub const PG_WAL_BACKLOG_RULE: &str = "WAL backlog: pg_wal above 4x max_wal_size";
/// `pg_wal` size, as a multiple of `max_wal_size`, above which it fires.
pub const PG_WAL_BACKLOG_RATIO: f64 = 4.0;
/// How long `pg_wal` must stay above the ratio before it fires.
pub const PG_WAL_BACKLOG_FOR_SECS: i32 = 900;

pub fn redis_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "High memory fragmentation ratio",
            metric_name: "redis.memory_fragmentation_ratio",
            threshold: 1.5,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Keys being evicted",
            metric_name: "redis.evicted_keys_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "High connected clients",
            metric_name: "redis.connected_clients",
            threshold: 1000.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Low keyspace hit ratio",
            metric_name: "redis.keyspace_hit_ratio",
            threshold: 0.90,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
    ]
}

fn mongodb_default_seeds() -> Vec<RuleSeed> {
    // Metric names mirror the MongoDB collector's serverStatus extractor
    // (see `temps_metrics::collector::mongodb`). Connection limits are based on
    // mongod's default `maxIncomingConnections = 65536`, but most deployments
    // run with the container default of ~819 (80% of 1024 ulimit) — the
    // ratio-style metric we don't have, so absolute thresholds are used.
    vec![
        RuleSeed {
            name: "High current connections (warning)",
            metric_name: "mongo.connections_current",
            threshold: 500.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Operations queued for reads",
            metric_name: "mongo.queued_reads",
            threshold: 5.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Operations queued for writes",
            metric_name: "mongo.queued_writes",
            threshold: 5.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "WiredTiger cache near full",
            metric_name: "mongo.wiredtiger_cache_ratio",
            threshold: 0.95,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Replication buffer pressure",
            metric_name: "mongo.replication_buffer_ratio",
            threshold: 0.80,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "Cursor timeouts detected",
            metric_name: "mongo.cursor_timed_out_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
    ]
}

pub fn mariadb_default_seeds() -> Vec<RuleSeed> {
    // Metric names mirror `temps_metrics::collector::mariadb`. One rule per
    // metric: `(service_id, metric_name)` is a unique index, so a second
    // severity on the same metric would be a silent ON CONFLICT no-op.
    // Capacity rules use ratios against the server's own limits, so they hold
    // for any `max_connections` / `open_files_limit`. Replica rules only ever
    // see data on a replica: a primary emits no replica metrics.
    vec![
        RuleSeed {
            name: "Connections near max_connections",
            metric_name: "mariadb.connections_used_ratio",
            threshold: 0.80,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "Connections refused at max_connections",
            metric_name: "mariadb.connections_refused_total",
            threshold: 0.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 0,
        },
        RuleSeed {
            name: "Low buffer pool hit ratio",
            metric_name: "mariadb.buffer_pool_hit_ratio",
            threshold: 0.90,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Replication lag",
            metric_name: "mariadb.replication_lag_seconds",
            threshold: 60.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Replication stopped (SQL thread)",
            metric_name: "mariadb.replica_sql_running",
            threshold: 1.0,
            comparator: "<",
            severity: "critical",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Replica disconnected from primary (I/O thread)",
            metric_name: "mariadb.replica_io_running",
            threshold: 1.0,
            comparator: "<",
            severity: "critical",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "Deadlocks detected",
            metric_name: "mariadb.deadlocks_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
        RuleSeed {
            name: "Long-running transaction",
            metric_name: "mariadb.oldest_transaction_seconds",
            threshold: 600.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Redo log waits (log buffer too small)",
            metric_name: "mariadb.redo_log_waits_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
        RuleSeed {
            name: "Open files near open_files_limit",
            metric_name: "mariadb.open_files_usage_ratio",
            threshold: 0.85,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
    ]
}

fn rustfs_default_seeds() -> Vec<RuleSeed> {
    // The pinned RustFS image exports these exact names through OTLP. Do not use the
    // legacy `s3.*` Prometheus aliases here: RustFS has no functional MinIO
    // `/minio/v2/metrics/cluster` scrape path, so those rules never receive
    // samples for a managed RustFS service.
    //
    // Two raw thresholds we can trust:
    //   - `rustfs_cluster_health_drives_offline_count` > 0 — any offline
    //     drive degrades availability
    //   - `rustfs_cluster_capacity_free_bytes` — absolute byte thresholds are the
    //     only safe option; the collector does not emit a free/used ratio.
    //     The thresholds below assume a small-to-medium deployment. Users on
    //     larger clusters should tune these per-rule via the UI.
    vec![
        RuleSeed {
            name: "Storage nodes offline",
            metric_name: "rustfs_cluster_health_drives_offline_count",
            threshold: 0.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Low free capacity (warning)",
            // 10 GiB free
            metric_name: "rustfs_cluster_capacity_free_bytes",
            threshold: 10.0 * 1024.0 * 1024.0 * 1024.0,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Low free capacity (critical)",
            // 2 GiB free
            metric_name: "rustfs_cluster_capacity_free_bytes",
            threshold: 2.0 * 1024.0 * 1024.0 * 1024.0,
            comparator: "<",
            severity: "critical",
            for_duration_secs: 60,
        },
    ]
}

/// Default alert rules over the proxy hot-path metrics written by the proxy
/// metrics sampler (`proxy.*`, `SourceKind::Node`, control-plane node).
/// Thresholds are rate/latency based — never absolute request counts — so
/// they hold at any traffic volume.
///
/// One rule per metric name: the `uidx_monitoring_alert_rules_node_metric`
/// unique index (mirroring the service/deployment indexes) makes a second
/// seed on the same metric a silent ON CONFLICT no-op.
pub fn proxy_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "High proxy error rate",
            metric_name: "proxy.error_rate_percent",
            threshold: 20.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Slow proxy responses (p99)",
            metric_name: "proxy.request_duration_p99_ms",
            threshold: 5000.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
    ]
}

/// Default alert rules over the node resource metrics written by
/// `NodeMetricsSampler` (`node.*`, `SourceKind::Node`, control-plane node).
/// Covers file descriptor / socket exhaustion specifically: both the
/// system-wide ceiling (`fs.file-max`, which every open socket counts
/// against) and this process's own `RLIMIT_NOFILE`, since either one alone
/// can be near 100% while the other has headroom — a container typically
/// reports an effectively infinite `fs.file-max` while its process nofile
/// limit is the real, much lower ceiling.
///
/// One rule per metric, not a warning/critical pair: the
/// `uidx_monitoring_alert_rules_node_metric` unique index is on
/// `(node_id, metric_name)` alone (no `severity` column), so a second seed
/// on the same metric name is a silent `ON CONFLICT DO NOTHING` no-op — a
/// warning+critical pair here would leave the critical row never inserted.
/// (The same trap already exists in `postgres_default_seeds`'s
/// warning/critical pairs on `pg.connections_active`, which is scoped to
/// `service_id` instead and pre-dates this file; out of scope to fix here.)
/// `severity: "critical"` reflects that "near exhaustion" already means the
/// machine may start refusing new connections.
fn node_fd_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "System file descriptors near exhaustion",
            metric_name: "node.fd_percent",
            threshold: 90.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
        RuleSeed {
            name: "Process file descriptors near limit",
            metric_name: "node.process_fd_percent",
            threshold: 90.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
    ]
}

/// Default disk-health alert rules for the control-plane node.
///
/// `NodeMetricsCollector` reads `/proc/mdstat` on every sample and SMART data
/// every two minutes (see `temps_metrics::collector::disk_health`); with the
/// evaluator's 30-second cycle and no `for_duration_secs`, a degraded array
/// or a failing drive alarms well within five minutes. The rules read the
/// unlabelled aggregate series: the number of degraded arrays, the worst
/// drive's wear, and the summed media errors. The node's monitoring page
/// shows which array or drive it is.
pub fn node_disk_health_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "RAID array degraded",
            metric_name: "node.raid_degraded",
            threshold: 0.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 0,
        },
        RuleSeed {
            name: "Drive wear above 80%",
            metric_name: "node.disk_wear_percent",
            threshold: 80.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
        RuleSeed {
            name: "Drive reporting media errors",
            metric_name: "node.disk_media_errors",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
    ]
}

/// Default alert rule for OTel ingest rate limiting.
///
/// Fires when the 60-second pipeline-stats sample reports more than 10 ingest
/// requests rejected by the rate limiter.  A single noisy batch that briefly
/// tips over the limit will not fire (it fires only when the threshold is
/// crossed on a single sample, not sustained over time — `for_duration_secs`
/// is 0 for immediacy).  Operators who want sustained-breach semantics can
/// edit the rule and set `for_duration_secs`.
fn otel_rate_limit_default_seeds() -> Vec<RuleSeed> {
    vec![RuleSeed {
        name: "OTel ingest rate-limited",
        metric_name: "otel.rate_limited_requests",
        threshold: 10.0,
        comparator: ">",
        severity: "warning",
        for_duration_secs: 0,
    }]
}

fn container_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            // Threshold against utilisation relative to the CPU the container
            // may use — its limit when set, else the host's core count (100% ==
            // saturated) — NOT raw `container.cpu_percent` where 100% == one
            // core. A 2-core container would otherwise fire at ~95% raw (≈47%
            // of its limit), and an uncapped container would fire the moment it
            // used a single core of a many-core host.
            name: "High CPU usage",
            metric_name: "container.cpu_utilization_percent",
            threshold: 90.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "High memory usage",
            metric_name: "container.memory_percent",
            threshold: 85.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
    ]
}

#[cfg(test)]
fn postgres_defaults(service_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    postgres_default_seeds()
        .into_iter()
        .map(|s| {
            make_service_rule(
                service_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

#[cfg(test)]
fn redis_defaults(service_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    redis_default_seeds()
        .into_iter()
        .map(|s| {
            make_service_rule(
                service_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

#[cfg(test)]
fn container_defaults(deployment_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    container_default_seeds()
        .into_iter()
        .map(|s| {
            make_deployment_rule(
                deployment_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otel_rate_limit_default_seeds_target_the_sampler_written_metric() {
        let seeds = otel_rate_limit_default_seeds();
        assert_eq!(seeds.len(), 1);
        let seed = &seeds[0];
        // Must match the metric name the plugin.rs pipeline-stats sampler
        // actually writes, and the one the evaluator's `alarm_type_for_rule`
        // recognizes — a drift between these three would seed a rule that never
        // fires.
        assert_eq!(seed.metric_name, "otel.rate_limited_requests");
        assert_eq!(seed.comparator, ">");
        assert!(seed.threshold > 0.0);
    }

    #[test]
    fn proxy_default_seeds_are_rate_based_and_unique_per_metric() {
        let seeds = proxy_default_seeds();
        assert_eq!(seeds.len(), 2);
        let mut names = std::collections::HashSet::new();
        for s in &seeds {
            assert!(s.metric_name.starts_with("proxy."));
            // Rules must be rate/latency based, never absolute counts.
            assert!(
                s.metric_name.contains("percent") || s.metric_name.contains("_ms"),
                "seed {} is not rate/latency based",
                s.metric_name
            );
            // One rule per metric: (node_id, metric_name) is a unique index,
            // a duplicate metric would be a silent ON CONFLICT no-op.
            assert!(
                names.insert(s.metric_name),
                "duplicate seed metric {}",
                s.metric_name
            );
        }
    }

    #[test]
    fn node_fd_default_seeds_are_unique_per_metric_and_critical() {
        let seeds = node_fd_default_seeds();
        assert_eq!(seeds.len(), 2);
        let mut names = std::collections::HashSet::new();
        for s in &seeds {
            assert!(s.metric_name.starts_with("node."));
            assert!(
                s.metric_name.contains("fd_percent"),
                "seed {} is not an fd/socket exhaustion metric",
                s.metric_name
            );
            // "Near exhaustion" is inherently a critical condition — see the
            // doc comment on node_fd_default_seeds for why this can't be a
            // warning+critical pair under the current unique index.
            assert_eq!(s.severity, "critical");
            // One rule per metric: (node_id, metric_name) is a unique index,
            // a duplicate metric would be a silent ON CONFLICT no-op.
            assert!(
                names.insert(s.metric_name),
                "duplicate seed metric {}",
                s.metric_name
            );
        }
    }

    #[test]
    fn node_disk_health_seeds_match_the_issue_thresholds() {
        let seeds = node_disk_health_default_seeds();
        let find = |metric: &str| {
            seeds
                .iter()
                .find(|s| s.metric_name == metric)
                .unwrap_or_else(|| panic!("no seed for {metric}"))
        };
        let raid = find("node.raid_degraded");
        assert_eq!((raid.comparator, raid.threshold), (">", 0.0));
        assert_eq!(raid.severity, "critical");
        let wear = find("node.disk_wear_percent");
        assert_eq!((wear.comparator, wear.threshold), (">", 80.0));
        assert_eq!(wear.severity, "warning");
        let errors = find("node.disk_media_errors");
        assert_eq!((errors.comparator, errors.threshold), (">", 0.0));
        assert_eq!(errors.severity, "warning");
        // Five-minute acceptance: no sustained-breach delay on top of the
        // two-minute SMART refresh and the 30 s evaluation cycle.
        assert!(seeds.iter().all(|s| s.for_duration_secs == 0));
        // Shares the (node_id, metric_name) unique index with the fd seeds.
        let fd: Vec<&str> = node_fd_default_seeds()
            .iter()
            .map(|s| s.metric_name)
            .collect();
        assert!(seeds.iter().all(|s| !fd.contains(&s.metric_name)));
    }

    #[test]
    fn rustfs_default_seeds_use_beta6_otlp_metric_names() {
        let names: Vec<&str> = rustfs_default_seeds()
            .iter()
            .map(|seed| seed.metric_name)
            .collect();

        assert!(names.contains(&"rustfs_cluster_health_drives_offline_count"));
        assert!(names.contains(&"rustfs_cluster_capacity_free_bytes"));
        assert!(names.iter().all(|name| !name.starts_with("s3.")));
    }

    #[test]
    fn postgres_defaults_generates_eight_rules() {
        let rules = postgres_defaults(42);
        assert_eq!(rules.len(), 8);
        for r in &rules {
            assert_eq!(r.service_id.clone().unwrap(), Some(42));
            assert_eq!(r.deployment_id.clone().unwrap(), None);
        }
        // Correctness fix: replication lag rules must reference the actual
        // collected metric name (pg.replication_replay_lag_seconds), not the
        // non-existent pg.replication_lag_seconds.
        let repl_names: Vec<String> = rules
            .iter()
            .filter(|r| r.metric_name.clone().unwrap().contains("replication"))
            .map(|r| r.metric_name.clone().unwrap())
            .collect();
        assert!(
            !repl_names.is_empty(),
            "should have at least one replication rule"
        );
        for name in &repl_names {
            assert_eq!(
                name, "pg.replication_replay_lag_seconds",
                "replication lag rule should reference pg.replication_replay_lag_seconds, got: {name}"
            );
        }
    }

    #[test]
    fn redis_defaults_generates_four_rules() {
        let rules = redis_defaults(7);
        assert_eq!(rules.len(), 4);
    }

    #[test]
    fn container_defaults_generates_two_rules() {
        let rules = container_defaults(99);
        assert_eq!(rules.len(), 2);
        for r in &rules {
            assert_eq!(r.deployment_id.clone().unwrap(), Some(99));
            assert_eq!(r.service_id.clone().unwrap(), None);
        }
    }

    #[test]
    fn postgres_defaults_severities_correct() {
        let rules = postgres_defaults(1);
        let conn_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.metric_name.clone().unwrap() == "pg.connections_active")
            .collect();
        assert_eq!(conn_rules.len(), 2);
        let severities: std::collections::HashSet<String> = conn_rules
            .iter()
            .map(|r| r.severity.clone().unwrap())
            .collect();
        assert!(severities.contains("warning"));
        assert!(severities.contains("critical"));
    }

    #[test]
    fn container_defaults_metric_names() {
        let rules = container_defaults(1);
        let names: Vec<String> = rules
            .iter()
            .map(|r| r.metric_name.clone().unwrap())
            .collect();
        // The CPU rule thresholds limit-relative utilisation, not raw
        // `container.cpu_percent` (100% == one core) — see the seed comment.
        assert!(names.contains(&"container.cpu_utilization_percent".to_string()));
        assert!(names.contains(&"container.memory_percent".to_string()));
    }

    /// A node's default rules are one statement: it lands whole or not at
    /// all, so a failure can never leave a worker with some disk-health rules
    /// (which marks it seeded) and the rest missing for good.
    #[tokio::test]
    async fn a_node_is_seeded_in_one_statement() {
        let seeds = node_disk_health_default_seeds();
        let statement = seed_node_rules_statement(5, &seeds).expect("seeds");
        let values = statement.values.as_ref().expect("bound values");
        assert_eq!(values.0.len(), seeds.len() * 7);
        let last = seeds.len() * 7;
        assert!(
            statement.sql.contains(&format!("${last}, true)")),
            "{}",
            statement.sql
        );
        assert!(!statement.sql.contains(&format!("${}", last + 1)));
        assert_eq!(statement.sql.matches("(NULL, NULL, $").count(), seeds.len());
        assert!(seed_node_rules_statement(5, &[]).is_none());

        // A failing statement inserts nothing and is reported.
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        assert!(seed_node_rules(&db, "test", 5, &seeds).await.is_err());
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            1,
            "one round trip, no partial follow-ups: {log:?}"
        );
    }

    // ── Public seeding entry points ───────────────────────────────────────────

    fn executed_sql(db: sea_orm::DatabaseConnection) -> Vec<sea_orm::Statement> {
        db.into_transaction_log()
            .iter()
            .flat_map(|transaction| transaction.statements().to_vec())
            .collect()
    }

    fn exec_ok() -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }
    }

    #[tokio::test]
    async fn seed_default_rules_skips_engines_without_defaults() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        seed_default_rules(&db, 1, "s3")
            .await
            .expect("an engine with no defaults is not an error");
        assert!(executed_sql(db).is_empty());
    }

    #[tokio::test]
    async fn seed_default_rules_inserts_nothing_for_a_missing_service() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<temps_entities::external_services::Model>::new()])
            .into_connection();
        seed_default_rules(&db, 404, "postgres")
            .await
            .expect("a service deleted concurrently is not an error");
        let statements = executed_sql(db);
        assert!(
            statements.iter().all(|s| !s.sql.contains("INSERT")),
            "{statements:?}"
        );
    }

    fn service_row(service_id: i32) -> temps_entities::external_services::Model {
        temps_entities::external_services::Model {
            id: service_id,
            name: "db".to_string(),
            service_type: "postgres".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            slug: Some("db".to_string()),
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: true,
            default_backup_provisioned: false,
            container_name: None,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
            workload_mode: Default::default(),
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    fn count_row(num_items: i64) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([("num_items", sea_orm::Value::BigInt(Some(num_items)))])
    }

    #[tokio::test]
    async fn seed_default_rules_inserts_every_seed_for_a_service_with_room() {
        let seeds = postgres_default_seeds();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([[service_row(3)]])
            .append_query_results([[count_row(0)]])
            .append_query_results([Vec::<monitoring_alert_rules::Model>::new()])
            .append_exec_results(seeds.iter().map(|_| exec_ok()))
            .into_connection();
        seed_default_rules(&db, 3, "POSTGRES")
            .await
            .expect("seeding succeeds");
        let statements = executed_sql(db);
        let inserts: Vec<_> = statements
            .iter()
            .filter(|s| s.sql.contains("INSERT INTO monitoring_alert_rules"))
            .collect();
        assert_eq!(inserts.len(), seeds.len(), "{statements:?}");
        for (insert, seed) in inserts.iter().zip(&seeds) {
            assert!(insert.sql.contains("VALUES (3, NULL, "), "{}", insert.sql);
            assert!(insert.sql.contains(seed.metric_name), "{}", insert.sql);
            assert!(
                insert.sql.contains("ON CONFLICT (service_id, metric_name)"),
                "{}",
                insert.sql
            );
        }
    }

    /// Installing only part of a default set would leave a service with gaps
    /// nobody asked for, so going over the budget inserts nothing.
    #[tokio::test]
    async fn seed_default_rules_over_the_budget_inserts_nothing() {
        let missing = postgres_default_seeds()
            .iter()
            .map(|seed| seed.metric_name)
            .collect::<std::collections::HashSet<_>>()
            .len() as u64;
        let existing = MAX_ALERT_RULES_PER_SERVICE - missing + 1;
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([[service_row(3)]])
            .append_query_results([[count_row(existing as i64)]])
            .append_query_results([Vec::<monitoring_alert_rules::Model>::new()])
            .into_connection();
        let error = seed_default_rules(&db, 3, "postgres")
            .await
            .expect_err("over the budget is refused");
        assert!(
            matches!(
                error,
                DefaultRuleSeedError::Capacity {
                    service_id: 3,
                    existing: e,
                    missing: m,
                    limit: MAX_ALERT_RULES_PER_SERVICE,
                } if e == existing && m == missing
            ),
            "{error:?}"
        );
        let statements = executed_sql(db);
        assert!(
            statements.iter().all(|s| !s.sql.contains("INSERT")),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn seed_default_container_rules_inserts_every_seed_for_the_deployment() {
        let seeds = container_default_seeds();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results(seeds.iter().map(|_| exec_ok()))
            .into_connection();
        seed_default_container_rules(&db, 99)
            .await
            .expect("seeding succeeds");
        let statements = executed_sql(db);
        assert_eq!(statements.len(), seeds.len());
        for (statement, seed) in statements.iter().zip(&seeds) {
            assert!(
                statement.sql.contains("VALUES (NULL, 99, "),
                "{}",
                statement.sql
            );
            assert!(
                statement.sql.contains(seed.metric_name),
                "{}",
                statement.sql
            );
            assert!(
                statement
                    .sql
                    .contains("ON CONFLICT (deployment_id, metric_name)"),
                "{}",
                statement.sql
            );
        }
    }

    #[tokio::test]
    async fn seed_default_container_rules_reports_a_database_failure() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let error = seed_default_container_rules(&db, 99)
            .await
            .expect_err("the failure is surfaced");
        assert!(matches!(error, MetricsError::DatabaseError(_)), "{error:?}");
    }

    /// Assert one multi-row statement, bound to the control-plane node, naming
    /// exactly `seeds`.
    fn assert_seeded_on_control_plane(db: sea_orm::DatabaseConnection, seeds: &[RuleSeed]) {
        let statements = executed_sql(db);
        assert_eq!(statements.len(), 1, "{statements:?}");
        let values = &statements[0].values.as_ref().expect("bound values").0;
        assert_eq!(values.len(), seeds.len() * 7);
        for (row, seed) in values.chunks(7).zip(seeds) {
            assert_eq!(row[0], sea_orm::Value::from(CONTROL_PLANE_NODE_ID));
            assert_eq!(row[2], sea_orm::Value::from(seed.metric_name));
        }
    }

    #[tokio::test]
    async fn seed_default_proxy_rules_targets_the_control_plane() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([exec_ok()])
            .into_connection();
        seed_default_proxy_rules(&db)
            .await
            .expect("seeding succeeds");
        assert_seeded_on_control_plane(db, &proxy_default_seeds());
    }

    #[tokio::test]
    async fn seed_default_otel_rate_limit_rules_targets_the_control_plane() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([exec_ok()])
            .into_connection();
        seed_default_otel_rate_limit_rules(&db)
            .await
            .expect("seeding succeeds");
        assert_seeded_on_control_plane(db, &otel_rate_limit_default_seeds());
    }

    #[tokio::test]
    async fn seed_default_node_resource_rules_seeds_fd_and_disk_health_together() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([exec_ok()])
            .into_connection();
        seed_default_node_resource_rules(&db)
            .await
            .expect("seeding succeeds");
        let mut seeds = node_fd_default_seeds();
        seeds.extend(node_disk_health_default_seeds());
        assert_seeded_on_control_plane(db, &seeds);
    }
}
