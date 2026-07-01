use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::openapi::OpenApi;

use crate::{
    docker::DockerRuntime,
    static_deployer::{FilesystemStaticDeployer, StaticDeployer},
    ContainerDeployer,
};

/// Deployer Plugin for managing container deployment operations
pub struct DeployerPlugin;

impl DeployerPlugin {
    pub fn new() -> Self {
        Self
    }

    /// Detect if Docker BuildKit is available by checking daemon version and capabilities
    async fn detect_buildkit() -> bool {
        match bollard::Docker::connect_with_defaults() {
            Ok(docker) => {
                // Check Docker version
                match docker.version().await {
                    Ok(version) => {
                        // BuildKit is available in Docker Engine 18.09+
                        if let Some(version_str) = version.version {
                            tracing::debug!("Docker version: {}", version_str);

                            // Parse version and check if >= 18.09
                            if let Some(major_minor) =
                                version_str.split('.').take(2).collect::<Vec<_>>().get(0..2)
                            {
                                if let (Ok(major), Ok(minor)) =
                                    (major_minor[0].parse::<u32>(), major_minor[1].parse::<u32>())
                                {
                                    let supports_buildkit =
                                        major > 18 || (major == 18 && minor >= 9);

                                    if !supports_buildkit {
                                        tracing::warn!(
                                            "Docker {}.{} does not support BuildKit (requires 18.09+)",
                                            major, minor
                                        );
                                        return false;
                                    }

                                    tracing::debug!("Docker {}.{} supports BuildKit", major, minor);
                                }
                            }
                        }

                        // Check Docker info for BuildKit support
                        match docker.info().await {
                            Ok(info) => {
                                // Log out all info for debug
                                tracing::debug!(
                                    "Docker info arch: {:?} os: {:?}",
                                    info.architecture,
                                    info.os_type
                                );
                                // Check if BuildKit is explicitly disabled
                                // Note: BuildKit is enabled by default in newer Docker versions
                                tracing::debug!("Docker info retrieved successfully");

                                // Modern Docker (20.10+) has BuildKit enabled by default
                                tracing::debug!("BuildKit available and will be used for builds");
                                true
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "Failed to get Docker info: {}, assuming BuildKit available",
                                    e
                                );
                                true // Assume available if we can't check
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get Docker version: {}", e);
                        false
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to connect to Docker: {}", e);
                false
            }
        }
    }
}

impl Default for DeployerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for DeployerPlugin {
    fn name(&self) -> &'static str {
        "deployer"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Create Docker client
            let docker = context.require_service::<bollard::Docker>();

            // Check if buildkit is available
            let use_buildkit = Self::detect_buildkit().await;
            tracing::debug!("Using buildkit: {}", use_buildkit);

            // Load build limits from settings. Only the control plane has
            // a ConfigService registered (workers run DockerRuntime through
            // `temps cli agent` which never reaches this plugin), so this
            // is naturally scoped to control-plane builds. On the rare
            // path where settings can't be read (fresh install before
            // first save, DB hiccup), we log a warning and fall back to
            // the legacy unbounded behaviour rather than fail plugin
            // startup — builds must keep working.
            let config_service = context.require_service::<temps_config::ConfigService>();
            let build_limits = match config_service.get_settings().await {
                Ok(settings) => Some(settings.build_limits),
                Err(e) => {
                    tracing::warn!(
                        "Could not read build_limits from settings ({}). \
                         Builds will run with the legacy unbounded behaviour \
                         until settings are saved.",
                        e
                    );
                    None
                }
            };

            // Create DockerRuntime service
            let mut docker_runtime = DockerRuntime::new(
                docker.clone(),
                use_buildkit,
                temps_core::NETWORK_NAME.to_string(),
            );
            if let Some(limits) = build_limits {
                let resource_caps = if limits.cpu_limit_cores > 0.0 && limits.memory_limit_mb > 0 {
                    Some(crate::docker::BuildResourceLimits {
                        cpu_cores: limits.cpu_limit_cores,
                        memory_mb: limits.memory_limit_mb,
                    })
                } else {
                    None
                };
                docker_runtime =
                    docker_runtime.with_build_limits(limits.max_concurrent, resource_caps);
                tracing::info!(
                    "DockerRuntime: build concurrency={}, per-build cpu={} cores, mem={} MB \
                     (0 = legacy 50%-of-host heuristic)",
                    limits.max_concurrent,
                    limits.cpu_limit_cores,
                    limits.memory_limit_mb
                );
            }

            // ADR-024: start the control-plane DNS resolver so containers
            // deployed locally on the control plane — and every single-node
            // install — can resolve `*.temps.local`. This plugin only runs on
            // the control plane (workers build DockerRuntime via `temps agent`),
            // so it is the right home. Best-effort: any failure (Docker down,
            // :53 already bound, no gateway) leaves containers on Docker's
            // embedded DNS, exactly as before this change.
            //
            // `get_service` (not `require_service`) is deliberate: the DB is an
            // optional dependency for this best-effort enhancement. A missing
            // DB (e.g. an embedded/test configuration) must skip DNS startup,
            // never fail the deployer plugin — do not promote this to
            // `require_service`.
            if let Some(db) = context.get_service::<sea_orm::DatabaseConnection>() {
                match docker_runtime.ensure_network_exists().await {
                    Ok(()) => match docker_runtime.inspect_app_network_gateway().await {
                        Some(gateway) => {
                            let snapshot_dir =
                                config_service.get_server_config().data_dir.join("dns");
                            if let Some(slot) =
                                temps_dns::start_control_plane_resolver(db, gateway, snapshot_dir)
                                    .await
                            {
                                docker_runtime = docker_runtime.with_overlay_dns_slot(slot);
                            }
                        }
                        None => tracing::warn!(
                            "app-network gateway not found; control-plane DNS resolver not started"
                        ),
                    },
                    Err(e) => tracing::warn!(
                        error = %e,
                        "could not ensure app network; control-plane DNS resolver not started"
                    ),
                }
            }

            let docker_runtime = Arc::new(docker_runtime);

            // Register the concrete service
            context.register_service(docker_runtime.clone());

            // Register as ContainerDeployer trait
            let container_deployer: Arc<dyn ContainerDeployer> = docker_runtime.clone();
            context.register_service(container_deployer);

            // Register as ImageBuilder trait
            let image_builder: Arc<dyn crate::ImageBuilder> = docker_runtime;
            context.register_service(image_builder);

            // Create and register StaticDeployer
            let config_service = context.require_service::<temps_config::ConfigService>();
            let static_files_dir = config_service.get_server_config().data_dir.join("static");
            let filesystem_static_deployer =
                Arc::new(FilesystemStaticDeployer::new(static_files_dir));
            let static_deployer: Arc<dyn StaticDeployer> = filesystem_static_deployer;
            context.register_service(static_deployer);

            tracing::debug!("Deployer plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, _context: &PluginContext) -> Option<PluginRoutes> {
        // Note: temps-deployer doesn't currently have HTTP handlers/routes
        // If routes are needed in the future, they can be added here
        None
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        // Note: temps-deployer doesn't currently have HTTP endpoints
        // If API endpoints are added in the future, OpenAPI schema can be returned here
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_deployer_plugin_name() {
        let deployer_plugin = DeployerPlugin::new();
        assert_eq!(deployer_plugin.name(), "deployer");
    }

    #[tokio::test]
    async fn test_deployer_plugin_default() {
        let deployer_plugin = DeployerPlugin;
        assert_eq!(deployer_plugin.name(), "deployer");
    }
}
