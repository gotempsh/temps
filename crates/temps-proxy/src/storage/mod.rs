//! Pluggable storage backend for proxy / request logs.
//!
//! Proxy logs are one row per HTTP request flowing through the Pingora reverse
//! proxy. Historically the write path (batch INSERT) and read path (list +
//! aggregations) were hard-wired to the `proxy_logs` TimescaleDB hypertable.
//!
//! This module introduces a [`ProxyLogStorage`] trait so the same surface can be
//! served from an alternative backend (ClickHouse) when the operator configures
//! `TEMPS_CLICKHOUSE_*` ([`temps_config::ServerConfig::is_clickhouse_enabled`]).
//! When ClickHouse is NOT configured, [`TimescaleDbProxyLogStore`] is used and
//! behaves byte-for-byte identically to the prior hard-coded path — it simply
//! relays to the existing query/insert logic.
//!
//! ## Layering
//!
//! - The HTTP handlers continue to talk to [`crate::service::proxy_log_service::ProxyLogService`].
//! - `ProxyLogService` (and the background batch writer) dispatch to an
//!   `Arc<dyn ProxyLogStorage>` chosen at construction time.
//! - The trait method signatures intentionally mirror the existing
//!   `ProxyLogService` methods one-to-one and reuse the SAME request/response
//!   DTOs (`ProxyLogResponse`, `TimeBucketStats`, `ProjectHealthSummary`,
//!   `AiAgentBreakdownRow`, etc.) so no DTO is redesigned.
//!
//! ## Hot-path safety
//!
//! [`ProxyLogStorage::write_batch`] is only ever invoked from the background
//! `ProxyLogBatchWriter` task — never from the Pingora request hot path. The
//! hot path only ever does a non-blocking `try_send` into an mpsc channel.
//! `write_batch` must fail open: on backend error it returns an `Err` which the
//! caller logs and drops (the batch is lost, live traffic is never blocked).

pub mod clickhouse;
pub mod clickhouse_migrations;
pub mod timescaledb;

pub use clickhouse::{ClickHouseProxyLogConfig, ClickHouseProxyLogStore};
pub use timescaledb::TimescaleDbProxyLogStore;

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use temps_config::ServerConfig;
use temps_core::UtcDateTime;
use temps_entities::proxy_logs;

use crate::handler::proxy_logs::ProxyLogsQuery;
use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentTimelineRow, AiPageBreakdownRow, AiStatusBreakdownRow,
    AiTimelineGroupBy, CreateProxyLogRequest, ProjectHealthSummary, ProxyLogServiceError,
    StatsFilters, TimeBucketStats,
};

/// Backend-neutral storage interface for proxy / request logs.
///
/// Every method maps one-to-one onto an existing `ProxyLogService` operation and
/// returns the existing DTOs, so swapping implementations never changes the
/// HTTP-visible result shape. All errors surface as [`ProxyLogServiceError`] so
/// the handler error-mapping flow is unchanged.
#[async_trait]
pub trait ProxyLogStorage: Send + Sync {
    /// Persist a batch of already-enriched log entries.
    ///
    /// Called ONLY from the background batch-writer task (never the Pingora hot
    /// path). Entries arrive fully enriched (UA parsed, bot/AI detection done,
    /// geo resolved) — implementations must not re-enrich, only persist. Must
    /// fail open: returning `Err` causes the caller to log-and-drop the batch;
    /// it never blocks live traffic and never panics.
    async fn write_batch(
        &self,
        entries: Vec<CreateProxyLogRequest>,
    ) -> Result<(), ProxyLogServiceError>;

