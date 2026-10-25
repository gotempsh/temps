// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress connector plugin registration. Depends on the DNS plugin's
//! provider and managed-record services and on the shared route table.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_core::EncryptionService;
use temps_dns::{DnsProviderService, ManagedDnsRecordService};
use temps_routes::CachedPeerTable;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::handlers::{configure_routes, IngressApiDoc, IngressAppState};
use crate::service::IngressConnectorService;

#[derive(Default)]
pub struct IngressPlugin;

impl IngressPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl TempsPlugin for IngressPlugin {
    fn name(&self) -> &'static str {
        "ingress"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let service = Arc::new(IngressConnectorService::new(
                context.require_service::<DatabaseConnection>(),
                context.require_service::<EncryptionService>(),
                context.require_service::<DnsProviderService>(),
                context.require_service::<ManagedDnsRecordService>(),
                context.require_service::<CachedPeerTable>(),
            ));
            service.start_reconciler();
            context.register_service(service);
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let service = context.require_service::<IngressConnectorService>();
        // Registered by the proxy plugin, which loads after this one.
        if let Some(gate) = context.get_service::<temps_core::RequestPolicyGateSlot>() {
            service.set_request_policy(gate);
        }
        let state = Arc::new(IngressAppState {
            service,
            audit_service: context.require_service::<dyn temps_core::AuditLogger>(),
        });
        Some(PluginRoutes::new(configure_routes().with_state(state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<IngressApiDoc as OpenApiTrait>::openapi())
    }
}
