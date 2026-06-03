//! TimescaleDB (PostgreSQL) implementation of [`ProxyLogStorage`].
//!
//! This is the DEFAULT backend and preserves the exact prior behaviour: every
//! read method delegates to the existing [`ProxyLogService`] query logic, and
//! the write path reproduces the prior multi-row batch INSERT against the
//! `proxy_logs` hypertable. No behaviour changes — this is purely the original
//! code relocated behind the trait.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use temps_core::UtcDateTime;
use temps_entities::proxy_logs;
use tracing::debug;

use super::ProxyLogStorage;
use crate::handler::proxy_logs::ProxyLogsQuery;
use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentTimelineRow, AiPageBreakdownRow, AiStatusBreakdownRow,
    AiTimelineGroupBy, CreateProxyLogRequest, ProjectHealthSummary, ProxyLogService,
    ProxyLogServiceError, StatsFilters, TimeBucketStats,
};

/// The exact column list (and order) used by the prior `batch_insert`. Kept
/// verbatim so the INSERT is byte-for-byte identical to the original hard-coded
/// path. NOTE: `trace_id` and `error_group_id` are intentionally omitted here to
/// match the original behaviour (they exist on the entity but were never written
/// by the batch path).
const INSERT_COLUMNS: [&str; 35] = [
    "timestamp",
    "method",
    "path",
    "query_string",
    "host",
    "status_code",
    "response_time_ms",
    "request_source",
    "is_system_request",
    "routing_status",
    "project_id",
    "environment_id",
    "deployment_id",
    "session_id",
    "visitor_id",
    "container_id",
    "upstream_host",
    "error_message",
    "client_ip",
    "user_agent",
    "referrer",
    "request_id",
    "ip_geolocation_id",
    "browser",
    "browser_version",
    "operating_system",
    "device_type",
    "is_bot",
    "bot_name",
    "request_size_bytes",
    "response_size_bytes",
    "cache_status",
    "request_headers",
    "response_headers",
    "created_date",
];

/// TimescaleDB-backed proxy log storage.
///
/// Holds the same primitives the prior code used (`db` + `ip_service`) and an
/// internal [`ProxyLogService`] used purely to relay read queries — that struct
/// is a thin wrapper over the same two `Arc`s, so this introduces no extra
/// connections or state. Writes are performed directly here via the original
/// multi-row INSERT.
pub struct TimescaleDbProxyLogStore {
    db: Arc<DatabaseConnection>,
    /// Internal relay used to dispatch read queries to the unchanged
    /// `ProxyLogService` implementation. Cheap to hold (two `Arc` clones).
    reader: ProxyLogService,
}

impl TimescaleDbProxyLogStore {
    pub fn new(db: Arc<DatabaseConnection>, ip_service: Arc<temps_geo::IpAddressService>) -> Self {
        let reader = ProxyLogService::new(db.clone(), ip_service);
        Self { db, reader }
    }

    /// Multi-row batch INSERT, identical to the original
    /// `ProxyLogBatchWriter::batch_insert`. Entries are expected to be already
    /// enriched by the caller.
    async fn batch_insert(&self, entries: &[CreateProxyLogRequest]) -> Result<(), sea_orm::DbErr> {
        if entries.is_empty() {
            return Ok(());
        }

        let cols_per_row = INSERT_COLUMNS.len();

        let mut sql = format!(
            "INSERT INTO proxy_logs ({}) VALUES ",
            INSERT_COLUMNS.join(", ")
        );

        let mut params: Vec<sea_orm::Value> = Vec::with_capacity(entries.len() * cols_per_row);
        let now = chrono::Utc::now();

        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            let offset = i * cols_per_row;
            sql.push('(');
            for j in 0..cols_per_row {
                if j > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!("${}", offset + j + 1));
            }
            sql.push(')');

            let created_date = now.date_naive();

