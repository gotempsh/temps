//! Logs Plugin implementation for the Temps plugin system
//!
//! This plugin provides logging services including:
//! - File-based logging service
//! - Docker container logging service
//! - Log management and organization

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;

use crate::{DockerLogService, LogService};

/// Logs Plugin for file and Docker container logging
pub struct LogsPlugin {
    log_base_path: PathBuf,
}

impl LogsPlugin {
    pub fn new(log_base_path: PathBuf) -> Self {
        Self { log_base_path }
    }
}

impl TempsPlugin for LogsPlugin {
    fn name(&self) -> &'static str {
        "logs"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Create LogService (file-based logging)
            let log_service = Arc::new(LogService::new(self.log_base_path.clone()));
            context.register_service(log_service);
            let docker = context.require_service::<bollard::Docker>();
            // Create DockerLogService
            let docker_log_service = Arc::new(DockerLogService::new(docker));
            context.register_service(docker_log_service);
            tracing::debug!("Docker log service registered successfully");

            tracing::debug!("Logs plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        // Logs plugin is service-only, no HTTP routes
        None
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Logs plugin is service-only, no API endpoints
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_logs_plugin_name() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(temp_dir.path().to_path_buf());
        assert_eq!(logs_plugin.name(), "logs");
    }

    #[test]
    fn test_logs_plugin_no_routes() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(temp_dir.path().to_path_buf());

        // Since we don't have a real context, we can't test configure_routes directly
        // but we can verify the plugin is created correctly
        assert_eq!(logs_plugin.name(), "logs");
    }
}
