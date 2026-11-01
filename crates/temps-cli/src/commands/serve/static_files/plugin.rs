// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Static Files Plugin
//!
//! Plugin for serving static files from the configured static directory

use std::sync::Arc;
use temps_config::ConfigService;
use temps_core::plugin::{PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext};
use utoipa::OpenApi;

use super::{handler, service::FileService};

pub(crate) struct StaticFilesPlugin {}

impl Default for StaticFilesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl StaticFilesPlugin {
    pub(crate) fn new() -> Self {
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
            let instance_id = config_service
                .stateless_instance_id()
                .await
                .map_err(|error| {
                    PluginError::InitializationFailed(format!(
                        "Could not read persisted installation mode: {error}"
                    ))
                })?;
            let stateless =
                temps_file_store::s3_config::resolve_stateless_storage_for(instance_id.as_deref())
                    .map_err(|error| PluginError::InitializationFailed(error.to_string()))?;
            let file_service = Arc::new(
                FileService::from_config(config_service.clone(), &stateless).map_err(|error| {
                    PluginError::PluginRegistrationFailed {
                        plugin_name: "static-files".to_string(),
                        error: format!("Failed to configure static file storage: {error}"),
                    }
                })?,
            );
            context.register_service(file_service);
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let file_service = context.get_service::<FileService>()?;
        Some(PluginRoutes::new(handler::configure_routes(file_service)))
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

#[cfg(test)]
mod tests {
    use temps_core::plugin::TempsPlugin;

    use super::*;

    #[test]
    fn plugin_documents_the_files_route() {
        let plugin = StaticFilesPlugin::new();

        assert_eq!(plugin.name(), "static-files");
        let schema = plugin.openapi_schema().expect("plugin publishes a schema");
        assert!(
            schema.paths.paths.contains_key("/files/{file_path}"),
            "{:?}",
            schema.paths.paths.keys().collect::<Vec<_>>()
        );
    }
}
