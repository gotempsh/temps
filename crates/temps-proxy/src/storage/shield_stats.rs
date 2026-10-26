// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the console's Shield tab reads (ADR 066, D11 and I10): verdict
//! counts from `proxy_logs`, and events from `shield_events`.
//!
//! Like the request-log API, it reads ClickHouse when `TEMPS_CLICKHOUSE_*`
//! is configured and Postgres otherwise ([`build_shield_stats_storage`]).
//! Every query is scoped to one project and environment and a bounded time
//! window, and aggregates in the database: the console never pulls rows to
//! count them.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde::Deserialize;
use temps_core::UtcDateTime;
use temps_entities::shield_events;

/// Most sources the summary names; the rest are left out, not summed.
pub const MAX_SOURCES: u64 = 20;

#[derive(Debug, thiserror::Error)]
pub enum ShieldStatsError {
    #[error("Shield statistics query failed in Postgres ({operation}): {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
    #[error("Shield statistics query failed in ClickHouse ({operation}): {reason}")]
    ClickHouse {
        operation: &'static str,
        reason: String,
    },
}

/// A count keyed by one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedCount {
    pub key: String,
    pub count: u64,
}

/// Events in a window, by what happened and in which mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCount {
    pub kind: String,
    pub verdict: String,
    pub mode: String,
    pub count: u64,
}

/// How long solved challenges took.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolveTimes {
    pub samples: u64,
    pub median_ms: Option<f64>,
    pub p90_ms: Option<f64>,
}

/// One stored event, as the console lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredShieldEvent {
    pub timestamp: UtcDateTime,
    pub node_id: Option<i32>,
    pub request_id: String,
    pub kind: String,
    pub ip: String,
    pub ja4: Option<String>,
    pub verdict: String,
    pub mode: String,
    pub reasons: Vec<String>,
    pub signature: Option<String>,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub difficulty: Option<i16>,
    pub solve_ms: Option<i64>,
}

/// One environment and a window.
#[derive(Debug, Clone, Copy)]
pub struct StatsScope {
    pub project_id: i32,
    pub environment_id: i32,
    pub start: UtcDateTime,
    pub end: UtcDateTime,
}

#[async_trait]
pub trait ShieldStatsStorage: Send + Sync {
    /// Which backend answers, for the console to say so.
    fn backend(&self) -> &'static str;

    /// Requests by `proxy_logs.shield_verdict`, leaving out those with none.
    async fn verdict_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError>;

    /// Requests by the source each reason names (the list, or the
    /// signature), most first, at most [`MAX_SOURCES`]. A request counts
    /// once per source however many of its reasons name it.
    async fn source_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError>;

    /// `shield_events` by kind, verdict and mode.
    async fn event_counts(&self, scope: StatsScope) -> Result<Vec<EventCount>, ShieldStatsError>;

    /// The median and 90th percentile of `solve_ms` over solved challenges.
    async fn solve_times(&self, scope: StatsScope) -> Result<SolveTimes, ShieldStatsError>;

    /// The newest events before `before`, newest first.
    async fn recent_events(
        &self,
        project_id: i32,
        environment_id: i32,
        before: Option<UtcDateTime>,
        limit: u64,
    ) -> Result<Vec<StoredShieldEvent>, ShieldStatsError>;
}

/// The source a stored reason names: `spamhaus_drop (185.93.89.0/24)` is
/// `spamhaus_drop`, `CVE-2021-41773 on /cgi-bin/x` is `CVE-2021-41773`.
/// The SQL in both backends does the same split; this is its reference.
pub fn reason_source(reason: &str) -> &str {
    let before_detail = reason.split(" (").next().unwrap_or(reason);
    before_detail.split(" on ").next().unwrap_or(before_detail)
}

