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

            // Create DockerRuntime service
            let docker_runtime = Arc::new(DockerRuntime::new(
                docker.clone(),
                use_buildkit,
                "temps-network".to_string(), // network_name
            ));

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
