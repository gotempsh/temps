// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Analytics Plugin implementation for the Temps plugin system
//!
//! This plugin provides analytics functionality including:
//! - Web analytics metrics and reporting
//! - Visitor tracking and session analytics
//! - Page views and performance metrics
//! - Real-time analytics data

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::CookieCrypto;
use utoipa::{openapi::OpenApi, OpenApi as OpenApiTrait};

use crate::api_traffic::ApiTrafficService;
use crate::handler::{configure_routes, AnalyticsApiDoc, AppState};
use crate::ingest_keys::{
    configure_ingest_key_routes, AnalyticsIngestKeyApiDoc, AnalyticsIngestKeyService,
    AnalyticsIngestKeysAppState, AnalyticsIngestRateLimiter,
};
use crate::{Analytics, AnalyticsService};

/// Analytics Plugin for web analytics and visitor tracking
pub struct AnalyticsPlugin;

impl AnalyticsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnalyticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for AnalyticsPlugin {
    fn name(&self) -> &'static str {
        "analytics"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let cookie_crypto = context.require_service::<CookieCrypto>();

            // The provider-neutral AI registry is always present. It reports
            // unavailable at runtime when no gateway key or host CLI is ready.
            // Requiring it here prevents plugin order from being captured as a
            // permanent false "not configured" state.
            let ai = context.require_service::<dyn temps_ai::AiService>();

            // Create AnalyticsService
            let analytics_service =
                Arc::new(AnalyticsService::new(db.clone(), cookie_crypto.clone()));

            // Create ApiTrafficService (shares the DB connection and AI registry)
            let api_traffic_service = Arc::new(ApiTrafficService::new(db.clone(), ai));
            context.register_service(api_traffic_service);

            // Register the analytics service with both the concrete type and trait
            context.register_service(analytics_service.clone());
            let analytics_trait: Arc<dyn Analytics> = analytics_service;
            context.register_service(analytics_trait);

            // ADR-040 ingest keys. Registered as shared singletons because the
            // three analytics ingest crates must observe the *same* resolution
            // cache and the same rate-limit windows as the admin CRUD surface
            // — a second instance would keep serving a key that this one just
            // revoked.
            context.register_service(Arc::new(AnalyticsIngestKeyService::new(db.clone())));
            context.register_service(Arc::new(AnalyticsIngestRateLimiter::new()));

            tracing::debug!("Analytics plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the AnalyticsService from the context
        let analytics_service = context.require_service::<dyn Analytics>();
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();
        let api_traffic_service = context.require_service::<ApiTrafficService>();

        // Create AppState
        let app_state = Arc::new(AppState {
            analytics_service,
            project_access_checker,
            api_traffic_service,
        });

        // ADR-040 ingest-key admin CRUD. Its own state so the key service and
        // audit logger are held directly rather than reached through the
        // analytics service.
        let ingest_keys_state = Arc::new(AnalyticsIngestKeysAppState {
            ingest_key_service: context.require_service::<AnalyticsIngestKeyService>(),
            // The very same limiter the public ingest handlers resolve, not a
            // second one: revoking a key must release the bucket that ingest
            // has been filling, and a private instance here would release
            // nothing.
            rate_limiter: context.require_service::<AnalyticsIngestRateLimiter>(),
            audit_service: context.require_service::<dyn temps_core::AuditLogger>(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
        });

        // Configure routes with the state
        let routes = configure_routes()
            .with_state(app_state)
            .merge(configure_ingest_key_routes().with_state(ingest_keys_state));

        Some(PluginRoutes::new(routes))
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let service = context.require_service::<ApiTrafficService>();
            if let Some(source) =
                context.get_service::<dyn crate::api_traffic::ApiTrafficDataSource>()
            {
                if !service.set_data_source(source) {
                    tracing::warn!(
                        "analytics: API traffic data source was already initialized; keeping the first source"
                    );
                }
            } else {
                tracing::warn!(
                    "analytics: no storage-neutral API traffic source registered; falling back to TimescaleDB"
                );
            }

            // ADR-040: a deployment transition updates
            // `environments.current_deployment_id` without touching
            // `analytics_ingest_keys` at all, so the ingest-key resolve
            // cache's targeted invalidation (on rotate/revoke/update) never
            // fires for it. An environment-scoped key would otherwise keep
            // attributing events to the previous deployment for up to
            // `RESOLVE_CACHE_TTL` after every deploy. `Job::RouteTableUpdated`
            // — the same in-process signal the route table itself reloads
            // on — fires exactly when that column changes, so subscribing
            // here closes the window immediately instead of waiting it out.
            let job_queue = context.require_service::<dyn temps_core::JobQueue>();
            let ingest_key_service = context.require_service::<AnalyticsIngestKeyService>();
            let mut job_receiver = job_queue.subscribe();
            tokio::spawn(async move {
                loop {
                    match job_receiver.recv().await {
                        Ok(temps_core::Job::RouteTableUpdated(_)) => {
                            ingest_key_service.invalidate_all_cached_scopes();
                        }
                        Ok(_) => {}
                        Err(temps_core::QueueError::ChannelClosed) => {
                            // The queue is gone — the process is shutting down.
                            tracing::warn!(
                                "analytics: ingest-key cache invalidation subscriber stopping, queue channel closed"
                            );
                            break;
                        }
                        Err(e) => {
                            // Broadcast receiver lagged: a RouteTableUpdated may
                            // have been missed. Invalidate defensively — the next
                            // resolve simply re-reads the current deployment id,
                            // which is always correct regardless of how many
                            // updates were dropped.
                            tracing::warn!(
                                "analytics: ingest-key cache invalidation subscriber lagged ({}); invalidating defensively",
                                e
                            );
                            ingest_key_service.invalidate_all_cached_scopes();
                        }
                    }
                }
            });

            Ok(())
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut doc = AnalyticsApiDoc::openapi();
        doc.merge(AnalyticsIngestKeyApiDoc::openapi());
        Some(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analytics_plugin_name() {
        let analytics_plugin = AnalyticsPlugin::new();
        assert_eq!(analytics_plugin.name(), "analytics");
    }

    #[test]
    fn test_analytics_plugin_default() {
        let analytics_plugin = AnalyticsPlugin;
        assert_eq!(analytics_plugin.name(), "analytics");
    }
}
