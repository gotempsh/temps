// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use temps_core::AuditLogger;

use super::provider_status::AiProviderStatusCache;
use crate::services::{
    AiLimitsService, CodingAgentService, GatewayService, ModelCatalogService, NativeApiService,
    ProviderKeyService, ProviderModelService, ProviderPreferenceService, StructuredOutputService,
    UsageService,
};

pub struct AiGatewayAppState {
    pub db: Arc<sea_orm::DatabaseConnection>,
    pub gateway_service: Arc<GatewayService>,
    pub provider_key_service: Arc<ProviderKeyService>,
    pub provider_model_service: Arc<ProviderModelService>,
    pub provider_preference_service: Arc<ProviderPreferenceService>,
    pub model_catalog_service: Arc<ModelCatalogService>,
    pub coding_agent_service: Arc<CodingAgentService>,
    pub usage_service: Arc<UsageService>,
    pub limits_service: Arc<AiLimitsService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    pub provider_status_cache: Arc<AiProviderStatusCache>,
    pub structured_output_service: Arc<StructuredOutputService>,
    pub native_api_service: Arc<NativeApiService>,
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
    model_catalog_service: Arc<ModelCatalogService>,
    coding_agent_service: Arc<CodingAgentService>,
    usage_service: Arc<UsageService>,
    audit_service: Arc<dyn AuditLogger>,
    telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    structured_output_service: Arc<StructuredOutputService>,
    project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Arc<AiGatewayAppState> {
    let native_api_service = Arc::new(NativeApiService::new(
        db.clone(),
        gateway_service.clone(),
        usage_service.clone(),
    ));
    NativeApiService::start_reconciler(&native_api_service);
    let limits_service = Arc::new(AiLimitsService::new(db.clone(), usage_service.clone()));
    Arc::new(AiGatewayAppState {
        db,
        gateway_service,
        provider_key_service,
        provider_model_service,
        provider_preference_service,
        model_catalog_service,
        coding_agent_service,
        usage_service,
        limits_service,
        audit_service,
        telemetry,
        provider_status_cache: Arc::new(AiProviderStatusCache::default()),
        structured_output_service,
        native_api_service,
        project_access_checker,
    })
}

/// App state over `db` for handler tests: every service is real, nothing is
/// started in the background, and audit records go to `audit`.
#[cfg(test)]
pub(crate) fn test_app_state(
    db: Arc<sea_orm::DatabaseConnection>,
    audit: Arc<dyn AuditLogger>,
) -> Arc<AiGatewayAppState> {
    let encryption = Arc::new(
        temps_core::EncryptionService::new(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("test encryption key is valid"),
    );
    let provider_key_service = Arc::new(ProviderKeyService::new(db.clone(), encryption));
    let gateway_service = Arc::new(GatewayService::new(provider_key_service.clone()));
    let provider_model_service = Arc::new(ProviderModelService::new(
        db.clone(),
        provider_key_service.clone(),
        gateway_service.clone(),
    ));
    let model_catalog_service = Arc::new(ModelCatalogService::new(
        db.clone(),
        provider_key_service.clone(),
        provider_model_service.clone(),
    ));
    let usage_service = Arc::new(UsageService::new(db.clone()));
    let ai: Arc<dyn temps_ai::AiService> = Arc::new(crate::services::GatewayAiService::new(
        gateway_service.clone(),
        db.clone(),
    ));
    let coding_agent_service = Arc::new(CodingAgentService::new(
        db.clone(),
        ai.clone(),
        audit.clone(),
    ));
    Arc::new(AiGatewayAppState {
        db: db.clone(),
        gateway_service: gateway_service.clone(),
        provider_key_service,
        provider_model_service,
        provider_preference_service: Arc::new(ProviderPreferenceService::new(db.clone())),
        model_catalog_service,
        coding_agent_service,
        usage_service: usage_service.clone(),
        limits_service: Arc::new(AiLimitsService::new(db.clone(), usage_service.clone())),
        audit_service: audit,
        telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
        provider_status_cache: Arc::new(AiProviderStatusCache::default()),
        structured_output_service: Arc::new(StructuredOutputService::new(
            ai,
            usage_service.clone(),
        )),
        native_api_service: Arc::new(NativeApiService::new(db, gateway_service, usage_service)),
        project_access_checker: None,
    })
}
