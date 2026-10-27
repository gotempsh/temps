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
use crate::reachability::access_handlers::{
    configure_routes as access_routes, AccessApiDoc, AccessAppState,
};
use crate::reachability::access_service::AccessProviderService;
use crate::reachability::address_handlers::{
    configure_routes as address_routes, AddressApiDoc, AddressAppState,
};
use crate::reachability::gate_handlers::{
    configure_routes as gate_routes, GateApiDoc, GateConsoleState,
};
use crate::reachability::handlers::{
    configure_routes as reachability_routes, ReachabilityApiDoc, ReachabilityAppState,
};
use crate::reachability::{
    EnvironmentAddressService, EnvironmentHostService, ReachabilityDeps, ReachabilityService,
    WaysIn,
};
use crate::service::IngressConnectorService;
use temps_config::ConfigService;

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
            // ADR 064 D9: how visitors reach each address, for the proxy's
            // IP access rules.
            let ways = Arc::new(WaysIn {
                db: context.require_service::<DatabaseConnection>(),
                connectors: service.clone(),
                config: context.require_service::<ConfigService>(),
            });
            // ...and the tunnel side of the same rule: no hostname is moved
            // onto a tunnel past a private-only allow list.
            service.set_move_guard(ways.clone());
            let ways_in: Arc<dyn temps_core::ways_in::EnvironmentWaysIn> = ways;
            context.register_service(ways_in);
            context.register_service(service);
            // ADR 065 D9: saved Cloudflare Access tokens. The server
            // registers the key service, which refreshes their signing
            // keys, before plugins register theirs.
            let access_http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| PluginError::PluginRegistrationFailed {
                    plugin_name: "ingress".to_string(),
                    error: format!("could not build the Cloudflare Access HTTP client: {e}"),
                })?;
            context.register_service(Arc::new(AccessProviderService::new(
                context.require_service::<DatabaseConnection>(),
                context.require_service::<EncryptionService>(),
                context.require_service::<temps_routes::access_keys::AccessKeyService>(),
                access_http,
            )));
            // Filled in `configure_routes`, once the projects plugin has
            // registered what reachability reads (ADR 068 D4).
            context.register_service(Arc::new(
                temps_core::environment_address::EnvironmentAddressSlot::new(),
            ));
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let service = context.require_service::<IngressConnectorService>();
        // Registered by the proxy plugin, which loads after this one.
        if let Some(gate) = context.get_service::<temps_core::RequestPolicyGateSlot>() {
            service.set_request_policy(gate);
        }
        let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
        // The projects plugin registers custom domains after this plugin's
        // services, so reachability is assembled here.
        let reachability = Arc::new(ReachabilityService::new(ReachabilityDeps {
            db: context.require_service::<DatabaseConnection>(),
            connectors: service.clone(),
            dns_providers: context.require_service::<DnsProviderService>(),
            records: context.require_service::<ManagedDnsRecordService>(),
            custom_domains: context.require_service::<temps_projects::CustomDomainService>(),
            domains: context.require_service::<temps_domains::DomainService>(),
            config: context.require_service::<ConfigService>(),
            access: context.require_service::<AccessProviderService>(),
        }));
        // Runs a restart left behind are reconciled now and every minute.
        reachability.clone().start_reconciler();
        let addresses = Arc::new(EnvironmentAddressService::new(
            reachability.clone(),
            context.require_service::<dyn temps_core::notifications::NotificationService>(),
        ));
        context
            .require_service::<temps_core::environment_address::EnvironmentAddressSlot>()
            .set(addresses.clone());
        // ADR 068 D3: the reconciler keeps tunnel environment hosts in line
        // with the environments, before every reconcile.
        let environment_hosts = Arc::new(EnvironmentHostService::new(
            reachability.clone(),
            addresses.clone(),
        ));
        service.set_environment_host_sync(environment_hosts.clone());
        let address_state = Arc::new(AddressAppState {
            service: addresses,
            environment_hosts,
            audit_service: audit_service.clone(),
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
        });
        let reachability_state = Arc::new(ReachabilityAppState {
            service: reachability,
            audit_service: audit_service.clone(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
        });
        // Signed-in addresses (ADR 065 D8); the server registers the gate
        // service before plugins configure their routes.
        let gate_state = Arc::new(GateConsoleState {
            gate: context.require_service::<temps_routes::gate::GateService>(),
            audit_service: audit_service.clone(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
        });
        let access_state = Arc::new(AccessAppState {
            service: context.require_service::<AccessProviderService>(),
            audit_service: audit_service.clone(),
        });
        let state = Arc::new(IngressAppState {
            service,
            audit_service,
        });
        Some(PluginRoutes::new(
            configure_routes()
                .with_state(state)
                .merge(reachability_routes().with_state(reachability_state))
                .merge(gate_routes().with_state(gate_state))
                .merge(access_routes().with_state(access_state))
                .merge(address_routes().with_state(address_state)),
        ))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut doc = <IngressApiDoc as OpenApiTrait>::openapi();
        doc.merge(<ReachabilityApiDoc as OpenApiTrait>::openapi());
        doc.merge(<GateApiDoc as OpenApiTrait>::openapi());
        doc.merge(<AccessApiDoc as OpenApiTrait>::openapi());
        doc.merge(<AddressApiDoc as OpenApiTrait>::openapi());
        Some(doc)
    }
}
