// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Analytics events plugin.
//!
//! Two backend wirings, picked at startup from `ServerConfig`:
//!
//! - **Default (PG-only).** Reads and writes go through
//!   `AnalyticsEventsService` against PostgreSQL/TimescaleDB. The CH
//!   fan-out worker is not started, but the outbox table is still
//!   populated by `record_event` so an operator can flip the switch
//!   later and replay history.
//! - **ClickHouse-enabled.** When all four `TEMPS_CLICKHOUSE_*` env vars
//!   are set, the read-side trait object becomes `ClickHouseEventsBackend`
//!   instead of the PG-backed service. Writes still go to PG (system of
//!   record); the `ChFanoutWorker` task continuously drains the outbox
//!   into CH. Operators get the storage choice at runtime — no rebuild
//!   with a feature flag is required.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::{debug, info, warn};

/// Analytics events tracking plugin
pub struct EventsPlugin;

impl Default for EventsPlugin {
    fn default() -> Self {
        Self
    }
}

impl TempsPlugin for EventsPlugin {
    fn name(&self) -> &'static str {
        "analytics-events"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let events_service = Arc::new(crate::services::AnalyticsEventsService::new(db));
            context.register_service(events_service);

            debug!("Analytics events services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let events_service = context.require_service::<crate::services::AnalyticsEventsService>();
        let route_table = context.require_service::<temps_proxy::CachedPeerTable>();
        let ip_address_service = context.require_service::<temps_geo::IpAddressService>();
        let cookie_crypto = context.require_service::<temps_core::CookieCrypto>();
        let config_service = context.require_service::<temps_config::ConfigService>();

        let server_config = config_service.get_server_config();

        // Default to the PG-backed read trait. If CH is enabled at config
        // time, swap the trait object for `ClickHouseEventsBackend`.
        // Writes always continue against PG via `events_writer`.
        let events_backend: Arc<dyn crate::services::AnalyticsEvents> =
            if server_config.is_clickhouse_enabled() {
                let db = context.require_service::<sea_orm::DatabaseConnection>();
                build_clickhouse_backend(&server_config, db, config_service.clone())
            } else {
                debug!("ClickHouse analytics backend disabled (TEMPS_CLICKHOUSE_* unset)");
                events_service.clone()
            };

        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        let state = Arc::new(crate::handlers::AppState {
            events_service: events_backend,
            events_writer: events_service,
            route_table,
            ip_address_service,
            cookie_crypto,
            telemetry,
            project_access_checker,
            ingest_key_service: context
                .require_service::<temps_analytics::AnalyticsIngestKeyService>(),
            ingest_rate_limiter: context
                .require_service::<temps_analytics::AnalyticsIngestRateLimiter>(),
            host_ingest_rate_limiter: Arc::new(crate::handlers::HostIngestRateLimiter::new()),
        });

        let routes = crate::handlers::configure_routes().with_state(state);
        Some(PluginRoutes::new(routes))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let events_service = context.require_service::<crate::services::AnalyticsEventsService>();
        let route_table = context.require_service::<temps_proxy::CachedPeerTable>();
        let ip_address_service = context.require_service::<temps_geo::IpAddressService>();
        let cookie_crypto = context.require_service::<temps_core::CookieCrypto>();

        // Public ingest only needs the write path — the read trait can be the
        // PG-backed service since these endpoints don't query.
        let events_backend: Arc<dyn crate::services::AnalyticsEvents> = events_service.clone();

        let telemetry = context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

        let state = Arc::new(crate::handlers::AppState {
            events_service: events_backend,
            events_writer: events_service,
            route_table,
            ip_address_service,
            cookie_crypto,
            telemetry,
            project_access_checker: None,
            // ADR-040: the keyed ingest path. Required, not optional — an
            // absent key service would silently turn every keyed request back
            // into a Host lookup, which is the exact failure this replaces.
            ingest_key_service: context
                .require_service::<temps_analytics::AnalyticsIngestKeyService>(),
            ingest_rate_limiter: context
                .require_service::<temps_analytics::AnalyticsIngestRateLimiter>(),
            host_ingest_rate_limiter: Arc::new(crate::handlers::HostIngestRateLimiter::new()),
        });

        // The CORS layer resolves keys through the very same service and
        // limiter the handler uses, so the two can never disagree about a key.
        let cors = temps_analytics::ingest_keys::KeyedIngestCors::new(
            state.ingest_key_service.clone(),
            state.ingest_rate_limiter.clone(),
        );
        let routes = crate::handlers::configure_public_routes(cors).with_state(state);
        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(<crate::handlers::EventsApiDoc as utoipa::OpenApi>::openapi())
    }
}

