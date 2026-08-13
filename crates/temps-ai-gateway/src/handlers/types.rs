use std::sync::Arc;
use temps_core::AuditLogger;

use super::provider_status::AiProviderStatusCache;
use crate::services::{
    GatewayService, ProviderKeyService, ProviderPreferenceService, UsageService,
};

pub struct AiGatewayAppState {
    pub gateway_service: Arc<GatewayService>,
    pub provider_key_service: Arc<ProviderKeyService>,
    pub provider_preference_service: Arc<ProviderPreferenceService>,
    pub usage_service: Arc<UsageService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    pub provider_status_cache: Arc<AiProviderStatusCache>,
}

pub async fn create_ai_gateway_app_state(
    gateway_service: Arc<GatewayService>,
    provider_key_service: Arc<ProviderKeyService>,
    provider_preference_service: Arc<ProviderPreferenceService>,
    usage_service: Arc<UsageService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
) -> Arc<AiGatewayAppState> {
    Arc::new(AiGatewayAppState {
        gateway_service,
        provider_key_service,
        provider_preference_service,
        usage_service,
        audit_service,
        telemetry,
        provider_status_cache: Arc::new(AiProviderStatusCache::default()),
    })
}
