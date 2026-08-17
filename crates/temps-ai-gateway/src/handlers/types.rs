use std::sync::Arc;
use temps_core::AuditLogger;

use super::provider_status::AiProviderStatusCache;
use crate::services::{
    GatewayService, GovernanceService, ProviderKeyService, ProviderModelService,
    ProviderPreferenceService, StructuredOutputService, UsageService,
};

pub struct AiGatewayAppState {
    pub db: Arc<sea_orm::DatabaseConnection>,
    pub gateway_service: Arc<GatewayService>,
    pub provider_key_service: Arc<ProviderKeyService>,
    pub provider_model_service: Arc<ProviderModelService>,
    pub provider_preference_service: Arc<ProviderPreferenceService>,
    pub usage_service: Arc<UsageService>,
    pub governance_service: Arc<GovernanceService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    pub provider_status_cache: Arc<AiProviderStatusCache>,
    pub structured_output_service: Arc<StructuredOutputService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

// This is the plugin composition root: keeping dependencies explicit makes
// the shared app state auditable and avoids a second opaque service locator.
#[allow(clippy::too_many_arguments)]
pub async fn create_ai_gateway_app_state(
    db: Arc<sea_orm::DatabaseConnection>,
    gateway_service: Arc<GatewayService>,
    provider_key_service: Arc<ProviderKeyService>,
    provider_model_service: Arc<ProviderModelService>,
    provider_preference_service: Arc<ProviderPreferenceService>,
    usage_service: Arc<UsageService>,
    governance_service: Arc<GovernanceService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    structured_output_service: Arc<StructuredOutputService>,
    project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Arc<AiGatewayAppState> {
    Arc::new(AiGatewayAppState {
        db,
        gateway_service,
        provider_key_service,
        provider_model_service,
        provider_preference_service,
        usage_service,
        governance_service,
        audit_service,
        telemetry,
        provider_status_cache: Arc::new(AiProviderStatusCache::default()),
        structured_output_service,
        project_access_checker,
    })
}
