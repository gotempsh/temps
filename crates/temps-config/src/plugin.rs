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

use crate::{ConfigService, ServerConfig, configure_routes, SettingsApiDoc};
use crate::handler::SettingsState;

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
            let config_service = Arc::new(ConfigService::new(
                self.server_config.clone(),
                db.clone(),
            ));
            context.register_service(config_service);

            tracing::debug!("Config plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the ConfigService from the context
        let config_service = context.require_service::<ConfigService>();

        // Create SettingsState
        let settings_state = Arc::new(SettingsState {
            config_service,
        });

        // Configure routes with the state
        let routes = configure_routes().with_state(settings_state);

        Some(PluginRoutes {
            router: routes,
        })
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(SettingsApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_config_plugin_name() {
        let server_config = Arc::new(ServerConfig::new("127.0.0.1:8000".to_string(), "sqlite:temps.db".to_string(), None, None).unwrap());
        let config_plugin = ConfigPlugin::new(server_config);
        assert_eq!(config_plugin.name(), "config");
    }
}