    /// Paginated, filtered, sorted list of proxy logs.
    ///
    /// Returns `(rows, total)` where `total` powers `total_pages` computation in
    /// the handler. Mirrors `ProxyLogService::list_with_filters`.
    async fn list_with_filters(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: ProxyLogsQuery,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError>;

    /// Fetch a single proxy log by its serial id.
    ///
    /// `timestamp` is the row's known event time (the list endpoint returns it
    /// per row). On the TimescaleDB hypertable it bounds the lookup to the
    /// chunks around that instant instead of scanning — and decompressing —
    /// the entire retention window.
    async fn get_by_id(
        &self,
        id: i32,
        timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError>;

    /// Fetch a single proxy log by its (unique) request id, for tracing joins.
    async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError>;

    /// `stats/today` — total request count since UTC midnight.
    async fn get_today_count(
        &self,
        filters: Option<StatsFilters>,
    ) -> Result<i64, ProxyLogServiceError>;

    /// `stats/time-buckets` — gapfilled time-series of request volume / latency /
    /// errors / bytes.
    async fn get_time_bucket_stats(
        &self,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        filters: Option<StatsFilters>,
    ) -> Result<Vec<TimeBucketStats>, ProxyLogServiceError>;

    /// `stats/projects-health` — per-project request/error/latency rollup with a
    /// derived health status, including projects with no data as `unknown`.
    async fn get_projects_health_summary(
        &self,
        project_ids: &[i32],
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        is_bot: Option<bool>,
    ) -> Result<Vec<ProjectHealthSummary>, ProxyLogServiceError>;

    /// `stats/ai-agents` — per-agent request counts / unique IPs / last seen.
    async fn get_ai_agent_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentBreakdownRow>, ProxyLogServiceError>;

    /// `stats/ai-pages` — per-path request counts and distinct-agent counts for
    /// AI-crawler traffic.
    async fn get_ai_page_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiPageBreakdownRow>, ProxyLogServiceError>;

    /// `stats/ai-agents/timeline` — time-bucketed AI-agent request counts split
    /// by provider or agent, with a continuous bucket spine.
    async fn get_ai_agent_timeline(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        group_by: AiTimelineGroupBy,
    ) -> Result<Vec<AiAgentTimelineRow>, ProxyLogServiceError>;

    /// `stats/ai-status` — HTTP status-class breakdown for AI-agent traffic.
    async fn get_ai_status_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<Vec<AiStatusBreakdownRow>, ProxyLogServiceError>;
}

/// Build the proxy-log storage backend for the running server.
///
/// Returns the ClickHouse-backed store when `config.is_clickhouse_enabled()`
/// (all four `TEMPS_CLICKHOUSE_*` vars present), otherwise the default
/// TimescaleDB store — preserving the prior behaviour byte-for-byte when CH is
/// not configured.
///
/// When ClickHouse is selected, the proxy-log migrations are applied in a
/// background task (`tokio::spawn`) so server startup is never blocked. If the
/// migrations fail, a warning is logged and the first read/write surfaces the
/// error per-call (the table/columns may already exist from a prior run). This
/// mirrors the metrics/otel ClickHouse wiring exactly.
///
/// Both the HTTP handler path (via `ProxyLogService::with_storage`) and the
/// background batch-writer path call this so they share one backend selection
/// and one connection configuration. Construction does no blocking I/O.
pub fn build_proxy_log_storage(
    config: &ServerConfig,
    db: Arc<DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
    resolver: Arc<dyn temps_core::RetentionResolver>,
) -> Arc<dyn ProxyLogStorage> {
    if config.is_clickhouse_enabled() {
        // is_clickhouse_enabled() guarantees all four fields are Some.
        let cfg = ClickHouseProxyLogConfig::new(
            config.clickhouse_url.clone().unwrap_or_default(),
            config.clickhouse_database.clone().unwrap_or_default(),
            config.clickhouse_user.clone().unwrap_or_default(),
            config.clickhouse_password.clone().unwrap_or_default(),
        );
        let store = ClickHouseProxyLogStore::new(cfg, resolver);

        // Apply migrations off the startup path. Cloning the client is cheap
        // (Arc-backed internally).
        //
        // `build_proxy_log_storage` is called during plugin registration, which
        // is a SYNCHRONOUS context with no Tokio reactor — a bare `tokio::spawn`
        // here panics ("there is no reactor running"). Guard on the runtime
        // handle: spawn when one exists, otherwise run the migrations to
        // completion on a short-lived current-thread runtime (proxy crates may
        // be initialized outside the async runtime, per the Pingora note).
        let client = store.client().clone();
        let database = config.clickhouse_database.clone().unwrap_or_default();
        let run_migrations = async move {
            match clickhouse_migrations::apply_migrations(&client, &database).await {
                Ok(report) => tracing::debug!(
                    applied = ?report.applied,
                    skipped = report.skipped.len(),
                    "ClickHouse proxy-log migrations applied"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    "ClickHouse proxy-log migrations failed; proxy-log \
                     writes/queries will surface the error per-call"
                ),
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(run_migrations);
            }
            Err(_) => match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(run_migrations),
                Err(e) => tracing::warn!(
                    error = %e,
                    "Could not build a runtime to apply ClickHouse proxy-log \
                     migrations; they will be attempted on first read/write"
                ),
            },
        }

        tracing::info!(
            "Proxy/request logs: ClickHouse backend enabled (TEMPS_CLICKHOUSE_* configured)"
        );
        Arc::new(store)
    } else {
        tracing::debug!("Proxy/request logs: TimescaleDB backend (default)");
        Arc::new(TimescaleDbProxyLogStore::new(db, ip_service))
    }
}