/// Build the ClickHouse-backed read trait object and spawn the fan-out
/// worker. Migrations are applied on a background task so plugin
/// `configure_routes` returns promptly, and are retried until they apply (a
/// server still starting heals by waiting; a decode failure is re-checked at a
/// slow cadence and recovers once the operator fixes the server) instead of
/// being attempted once and left to surface as `Code: 60` per query.
fn build_clickhouse_backend(
    server_config: &temps_config::ServerConfig,
    db: Arc<sea_orm::DatabaseConnection>,
    config: Arc<temps_config::ConfigService>,
) -> Arc<dyn crate::services::AnalyticsEvents> {
    use temps_analytics_backend::clickhouse::{ClickHouseBackend, ClickHouseConfig};

    let cfg = ClickHouseConfig::new(
        server_config.clickhouse_url.clone().unwrap_or_default(),
        server_config
            .clickhouse_database
            .clone()
            .unwrap_or_default(),
        server_config.clickhouse_user.clone().unwrap_or_default(),
        server_config
            .clickhouse_password
            .clone()
            .unwrap_or_default(),
    )
    .with_compression(config.clickhouse_compression());
    let backend = ClickHouseBackend::new(cfg);
    let client = Arc::new(backend.client_clone());

    info!(
        url = %server_config.clickhouse_url.as_deref().unwrap_or(""),
        database = %server_config.clickhouse_database.as_deref().unwrap_or(""),
        "ClickHouse analytics backend enabled — applying migrations and starting fan-out worker"
    );

    // Run migrations + start fan-out worker in the background so we don't
    // block plugin init on remote calls.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let client_for_migrations = Arc::clone(&client);
        let db_for_migrations = Arc::clone(&db);
        temps_clickhouse::spawn_retrying(
            "analytics",
            temps_clickhouse::MigrationRetryPolicy::default(),
            move || {
                let client = Arc::clone(&client_for_migrations);
                let config = Arc::clone(&config);
                let db = Arc::clone(&db_for_migrations);
                async move {
                    use temps_analytics_backend::AnalyticsBackendError;
                    let report =
                        temps_analytics_backend::migrations::apply_migrations(&client).await?;
                    // Dynamic settings are reconciled after every migration
                    // pass, even when all migrations were already recorded.
                    let policy = config.get_retention_policy().await.map_err(|e| {
                        AnalyticsBackendError::BackendUnavailable {
                            backend: "clickhouse".into(),
                            reason: format!("read events retention after migrations: {e}"),
                        }
                    })?;
                    let ttl_changed = temps_analytics_backend::retention::reconcile_events_ttl(
                        db.as_ref(),
                        &client,
                        policy.analytics.events_days,
                    )
                    .await
                    .map_err(|e| {
                        AnalyticsBackendError::BackendUnavailable {
                            backend: "clickhouse".into(),
                            reason: e.to_string(),
                        }
                    })?;
                    if ttl_changed {
                        info!(
                            database = %client.database().unwrap_or("default"),
                            days = policy.analytics.events_days,
                            "ClickHouse events TTL updated after migrations; expiry follows asynchronous merges"
                        );
                    }
                    Ok::<_, AnalyticsBackendError>(report)
                }
            },
            |report| {
                info!(
                    applied = ?report.applied,
                    skipped_count = report.skipped.len(),
                    "ClickHouse migrations applied"
                )
            },
        );

        // Spawn the fan-out worker. It owns one client clone for inserts
        // and a DB clone for the outbox claim/release.
        let client_for_worker = Arc::clone(&client);
        let db_for_worker = Arc::clone(&db);
        handle.spawn(async move {
            let worker = crate::services::ChFanoutWorker::new(
                db_for_worker,
                client_for_worker,
                crate::services::ChFanoutConfig::default(),
            );
            worker.run().await;
        });
    } else {
        warn!(
            "No tokio runtime available when initializing ClickHouse plugin; \
             migrations will not run and fan-out worker will not start. This \
             usually means the plugin was wired during a sync init path."
        );
    }

    Arc::new(crate::services::ClickHouseEventsBackend::new(client))
}
