// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Registers the recording service and the node-0 recorder, once, for
//! every workload kind.
//!
//! It registers after the OTel and network-policy plugins, whose services
//! it builds on, and before no one: the workload kinds (sandboxes,
//! environments) take [`EgressRecordingService`] and
//! [`LocalEgressRecorder`] in their second phase, where they register
//! their adapter and, when their workloads run on this host, their traffic
//! source.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_core::plugin::{PluginError, ServiceRegistrationContext, TempsPlugin};
use temps_network_policy::NodeRecordingStatus;
use temps_workload_netpol::EgressRecordingCapability;
use tracing::warn;

use crate::local::{LocalCapability, LocalEgressRecorder, LocalServing};
use crate::service::{EgressLogStore, EgressRecordingService, NodeRecordingCapabilities};

/// Capabilities when no network-policy service runs: nobody has reported.
struct NoNodeReports;

impl NodeRecordingCapabilities for NoNodeReports {
    fn status(&self, _node_id: i32) -> NodeRecordingStatus {
        NodeRecordingStatus::NotReporting {
            within_secs: temps_network_policy::service::ENFORCER_REPORT_TTL.as_secs(),
        }
    }
}

impl LocalCapability for NoNodeReports {
    fn set(&self, _capability: EgressRecordingCapability) {}
}

pub struct WorkloadEgressPlugin;

impl WorkloadEgressPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkloadEgressPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for WorkloadEgressPlugin {
    fn name(&self) -> &'static str {
        "workload-egress"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            // Without the network-policy service every node reads as not
            // reporting; without OTel, recording says its storage is not
            // enabled. Neither stops the server.
            let policy = context.get_service::<temps_network_policy::NetworkPolicyService>();
            if policy.is_none() {
                warn!(
                    "workload egress: no network policy service; egress recording reports every \
                     node as not reporting"
                );
            }
            let store = context
                .get_service::<temps_otel::OtelService>()
                .map(|otel| otel as Arc<dyn EgressLogStore>);
            if store.is_none() {
                warn!("workload egress: no OTel service; egress recording is unavailable");
            }
            let capabilities: Arc<dyn NodeRecordingCapabilities> = match &policy {
                Some(policy) => policy.clone(),
                None => Arc::new(NoNodeReports),
            };
            let local: Arc<dyn LocalCapability> = match policy {
                Some(policy) => policy,
                None => Arc::new(NoNodeReports),
            };
            let service = Arc::new(EgressRecordingService::new(db, capabilities, store));
            context.register_service(service.clone());
            // The recorder says which kinds it serves here; the service
            // reports node 0 able to record only those.
            let serving = LocalServing::new();
            service.set_local_serving(serving.clone());
            let installation = crate::steering::InstallationId::derive(
                &context.require_service::<temps_core::EncryptionService>(),
            );
            context.register_service(LocalEgressRecorder::new(
                service,
                local,
                serving,
                installation,
            ));
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_is_named_for_what_it_shares() {
        assert_eq!(WorkloadEgressPlugin::new().name(), "workload-egress");
    }
}