/// The backend the request-log API uses, for the same configuration.
pub fn build_shield_stats_storage(
    config: Option<&temps_config::ServerConfig>,
    db: Arc<DatabaseConnection>,
    compression: temps_clickhouse::ClickHouseCompression,
) -> Arc<dyn ShieldStatsStorage> {
    match config.filter(|config| config.is_clickhouse_enabled()) {
        Some(config) => Arc::new(ClickHouseShieldStats {
            client: temps_clickhouse::build_client(
                config.clickhouse_url.as_deref().unwrap_or_default(),
                config.clickhouse_database.as_deref().unwrap_or_default(),
                config.clickhouse_user.as_deref().unwrap_or_default(),
                config.clickhouse_password.as_deref().unwrap_or_default(),
                compression,
            ),
        }),
        None => Arc::new(PostgresShieldStats { db }),
    }
}

// ── Postgres ────────────────────────────────────────────────────────────────

pub struct PostgresShieldStats {
    db: Arc<DatabaseConnection>,
}

impl PostgresShieldStats {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn rows(
        &self,
        operation: &'static str,
        sql: &str,
        scope: StatsScope,
    ) -> Result<Vec<sea_orm::QueryResult>, ShieldStatsError> {
        self.db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [
                    scope.project_id.into(),
                    scope.environment_id.into(),
                    scope.start.into(),
                    scope.end.into(),
                ],
            ))
            .await
            .map_err(|source| ShieldStatsError::Database { operation, source })
    }
}

fn pg_err(operation: &'static str) -> impl FnOnce(DbErr) -> ShieldStatsError {
    move |source| ShieldStatsError::Database { operation, source }
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Rows of one environment in the window, `$1`–`$4`, in `proxy_logs` or
/// `shield_events`.
const PG_SCOPE: &str = "project_id = $1 AND environment_id = $2 \
     AND timestamp >= $3 AND timestamp < $4";

/// [`PG_SCOPE`] for `proxy_logs` aliased as `p`.
const PG_SCOPE_P: &str = "p.project_id = $1 AND p.environment_id = $2 \
     AND p.timestamp >= $3 AND p.timestamp < $4";

#[async_trait]
impl ShieldStatsStorage for PostgresShieldStats {
    fn backend(&self) -> &'static str {
        "postgres"
    }

    async fn verdict_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT shield_verdict AS key, COUNT(*)::bigint AS count FROM proxy_logs \
             WHERE {PG_SCOPE} AND shield_verdict IS NOT NULL AND shield_verdict <> '' \
             GROUP BY shield_verdict"
        );
        keyed(
            self.rows("verdict_counts", &sql, scope).await?,
            "verdict_counts",
        )
    }

    async fn source_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT label AS key, COUNT(*)::bigint AS count FROM ( \
                 SELECT DISTINCT p.id, p.timestamp, \
                     split_part(split_part(r.reason, ' (', 1), ' on ', 1) AS label \
                 FROM proxy_logs p \
                 CROSS JOIN LATERAL jsonb_array_elements_text(p.shield_reasons) AS r(reason) \
                 WHERE {PG_SCOPE_P} AND jsonb_typeof(p.shield_reasons) = 'array' \
             ) labelled \
             GROUP BY label ORDER BY count DESC, label LIMIT {MAX_SOURCES}"
        );
        keyed(
            self.rows("source_counts", &sql, scope).await?,
            "source_counts",
        )
    }

    async fn event_counts(&self, scope: StatsScope) -> Result<Vec<EventCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT kind, verdict, mode, COUNT(*)::bigint AS count FROM shield_events \
             WHERE {PG_SCOPE} GROUP BY kind, verdict, mode"
        );
        self.rows("event_counts", &sql, scope)
            .await?
            .iter()
            .map(|row| {
                Ok(EventCount {
                    kind: row.try_get("", "kind")?,
                    verdict: row.try_get("", "verdict")?,
                    mode: row.try_get("", "mode")?,
                    count: count(row.try_get("", "count")?),
                })
            })
            .collect::<Result<_, DbErr>>()
            .map_err(pg_err("event_counts"))
    }

    async fn solve_times(&self, scope: StatsScope) -> Result<SolveTimes, ShieldStatsError> {
        let sql = format!(
            "SELECT COUNT(solve_ms)::bigint AS samples, \
                 percentile_cont(0.5) WITHIN GROUP (ORDER BY solve_ms) AS median, \
                 percentile_cont(0.9) WITHIN GROUP (ORDER BY solve_ms) AS p90 \
             FROM shield_events \
             WHERE {PG_SCOPE} AND kind = 'challenge_solved' AND solve_ms IS NOT NULL"
        );
        let rows = self.rows("solve_times", &sql, scope).await?;
        let Some(row) = rows.first() else {
            return Ok(SolveTimes::default());
        };
        let read = || -> Result<SolveTimes, DbErr> {
            Ok(SolveTimes {
                samples: count(row.try_get("", "samples")?),
                median_ms: row.try_get("", "median")?,
                p90_ms: row.try_get("", "p90")?,
            })
        };
        read().map_err(pg_err("solve_times"))
    }

    async fn recent_events(
        &self,
        project_id: i32,
        environment_id: i32,
        before: Option<UtcDateTime>,
        limit: u64,
    ) -> Result<Vec<StoredShieldEvent>, ShieldStatsError> {
        let mut query = shield_events::Entity::find()
            .filter(shield_events::Column::ProjectId.eq(project_id))
            .filter(shield_events::Column::EnvironmentId.eq(environment_id));
        if let Some(before) = before {
            query = query.filter(shield_events::Column::Timestamp.lt(before));
        }
        let rows = query
            .order_by_desc(shield_events::Column::Timestamp)
            .order_by_desc(shield_events::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await
            .map_err(pg_err("recent_events"))?;
        Ok(rows
            .into_iter()
            .map(|row| StoredShieldEvent {
                timestamp: row.timestamp,
                node_id: row.node_id,
                request_id: row.request_id,
                kind: row.kind,
                ip: row.ip,
                ja4: row.ja4.filter(|ja4| !ja4.is_empty()),
                verdict: row.verdict,
                mode: row.mode,
                reasons: serde_json::from_value(row.reasons).unwrap_or_default(),
                signature: row.signature.filter(|signature| !signature.is_empty()),
                host: row.host,
                path: row.path,
                user_agent: row.user_agent,
                difficulty: row.difficulty,
                solve_ms: row.solve_ms.map(i64::from),
            })
            .collect())
    }
}

