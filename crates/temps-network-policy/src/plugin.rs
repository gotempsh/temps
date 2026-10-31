// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;
use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_workload_netpol::DnsPolicy;
use tracing::{debug, info, warn};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{router, NetworkPolicyApiDoc, NetworkPolicyAppState};
use crate::service::NetworkPolicyService;
use crate::source::spawn_control_plane_enforcer;

pub struct NetworkPolicyPlugin;

impl NetworkPolicyPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NetworkPolicyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the Docker daemon runs on this host (so nftables applied here
/// reach its bridges) rather than in a VM or on another machine.
#[allow(
    clippy::disallowed_methods,
    reason = "host lookup: DOCKER_HOST is the host's Docker client setting, not Temps configuration"
)]
fn daemon_is_local() -> bool {
    // Reading DOCKER_HOST is the Docker client's own bootstrap
    // configuration, not a Temps setting.
    std::env::var("DOCKER_HOST")
        .map(|h| h.is_empty() || h.starts_with("unix://"))
        .unwrap_or(true)
}

impl TempsPlugin for NetworkPolicyPlugin {
    fn name(&self) -> &'static str {
        "network-policy"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            context.register_service(Arc::new(NetworkPolicyService::new(db)));
            debug!("network-policy: services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let service = context.require_service::<NetworkPolicyService>();
            let workloads = context.require_service::<temps_core::LocalWorkloadPolicy>();
            let docker = context.require_service::<temps_core::DockerHandle>();
            // Shared with the proxy-owned resolver (`temps serve`).
            let dns = context.require_service::<ArcSwap<DnsPolicy>>();
            if !workloads.local_workloads_enabled() {
                info!(
                    profile = workloads.profile(),
                    "network policy: no local workloads in this profile, node 0 enforcer not started"
                );
                return Ok(());
            }
            let Some(client) = docker.cloned() else {
                warn!("network policy: Docker is unavailable, node 0 enforcer not started");
                return Ok(());
            };
            if let Err(e) = spawn_control_plane_enforcer(
                service,
                client.as_ref().clone(),
                daemon_is_local(),
                dns,
            ) {
                warn!(error = %e, "network policy: node 0 enforcer could not start");
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let state = Arc::new(NetworkPolicyAppState {
            service: context.require_service::<NetworkPolicyService>(),
            audit: context.require_service::<dyn temps_core::AuditLogger>(),
        });
        Some(PluginRoutes::new(router(state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<NetworkPolicyApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_stable() {
        assert_eq!(NetworkPolicyPlugin::new().name(), "network-policy");
    }
}
