// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Infrastructure Plugin implementation for the Temps plugin system
//!
//! This plugin provides infrastructure and platform information functionality including:
//! - PlatformInfoService for platform detection and network information
//! - Infrastructure diagnostics routes (platform info, IP addresses, access mode)
//! - Infrastructure health monitoring endpoints

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    routes::{configure_routes, DnsApiDoc, DnsAppState, InfraAppState, PlatformInfoApiDoc},
    services::{DnsService, PlatformInfoService},
};

/// State container for infrastructure plugin that implements InfraAppState and DnsAppState
#[derive(Clone)]
pub struct InfraState {
    platform_info_service: Arc<PlatformInfoService>,
    dns_service: Arc<DnsService>,
}

impl InfraState {
    pub fn new(
        platform_info_service: Arc<PlatformInfoService>,
        dns_service: Arc<DnsService>,
    ) -> Self {
        Self {
            platform_info_service,
            dns_service,
        }
    }
}

impl InfraAppState for InfraState {
    fn platform_info_service(&self) -> &PlatformInfoService {
        &self.platform_info_service
    }
}

impl DnsAppState for InfraState {
    fn dns_service(&self) -> &DnsService {
        &self.dns_service
    }
}

/// Infrastructure Plugin for managing platform information and network diagnostics
pub struct InfraPlugin;

impl InfraPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InfraPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for InfraPlugin {
    fn name(&self) -> &'static str {
        "infra"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Docker is used for exactly one thing here: reading the
            // daemon's os/arch. `get_service`, not `require_service` — a
            // control plane with no daemon still needs every other platform
            // endpoint, and the capabilities endpoint below is precisely how a
            // client learns the daemon is absent.
            let docker = context.get_service::<bollard::Docker>();
            let docker_available = docker.is_some();

            // The serve bootstrap is the only place that knows the profile;
            // absent (embedded/test contexts) means the historical
            // everything-enabled control plane.
            let policy = temps_core::policy_or_default(
                context.get_service::<temps_core::LocalWorkloadPolicy>(),
            );
            let features = crate::types::PlatformFeatures {
                profile: policy.profile().to_string(),
                deployments_local: policy.local_workloads_enabled(),
                managed_services: policy.local_workloads_enabled(),
                backups_local: policy.local_workloads_enabled(),
                sandboxes: policy.local_workloads_enabled(),
                docker: docker_available && policy.docker_available(),
            };

            // Create PlatformInfoService
            let platform_info_service = Arc::new(
                match docker {
                    Some(docker) => PlatformInfoService::new(docker),
                    None => PlatformInfoService::without_docker(),
                }
                .with_features(features),
            );
            context.register_service(platform_info_service.clone());

            // Create DnsService
            let dns_service = Arc::new(DnsService::new());
            context.register_service(dns_service.clone());

            // Create InfraState for handlers
            let infra_state = Arc::new(InfraState::new(platform_info_service, dns_service));
            context.register_service(infra_state);

            tracing::debug!("Infrastructure plugin services registered successfully");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Get the InfraState
        let infra_state = context.require_service::<InfraState>();

        // Configure infrastructure routes
        let infra_routes = configure_routes::<InfraState>().with_state(infra_state);

        Some(PluginRoutes::new(infra_routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut platform_api = <PlatformInfoApiDoc as OpenApiTrait>::openapi();
        let dns_api = <DnsApiDoc as OpenApiTrait>::openapi();

        // Merge DNS API paths into platform API
        platform_api.paths.paths.extend(dns_api.paths.paths);

        // Merge DNS API components into platform API
        if let Some(dns_components) = dns_api.components {
            if let Some(ref mut platform_components) = platform_api.components {
                // Merge schemas
                platform_components.schemas.extend(dns_components.schemas);
            } else {
                platform_api.components = Some(dns_components);
            }
        }

        Some(platform_api)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_infra_plugin_name() {
        let infra_plugin = InfraPlugin::new();
        assert_eq!(infra_plugin.name(), "infra");
    }

    #[tokio::test]
    async fn test_infra_plugin_default() {
        let infra_plugin = InfraPlugin;
        assert_eq!(infra_plugin.name(), "infra");
    }
}