fn keyed(
    rows: Vec<sea_orm::QueryResult>,
    operation: &'static str,
) -> Result<Vec<KeyedCount>, ShieldStatsError> {
    rows.iter()
        .map(|row| {
            Ok(KeyedCount {
                key: row.try_get("", "key")?,
                count: count(row.try_get("", "count")?),
            })
        })
        .collect::<Result<_, DbErr>>()
        .map_err(pg_err(operation))
}

// ── ClickHouse ──────────────────────────────────────────────────────────────

pub struct ClickHouseShieldStats {
    client: ::clickhouse::Client,
}

#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChKeyedRow {
    key: String,
    count: u64,
}

#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChEventCountRow {
    kind: String,
    verdict: String,
    mode: String,
    count: u64,
}

#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChSolveRow {
    samples: u64,
    median: f64,
    p90: f64,
}

#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChEventRow {
    ts: i64,
    node_id: Option<i32>,
    request_id: String,
    kind: String,
    ip: String,
    ja4: String,
    verdict: String,
    mode: String,
    reasons: Vec<String>,
    signature: String,
    host: String,
    path: String,
    user_agent: String,
    difficulty: Option<u8>,
    solve_ms: Option<u32>,
}

/// One environment's rows in the window: four binds, in this order.
const CH_SCOPE: &str = "project_id = ? AND environment_id = ? \
     AND timestamp >= fromUnixTimestamp64Milli(?) AND timestamp < fromUnixTimestamp64Milli(?)";

