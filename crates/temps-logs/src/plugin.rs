// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
use temps_core::LogStorageConfig;
use utoipa::openapi::OpenApi;

use crate::log_archive::LogArchiveStorage;
use crate::{DockerLogService, LogService, S3LogArchive};
use temps_file_store::s3_config::StatelessStorage;

/// Logs Plugin for file and Docker container logging
pub struct LogsPlugin {
    log_base_path: PathBuf,
    storage_config: LogStorageConfig,
}

impl LogsPlugin {
    /// `storage_config` selects where finished build/deploy job logs are
    /// archived once a job completes. This is the *same*
    /// `temps_core::LogStorageConfig` value built from
    /// `TEMPS_LOG_STORAGE_BACKEND` / `TEMPS_LOG_S3_*` that drives
    /// `LogAggregatorPlugin` -- one operator decision, two consumers. Pass
    /// `LogStorageConfig::Filesystem { .. }` to keep today's behavior
    /// (build/deploy logs stay on local disk forever, no archival).
    pub fn new(log_base_path: PathBuf, storage_config: LogStorageConfig) -> Self {
        Self {
            log_base_path,
            storage_config,
        }
    }

    /// Build the archive backend implied by `storage_config`. `None` for the
    /// `Filesystem` variant -- `temps-logs` itself always writes local files
    /// while a job is running, so "filesystem" for this plugin just means
    /// "no archival step", not a distinct backend implementation the way it
    /// is for `temps-log-aggregator`.
    fn build_archive(&self) -> Option<Arc<dyn LogArchiveStorage>> {
        match &self.storage_config {
            LogStorageConfig::Filesystem { .. } => None,
            LogStorageConfig::S3 {
                bucket,
                prefix,
                region,
                endpoint,
                access_key_id,
                secret_access_key,
                force_path_style,
            } => Some(Arc::new(S3LogArchive::new(
                bucket.clone(),
                prefix.clone(),
                region.clone(),
                endpoint.clone(),
                access_key_id.clone(),
                secret_access_key.clone(),
                *force_path_style,
            )) as Arc<dyn LogArchiveStorage>),
        }
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
            // Create LogService (file-based logging), with an optional S3
            // archive backend for finished build/deploy job logs.
            let archive = self.build_archive();
            if archive.is_some() {
                tracing::debug!(
                    "Build/deploy log archival to S3 enabled (TEMPS_LOG_STORAGE_BACKEND=s3)"
                );
            }
            let config_service = context.require_service::<temps_config::ConfigService>();
            let instance_id = config_service
                .stateless_instance_id()
                .await
                .map_err(|error| PluginError::PluginRegistrationFailed {
                    plugin_name: "logs".to_string(),
                    error: format!("Failed to read persisted installation mode: {error}"),
                })?;
            let durable_chunks = matches!(
                temps_file_store::s3_config::resolve_stateless_storage_for(instance_id.as_deref())
                    .map_err(|error| {
                        PluginError::PluginRegistrationFailed {
                            plugin_name: "logs".to_string(),
                            error: format!("Failed to resolve stateless log storage: {error}"),
                        }
                    })?,
                StatelessStorage::Enabled { .. }
            );
            let log_service = Arc::new(LogService::with_archive_mode(
                self.log_base_path.clone(),
                archive,
                durable_chunks,
            ));
            context.register_service(log_service);
            // DockerLogService is registered unconditionally — it holds an
            // Arc<DockerHandle> and returns a typed DockerUnavailable error
            // at point of use if the daemon is absent. File-based logs are
            // always unaffected. Container tailing simply returns a
            // DockerUnavailable on a control-plane process.
            let docker_handle = context.require_service::<temps_core::DockerHandle>();
            let docker_log_service = Arc::new(DockerLogService::new(docker_handle));
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

    fn filesystem_config() -> LogStorageConfig {
        LogStorageConfig::Filesystem {
            base_path: "/tmp/unused-by-logs-plugin".into(),
        }
    }

    #[test]
    fn test_logs_plugin_name() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(temp_dir.path().to_path_buf(), filesystem_config());
        assert_eq!(logs_plugin.name(), "logs");
    }

    #[test]
    fn test_logs_plugin_no_routes() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(temp_dir.path().to_path_buf(), filesystem_config());

        // Since we don't have a real context, we can't test configure_routes directly
        // but we can verify the plugin is created correctly
        assert_eq!(logs_plugin.name(), "logs");
    }

    #[test]
    fn test_logs_plugin_build_archive_none_for_filesystem() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(temp_dir.path().to_path_buf(), filesystem_config());
        assert!(logs_plugin.build_archive().is_none());
    }

    #[test]
    fn test_logs_plugin_build_archive_some_for_s3() {
        let temp_dir = TempDir::new().unwrap();
        let logs_plugin = LogsPlugin::new(
            temp_dir.path().to_path_buf(),
            LogStorageConfig::S3 {
                bucket: "test-bucket".to_string(),
                prefix: Some("logs/".to_string()),
                region: "us-east-1".to_string(),
                endpoint: Some("http://localhost:9000".to_string()),
                access_key_id: "id".to_string(),
                secret_access_key: "secret".to_string(),
                force_path_style: true,
            },
        );
        assert!(logs_plugin.build_archive().is_some());
    }
}