            params.push(now.into()); // timestamp
            params.push(entry.method.clone().into()); // method
            params.push(entry.path.clone().into()); // path
            params.push(entry.query_string.clone().into()); // query_string
            params.push(entry.host.clone().into()); // host
            params.push(entry.status_code.into()); // status_code
            params.push(entry.response_time_ms.into()); // response_time_ms
            params.push(entry.request_source.clone().into()); // request_source
            params.push(entry.is_system_request.into()); // is_system_request
            params.push(entry.routing_status.clone().into()); // routing_status
            params.push(entry.project_id.into()); // project_id
            params.push(entry.environment_id.into()); // environment_id
            params.push(entry.deployment_id.into()); // deployment_id
            params.push(entry.session_id.into()); // session_id
            params.push(entry.visitor_id.into()); // visitor_id
            params.push(entry.container_id.clone().into()); // container_id
            params.push(entry.upstream_host.clone().into()); // upstream_host
            params.push(entry.error_message.clone().into()); // error_message
            params.push(entry.client_ip.clone().into()); // client_ip
            params.push(entry.user_agent.clone().into()); // user_agent
            params.push(entry.referrer.clone().into()); // referrer
            params.push(entry.request_id.clone().into()); // request_id
            params.push(entry.ip_geolocation_id.into()); // ip_geolocation_id
            params.push(entry.browser.clone().into()); // browser
            params.push(entry.browser_version.clone().into()); // browser_version
            params.push(entry.operating_system.clone().into()); // operating_system
            params.push(entry.device_type.clone().into()); // device_type
            params.push(entry.is_bot.into()); // is_bot
            params.push(entry.bot_name.clone().into()); // bot_name
            params.push(entry.request_size_bytes.into()); // request_size_bytes
            params.push(entry.response_size_bytes.into()); // response_size_bytes
            params.push(entry.cache_status.clone().into()); // cache_status
            params.push(entry.request_headers.clone().into()); // request_headers
            params.push(entry.response_headers.clone().into()); // response_headers
            params.push(created_date.into()); // created_date
        }

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                params,
            ))
            .await?;

        debug!("Batch inserted {} proxy log entries", entries.len());
        Ok(())
    }
}

#[async_trait]
impl ProxyLogStorage for TimescaleDbProxyLogStore {
    async fn write_batch(
        &self,
        entries: Vec<CreateProxyLogRequest>,
    ) -> Result<(), ProxyLogServiceError> {
        self.batch_insert(&entries).await?;
        Ok(())
    }

    async fn list_with_filters(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: ProxyLogsQuery,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError> {
        self.reader
            .list_with_filters(start_date, end_date, filters, page, page_size)
            .await
    }

    async fn get_by_id(&self, id: i32) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        self.reader.get_by_id(id).await
    }

    async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        self.reader.get_by_request_id(request_id).await
    }

    async fn get_today_count(
        &self,
        filters: Option<StatsFilters>,
    ) -> Result<i64, ProxyLogServiceError> {
        self.reader.get_today_count(filters).await
    }

    async fn get_time_bucket_stats(
        &self,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        filters: Option<StatsFilters>,
    ) -> Result<Vec<TimeBucketStats>, ProxyLogServiceError> {
        self.reader
            .get_time_bucket_stats(start_time, end_time, bucket_interval, filters)
            .await
    }

    async fn get_projects_health_summary(
        &self,
        project_ids: &[i32],
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        is_bot: Option<bool>,
    ) -> Result<Vec<ProjectHealthSummary>, ProxyLogServiceError> {
        self.reader
            .get_projects_health_summary(project_ids, start_time, end_time, is_bot)
            .await
    }

    async fn get_ai_agent_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_agent_breakdown(
                project_id,
                environment_id,
                path,
                start_time,
                end_time,
                limit,
            )
            .await
    }

    async fn get_ai_page_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiPageBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_page_breakdown(
                project_id,
                environment_id,
                path,
                start_time,
                end_time,
                limit,
            )
            .await
    }

    async fn get_ai_agent_timeline(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        group_by: AiTimelineGroupBy,
    ) -> Result<Vec<AiAgentTimelineRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_agent_timeline(
                project_id,
                environment_id,
                start_time,
                end_time,
                bucket_interval,
                group_by,
            )
            .await
    }

    async fn get_ai_status_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<Vec<AiStatusBreakdownRow>, ProxyLogServiceError> {
        self.reader
            .get_ai_status_breakdown(project_id, environment_id, start_time, end_time)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> TimescaleDbProxyLogStore {
        unsafe { std::env::set_var("TEMPS_GEO_MOCK", "true") };
        let db = Arc::new(DatabaseConnection::default());
        let geoip =
            Arc::new(temps_geo::GeoIpService::new().expect("Failed to create mock GeoIpService"));
        let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip));
        TimescaleDbProxyLogStore::new(db, ip_service)
    }

    #[tokio::test]
    async fn test_batch_insert_empty_is_noop() {
        let store = make_store();
        // Empty batch must short-circuit without touching the (disconnected) db.
        let result = store.batch_insert(&[]).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_insert_columns_are_35() {
        // Regression guard: the prior batch_insert wrote exactly these 35
        // columns. trace_id / error_group_id are intentionally excluded.
        assert_eq!(INSERT_COLUMNS.len(), 35);
        assert!(!INSERT_COLUMNS.contains(&"trace_id"));
        assert!(!INSERT_COLUMNS.contains(&"error_group_id"));
        assert!(!INSERT_COLUMNS.contains(&"id"));
    }

    #[test]
    fn test_store_constructs() {
        let _store = make_store();
    }
}
