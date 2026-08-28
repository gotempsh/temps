// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Config Plugin implementation for the Temps plugin system
//!
//! This plugin provides configuration management functionality including:
//! - Server configuration management
//! - Application settings
//! - Logging configuration

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::{openapi::OpenApi, OpenApi as OpenApiTrait};

use crate::handler::SettingsState;
use crate::{configure_routes, ConfigService, ServerConfig, SettingsApiDoc};

/// Config Plugin for managing application configuration
pub struct ConfigPlugin {
    server_config: Arc<ServerConfig>,
}

impl ConfigPlugin {
    pub fn new(server_config: Arc<ServerConfig>) -> Self {
        Self { server_config }
    }
}

impl TempsPlugin for ConfigPlugin {
    fn name(&self) -> &'static str {
        "config"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Get required dependencies from the service registry
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            // Create ConfigService
            let config_service =
                Arc::new(ConfigService::new(self.server_config.clone(), db.clone()));

            // A control plane owns its cluster trust root independently of
            // whether a worker has joined yet. Full `temps serve` processes
            // pre-register the encryption service, so initialize the CA as
            // part of startup. The standalone proxy intentionally does not
            // carry encryption material and therefore skips this step; the
            // control-plane process remains the sole CA owner.
            if let Some(encryption_service) = context.get_service::<temps_core::EncryptionService>()
            {
                crate::cluster_ca::ensure_cluster_ca(&config_service, &encryption_service)
                    .await
                    .map_err(|error| PluginError::PluginRegistrationFailed {
                        plugin_name: self.name().to_string(),
                        error: format!("failed to initialize cluster CA: {error}"),
                    })?;
                tracing::info!("Cluster CA is initialized");
            }

            // Start the cross-process settings cache invalidation listener on
            // THIS shared singleton (Postgres LISTEN/NOTIFY on `settings_change`).
            // Non-fatal on failure: the 5s cache TTL remains the safety net.
            config_service.start_settings_listener();

            context.register_service(config_service);

            tracing::debug!("Config plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the ConfigService from the context
        let config_service = context.require_service::<ConfigService>();
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        let sensitive_action_authorizer =
            context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();
        let encryption_service = context.require_service::<temps_core::EncryptionService>();
        let db = context.require_service::<sea_orm::DatabaseConnection>();
        let enrollment_token_service =
            Arc::new(crate::enrollment_tokens::EnrollmentTokenService::new(db));

        // Get the route table refresher if available (it's registered by the proxy subsystem)
        let route_table_refresher =
            context.get_service::<dyn temps_core::route_table::RouteTableRefresher>();

        // Update-notifier slot, when the host process runs one (`temps serve`
        // registers it; the standalone proxy's isolated plugin context does
        // not). Optional: without it, update-status just reports "no update".
        let update_status = context.get_service::<temps_core::UpdateStatusSlot>();

        // Same optionality as the slot above, for the same reason: only a host
        // that owns the binary and the process lifecycle registers an updater.
        let self_updater = context.get_service::<dyn temps_core::SelfUpdater>();

        // Create SettingsState
        let settings_state = Arc::new(SettingsState {
            config_service,
            encryption_service,
            audit_service,
            sensitive_action_authorizer,
            route_table_refresher,
            enrollment_token_service,
            update_status,
            self_updater,
        });

        // Configure routes with the state
        let routes = configure_routes().with_state(settings_state);

        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(SettingsApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn test_config_plugin_name() {
        let server_config = Arc::new(
            ServerConfig::new(
                "127.0.0.1:8000".to_string(),
                "sqlite:temps.db".to_string(),
                None,
                None,
            )
            .unwrap(),
        );
        let config_plugin = ConfigPlugin::new(server_config);
        assert_eq!(config_plugin.name(), "config");
    }

    #[tokio::test]
    async fn control_plane_startup_initializes_cluster_ca_before_any_node_joins() {
        let mut initialized_settings = temps_core::AppSettings::default();
        initialized_settings.multi_node.cluster_ca_cert_pem =
            Some("persisted-cluster-ca".to_string());
        initialized_settings.multi_node.cluster_ca_key_encrypted =
            Some("persisted-encrypted-key".to_string());
        let initialized_row = temps_entities::settings::Model {
            id: 1,
            data: initialized_settings.to_json(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // `ensure_cluster_ca` first reads settings, then locks and reads
            // the singleton row inside the initialization transaction. The
            // final result lets the assertion read what startup persisted.
            .append_query_results([
                Vec::<temps_entities::settings::Model>::new(),
                Vec::<temps_entities::settings::Model>::new(),
                vec![initialized_row.clone()],
                vec![initialized_row],
            ])
            .append_exec_results([MockExecResult {
                last_insert_id: 1,
                rows_affected: 1,
            }])
            .into_connection();
        let context = ServiceRegistrationContext::new();
        context.register_service(Arc::new(db));
        context.register_service(Arc::new(
            temps_core::EncryptionService::new(&"11".repeat(32))
                .expect("valid test encryption key"),
        ));

        let plugin = ConfigPlugin::new(Arc::new(
            ServerConfig::new(
                "127.0.0.1:8000".to_string(),
                "postgres://localhost/temps".to_string(),
                None,
                None,
            )
            .expect("valid test server config"),
        ));

        plugin
            .register_services(&context)
            .await
            .expect("control-plane startup should initialize cluster trust");

        let settings = context
            .require_service::<ConfigService>()
            .get_settings()
            .await
            .expect("initialized settings should remain readable");
        assert_eq!(
            settings.multi_node.cluster_ca_cert_pem.as_deref(),
            Some("persisted-cluster-ca")
        );
        assert_eq!(
            settings.multi_node.cluster_ca_key_encrypted.as_deref(),
            Some("persisted-encrypted-key")
        );
    }
}
