use crate::{
    apikey_service::ApiKeyService, auth_service::AuthService,
    deployment_token_service::DeploymentTokenValidationService, user_service::UserService,
};
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use temps_core::notifications::DynNotificationService;
use temps_core::{CookieCrypto, EncryptionService};

/// Application state containing all authentication services for Axum
#[derive(Clone)]
pub struct AuthState {
    /// Database connection
    pub db: Arc<DatabaseConnection>,
    /// Authentication service
    pub auth_service: Arc<AuthService>,
    /// Encryption service
    pub encryption_service: Arc<EncryptionService>,
    /// Audit service
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Api
    pub api_key_service: Arc<ApiKeyService>,
    /// User service
    pub user_service: Arc<UserService>,
    /// Cookie crypto service
    pub cookie_crypto: Arc<CookieCrypto>,
    /// Deployment token validation service
    pub deployment_token_service: Arc<DeploymentTokenValidationService>,
    /// OIDC SSO service
    pub oidc_service: Arc<crate::oidc_service::OidcService>,
    /// Anonymous product telemetry reporter
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

impl AuthState {
    /// Create new AuthState with all services
    pub fn new(
        db: Arc<DatabaseConnection>,
        audit_service: Arc<dyn temps_core::AuditLogger>,
        encryption_service: Arc<EncryptionService>,
        cookie_crypto: Arc<CookieCrypto>,
        notification_service: DynNotificationService,
        telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
    ) -> Self {
        let auth_service = Arc::new(AuthService::new(db.clone(), notification_service));
        let api_key_service = Arc::new(ApiKeyService::new(db.clone()));
        let user_service = Arc::new(UserService::new(db.clone()));
        let deployment_token_service = Arc::new(DeploymentTokenValidationService::new(db.clone()));
        let oidc_service = Arc::new(crate::oidc_service::OidcService::new(
            db.clone(),
            encryption_service.clone(),
            user_service.clone(),
        ));
        Self {
            db,
            auth_service,
            audit_service,
            api_key_service,
            encryption_service,
            user_service,
            cookie_crypto,
            deployment_token_service,
            oidc_service,
            telemetry,
        }
    }
}
