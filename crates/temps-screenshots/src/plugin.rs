//! Screenshots Plugin
//!
//! Plugin for capturing screenshots of deployed applications

use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::plugin::{PluginContext, PluginError, ServiceRegistrationContext};
use tracing::{debug, info};

use crate::service::ScreenshotService;

pub struct ScreenshotsPlugin {}

impl ScreenshotsPlugin {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ScreenshotsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl temps_core::plugin::TempsPlugin for ScreenshotsPlugin {
    fn name(&self) -> &'static str {
        "screenshots"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> std::pin::Pin<
        Box<
            dyn std::prelude::rust_2024::Future<
                    Output = temps_core::anyhow::Result<(), PluginError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            debug!("Registering screenshot service");

            let config_service = context.require_service::<ConfigService>();

            // Check if screenshots are enabled
            let screenshots_enabled = config_service
                .get_settings()
                .await
                .ok()
                .map(|s| s.screenshots.enabled)
                .unwrap_or(false);

            if !screenshots_enabled {
                info!("Screenshots are disabled in configuration");
                return Ok(());
            }

            let screenshot_service = ScreenshotService::new(config_service.clone())
                .await
                .map_err(|e| {
                    PluginError::PluginRegistrationFailed {
                        plugin_name: "screenshots".to_string(),
                        error: format!("Failed to create screenshot service: {}", e),
                    }
                })?;

            info!(
                "Screenshot service registered with provider: {}",
                screenshot_service.provider_name()
            );

            context.register_service(Arc::new(screenshot_service));
            Ok(())
        })
    }

    fn configure_routes(&self, _context: &PluginContext) -> Option<temps_core::plugin::PluginRoutes> {
        // Screenshots don't need HTTP routes - they're used internally by jobs
        None
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        None
    }

    fn configure_middleware(
        &self,
        _context: &PluginContext,
    ) -> Option<temps_core::plugin::PluginMiddlewareCollection> {
        None
    }
}
