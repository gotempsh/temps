//! Static Files Plugin
//!
//! Plugin for serving static files from the configured static directory

use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext,
};
use utoipa::OpenApi;

use crate::{handler, service::FileService};

pub struct StaticFilesPlugin {}

impl StaticFilesPlugin {
    pub fn new() -> Self {
        Self {}
    }
}

impl temps_core::plugin::TempsPlugin for StaticFilesPlugin {
    fn name(&self) -> &'static str {
        "static-files"
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
            let config_service = context.require_service::<ConfigService>();
            let file_service = Arc::new(FileService::new(config_service.clone()));
            context.register_service(file_service);
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let file_service = context.get_service::<FileService>()?;
        Some(PluginRoutes {
            router: handler::configure_routes(file_service),
        })
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(handler::FileApiDoc::openapi())
    }

    fn configure_middleware(
        &self,
        _context: &PluginContext,
    ) -> Option<temps_core::plugin::PluginMiddlewareCollection> {
        None
    }
}