fn bind_scope(query: ::clickhouse::query::Query, scope: StatsScope) -> ::clickhouse::query::Query {
    query
        .bind(scope.project_id)
        .bind(scope.environment_id)
        .bind(scope.start.timestamp_millis())
        .bind(scope.end.timestamp_millis())
}

/// `shield_events` is created by the proxy's ClickHouse migrations, which
/// run in the background after startup. Until it exists there are no
/// events to show, which is what an empty answer says.
fn is_missing_table(error: &::clickhouse::error::Error) -> bool {
    let message = error.to_string();
    message.contains("UNKNOWN_TABLE") || message.contains("Code: 60")
}

fn ch_err(operation: &'static str) -> impl FnOnce(::clickhouse::error::Error) -> ShieldStatsError {
    move |error| ShieldStatsError::ClickHouse {
        operation,
        reason: error.to_string(),
    }
}

/// An IPv4 client is stored as `::ffff:a.b.c.d`; the console shows it as
/// it arrived.
fn display_ip(ip: String) -> String {
    match ip.strip_prefix("::ffff:") {
        Some(v4) if v4.parse::<std::net::Ipv4Addr>().is_ok() => v4.to_string(),
        _ => ip,
    }
}

#[async_trait]
impl ShieldStatsStorage for ClickHouseShieldStats {
    fn backend(&self) -> &'static str {
        "clickhouse"
    }

    async fn verdict_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT toString(shield_verdict) AS key, count() AS count FROM proxy_logs \
             WHERE {CH_SCOPE} AND shield_verdict != '' GROUP BY key"
        );
        let rows = bind_scope(self.client.query(&sql), scope)
            .fetch_all::<ChKeyedRow>()
            .await
            .map_err(ch_err("verdict_counts"))?;
        Ok(rows
            .into_iter()
            .map(|row| KeyedCount {
                key: row.key,
                count: row.count,
            })
            .collect())
    }

    async fn source_counts(&self, scope: StatsScope) -> Result<Vec<KeyedCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT label AS key, count() AS count FROM ( \
                 SELECT arrayDistinct(arrayMap( \
                     r -> splitByString(' on ', splitByString(' (', toString(r))[1])[1], \
                     shield_reasons)) AS labels \
                 FROM proxy_logs WHERE {CH_SCOPE} AND notEmpty(shield_reasons) \
             ) ARRAY JOIN labels AS label \
             GROUP BY label ORDER BY count DESC, label LIMIT {MAX_SOURCES}"
        );
        let rows = bind_scope(self.client.query(&sql), scope)
            .fetch_all::<ChKeyedRow>()
            .await
            .map_err(ch_err("source_counts"))?;
        Ok(rows
            .into_iter()
            .map(|row| KeyedCount {
                key: row.key,
                count: row.count,
            })
            .collect())
    }

    async fn event_counts(&self, scope: StatsScope) -> Result<Vec<EventCount>, ShieldStatsError> {
        let sql = format!(
            "SELECT toString(kind) AS kind, toString(verdict) AS verdict, \
                 toString(mode) AS mode, count() AS count \
             FROM shield_events WHERE {CH_SCOPE} GROUP BY kind, verdict, mode"
        );
        match bind_scope(self.client.query(&sql), scope)
            .fetch_all::<ChEventCountRow>()
            .await
        {
            Ok(rows) => Ok(rows
                .into_iter()
                .map(|row| EventCount {
                    kind: row.kind,
                    verdict: row.verdict,
                    mode: row.mode,
                    count: row.count,
                })
                .collect()),
            Err(error) if is_missing_table(&error) => Ok(Vec::new()),
            Err(error) => Err(ch_err("event_counts")(error)),
        }
    }

    async fn solve_times(&self, scope: StatsScope) -> Result<SolveTimes, ShieldStatsError> {
        let sql = format!(
            "SELECT count() AS samples, \
                 toFloat64(quantileExact(0.5)(assumeNotNull(solve_ms))) AS median, \
                 toFloat64(quantileExact(0.9)(assumeNotNull(solve_ms))) AS p90 \
             FROM shield_events \
             WHERE {CH_SCOPE} AND kind = 'challenge_solved' AND solve_ms IS NOT NULL"
        );
        match bind_scope(self.client.query(&sql), scope)
            .fetch_one::<ChSolveRow>()
            .await
        {
            // Quantiles of nothing are not a measurement.
            Ok(row) if row.samples == 0 => Ok(SolveTimes::default()),
            Ok(row) => Ok(SolveTimes {
                samples: row.samples,
                median_ms: row.median.is_finite().then_some(row.median),
                p90_ms: row.p90.is_finite().then_some(row.p90),
            }),
            Err(error) if is_missing_table(&error) => Ok(SolveTimes::default()),
            Err(error) => Err(ch_err("solve_times")(error)),
        }
    }

    async fn recent_events(
        &self,
        project_id: i32,
        environment_id: i32,
        before: Option<UtcDateTime>,
        limit: u64,
    ) -> Result<Vec<StoredShieldEvent>, ShieldStatsError> {
        // `limit` is server-bounded by the caller; it is a number, not text.
        let sql = format!(
            "SELECT toUnixTimestamp64Milli(timestamp) AS ts, node_id, request_id, \
                 toString(kind) AS kind, toString(ip) AS ip, toString(ja4) AS ja4, \
                 toString(verdict) AS verdict, toString(mode) AS mode, \
                 arrayMap(r -> toString(r), reasons) AS reasons, \
                 toString(signature) AS signature, host, path, user_agent, difficulty, solve_ms \
             FROM shield_events \
             WHERE project_id = ? AND environment_id = ? \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
             ORDER BY timestamp DESC LIMIT {limit}"
        );
        let before = before.unwrap_or_else(Utc::now).timestamp_millis();
        let rows = match self
            .client
            .query(&sql)
            .bind(project_id)
            .bind(environment_id)
            .bind(before)
            .fetch_all::<ChEventRow>()
            .await
        {
            Ok(rows) => rows,
            Err(error) if is_missing_table(&error) => Vec::new(),
            Err(error) => return Err(ch_err("recent_events")(error)),
        };
        Ok(rows
            .into_iter()
            .map(|row| StoredShieldEvent {
                timestamp: Utc
                    .timestamp_millis_opt(row.ts)
                    .single()
                    .unwrap_or_default(),
                node_id: row.node_id,
                request_id: row.request_id,
                kind: row.kind,
                ip: display_ip(row.ip),
                ja4: Some(row.ja4).filter(|ja4| !ja4.is_empty()),
                verdict: row.verdict,
                mode: row.mode,
                reasons: row.reasons,
                signature: Some(row.signature).filter(|signature| !signature.is_empty()),
                host: row.host,
                path: row.path,
                user_agent: row.user_agent,
                difficulty: row.difficulty.map(i16::from),
                solve_ms: row.solve_ms.map(i64::from),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_names_its_source() {
        assert_eq!(
            reason_source("spamhaus_drop (185.93.89.0/24)"),
            "spamhaus_drop"
        );
        assert_eq!(
            reason_source("operator (ja4 t13d1516h2_8daaf6152771_02713d6af862, stale)"),
            "operator"
        );
        assert_eq!(
            reason_source("CVE-2021-41773 on /cgi-bin/.%2e/x"),
            "CVE-2021-41773"
        );
        assert_eq!(
            reason_source("browsers (claims Chrome, ja4 of curl)"),
            "browsers"
        );
        assert_eq!(reason_source("plain"), "plain");
    }

    #[test]
    fn ipv4_clients_read_as_they_arrived() {
        assert_eq!(display_ip("::ffff:203.0.113.9".into()), "203.0.113.9");
        assert_eq!(display_ip("2001:db8::1".into()), "2001:db8::1");
        assert_eq!(display_ip("::ffff:zz".into()), "::ffff:zz");
    }
}
