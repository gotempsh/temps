use std::sync::Arc;
use temps_core::AuditLogger;

use crate::services::{GatewayService, ProviderKeyService, UsageService};

pub struct AiGatewayAppState {
    pub gateway_service: Arc<GatewayService>,
    pub provider_key_service: Arc<ProviderKeyService>,
    pub usage_service: Arc<UsageService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

pub async fn create_ai_gateway_app_state(
    gateway_service: Arc<GatewayService>,
    provider_key_service: Arc<ProviderKeyService>,
    usage_service: Arc<UsageService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> Arc<AiGatewayAppState> {
    Arc::new(AiGatewayAppState {
        gateway_service,
        provider_key_service,
        usage_service,
        audit_service,
        telemetry,
    })
}
