use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseBackend, MockDatabase};
use temps_auth::{AuthContext, Permission};
use temps_core::{AuditLogger, AuditOperation, NoopTelemetryReporter, RequestMetadata};
use temps_domains::tls::{
    ChallengeData, ChallengeType, ProviderError, ProvisioningResult, ValidationResult,
};
use temps_domains::{
    Certificate, CertificateProvider, CertificateRepository, DefaultCertificateRepository,
    DomainAppState, DomainService, TlsService,
};
use tower::ServiceExt;

struct UnusedCertificateProvider;

#[async_trait]
impl CertificateProvider for UnusedCertificateProvider {
    async fn provision(
        &self,
        _domain: &str,
        _challenge: ChallengeType,
        _email: &str,
    ) -> Result<ProvisioningResult, ProviderError> {
        panic!("permission/inactive-provider tests must not provision a certificate")
    }

    async fn complete_challenge(
        &self,
        _domain: &str,
        _challenge_data: &ChallengeData,
        _email: &str,
    ) -> Result<Certificate, ProviderError> {
        panic!("permission/inactive-provider tests must not finalize a certificate")
    }

    fn supported_challenges(&self) -> Vec<ChallengeType> {
        vec![ChallengeType::Dns01]
    }

    async fn validate_prerequisites(
        &self,
        _domain: &str,
        _email: &str,
    ) -> Result<ValidationResult, ProviderError> {
        Ok(ValidationResult {
            is_valid: true,
            errors: vec![],
            warnings: vec![],
        })
    }

    async fn cancel_order(&self, _domain: &str) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingAudit {
    operations: Mutex<Vec<String>>,
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
        self.operations
            .lock()
            .unwrap()
            .push(operation.operation_type());
        Ok(())
    }
}

/// Test double for handlers unrelated to sensitive-action gating -- always
/// allows, so these permission/routing tests don't need to know about it.
struct AllowAllSensitiveActions;

#[async_trait]
impl temps_core::SensitiveActionAuthorizer for AllowAllSensitiveActions {
    async fn authorize(
        &self,
        _action: &temps_core::SensitiveAction,
        _principal: &temps_core::SensitiveActionPrincipal,
    ) -> Result<temps_core::SensitiveActionDecision, temps_core::SensitiveActionAuthorizationError>
    {
        Ok(temps_core::SensitiveActionDecision::Allow)
    }
}

fn test_user() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 42,
        name: "Domain operator".to_string(),
        email: "domains@example.com".to_string(),
        password_hash: None,
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        must_change_password: false,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        oidc_subject: None,
        oidc_provider_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn auth(permissions: Vec<Permission>) -> AuthContext {
    AuthContext::new_api_key(
        test_user(),
        None,
        Some(permissions),
        "domain-governance-test".to_string(),
        1,
    )
}

fn metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "domain-governance-router-test".to_string(),
        headers: Default::default(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

fn permission_router() -> axum::Router {
    // Empty MockDatabase: if either guard is moved below service access, the
    // test fails on an unexpected query instead of producing a false-positive 403.
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let repository = Arc::new(DefaultCertificateRepository::new(
        db.clone(),
        encryption.clone(),
    ));
    let provider = Arc::new(UnusedCertificateProvider);
    let domain_service = Arc::new(DomainService::new(
        db.clone(),
        provider.clone(),
        repository.clone(),
        encryption,
    ));
    let state = Arc::new(DomainAppState {
        tls_service: Arc::new(TlsService::new(repository.clone(), provider)),
        repository,
        domain_service,
        dns_provider_service: None,
        audit_service: Arc::new(RecordingAudit::default()),
        telemetry: Arc::new(NoopTelemetryReporter),
        sensitive_action_authorizer: Arc::new(AllowAllSensitiveActions),
    });
    temps_domains::configure_routes().with_state(state)
}

async fn setup_dns_status(router: axum::Router, permissions: Vec<Permission>) -> StatusCode {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/domains/123/setup-dns")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"dns_provider_id":7}"#))
        .unwrap();
    request.extensions_mut().insert(auth(permissions));
    request.extensions_mut().insert(metadata());
    router.oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn test_setup_dns_with_only_domains_write_returns_forbidden_before_state_touch() {
    let status = setup_dns_status(permission_router(), vec![Permission::DomainsWrite]).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_setup_dns_with_only_dns_providers_write_returns_forbidden_before_state_touch() {
    let status = setup_dns_status(permission_router(), vec![Permission::DnsProvidersWrite]).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_setup_dns_inactive_provider_rejects_without_dns_or_audit_touch() {
    use temps_domains::tls::models::AcmeOrder;
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping inactive-provider router test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let repository = Arc::new(DefaultCertificateRepository::new(
        db.clone(),
        encryption.clone(),
    ));
    let certificate_provider = Arc::new(UnusedCertificateProvider);
    let domain_service = Arc::new(DomainService::new(
        db.clone(),
        certificate_provider.clone(),
        repository.clone(),
        encryption.clone(),
    ));
    let domain = domain_service
        .create_domain("app.example.com", "dns-01")
        .await
        .expect("create domain");
    repository
        .save_acme_order(AcmeOrder {
            id: 0,
            order_url: "https://acme.test/order/1".to_string(),
            domain_id: domain.id,
            email: "acme@example.com".to_string(),
            status: "pending".to_string(),
            identifiers: serde_json::json!([]),
            authorizations: Some(serde_json::json!({
                "dns_txt_records": [{
                    "name": "_acme-challenge.app.example.com",
                    "value": "must-not-be-published"
                }]
            })),
            finalize_url: None,
            certificate_url: None,
            error: None,
            error_type: None,
            token: Some("token".to_string()),
            key_authorization: Some("key-auth".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::days(1)),
        })
        .await
        .expect("save order");
    let provider = dns_providers::ActiveModel {
        name: Set("disabled-provider".to_string()),
        provider_type: Set("manual".to_string()),
        // Deliberately invalid ciphertext: a regression that initializes the
        // provider before checking is_active turns this request into a 500.
        credentials: Set("not-valid-ciphertext".to_string()),
        is_active: Set(false),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert inactive provider");
    dns_managed_domains::ActiveModel {
        provider_id: Set(provider.id),
        domain: Set("example.com".to_string()),
        auto_manage: Set(false),
        verified: Set(true),
        generated_hostname_mode: Set("standard".to_string()),
        sync_generated_records: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert managed domain");

    let dns_provider_service = Arc::new(temps_dns::services::DnsProviderService::new(
        db.clone(),
        encryption,
    ));
    let audit = Arc::new(RecordingAudit::default());
    let state = Arc::new(DomainAppState {
        tls_service: Arc::new(TlsService::new(repository.clone(), certificate_provider)),
        repository,
        domain_service,
        dns_provider_service: Some(dns_provider_service),
        audit_service: audit.clone(),
        telemetry: Arc::new(NoopTelemetryReporter),
        sensitive_action_authorizer: Arc::new(AllowAllSensitiveActions),
    });

    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/domains/{}/setup-dns", domain.id))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"dns_provider_id":{}}}"#,
            provider.id
        )))
        .unwrap();
    request.extensions_mut().insert(auth(vec![
        Permission::DomainsWrite,
        Permission::DnsProvidersWrite,
    ]));
    request.extensions_mut().insert(metadata());
    let response = temps_domains::configure_routes()
        .with_state(state)
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["title"], "DNS Provider Is Inactive");
    assert!(problem["detail"]
        .as_str()
        .unwrap()
        .contains("disabled-provider"));
    assert!(
        audit.operations.lock().unwrap().is_empty(),
        "rejected inactive providers must not emit a successful DNS setup audit"
    );
}

#[tokio::test]
#[serial_test::serial(dns_governance_db)]
async fn test_setup_dns_normalized_zone_ambiguity_returns_conflict() {
    use temps_domains::tls::models::AcmeOrder;
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping setup-DNS ambiguity test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let repository = Arc::new(DefaultCertificateRepository::new(
        db.clone(),
        encryption.clone(),
    ));
    let certificate_provider = Arc::new(UnusedCertificateProvider);
    let domain_service = Arc::new(DomainService::new(
        db.clone(),
        certificate_provider.clone(),
        repository.clone(),
        encryption.clone(),
    ));
    let domain = domain_service
        .create_domain("app.example.com", "dns-01")
        .await
        .expect("create domain");
    repository
        .save_acme_order(AcmeOrder {
            id: 0,
            order_url: "https://acme.test/order/ambiguity".to_string(),
            domain_id: domain.id,
            email: "acme@example.com".to_string(),
            status: "pending".to_string(),
            identifiers: serde_json::json!([]),
            authorizations: Some(serde_json::json!({
                "dns_txt_records": [{
                    "name": "_acme-challenge.app.example.com",
                    "value": "must-not-be-published"
                }]
            })),
            finalize_url: None,
            certificate_url: None,
            error: None,
            error_type: None,
            token: Some("token".to_string()),
            key_authorization: Some("key-auth".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::days(1)),
        })
        .await
        .expect("save order");
    let provider = dns_providers::ActiveModel {
        name: Set("ambiguous-provider".to_string()),
        provider_type: Set("manual".to_string()),
        credentials: Set("not-valid-ciphertext".to_string()),
        is_active: Set(true),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert provider");
    for zone in ["example.com", "  *.EXAMPLE.COM. "] {
        dns_managed_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set(zone.to_string()),
            auto_manage: Set(false),
            verified: Set(true),
            generated_hostname_mode: Set("standard".to_string()),
            sync_generated_records: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert equivalent managed zone");
    }

    let dns_provider_service =
        Arc::new(temps_dns::services::DnsProviderService::new(db, encryption));
    let state = Arc::new(DomainAppState {
        tls_service: Arc::new(TlsService::new(repository.clone(), certificate_provider)),
        repository,
        domain_service,
        dns_provider_service: Some(dns_provider_service),
        audit_service: Arc::new(RecordingAudit::default()),
        telemetry: Arc::new(NoopTelemetryReporter),
        sensitive_action_authorizer: Arc::new(AllowAllSensitiveActions),
    });
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/domains/{}/setup-dns", domain.id))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"dns_provider_id":{}}}"#,
            provider.id
        )))
        .unwrap();
    request.extensions_mut().insert(auth(vec![
        Permission::DomainsWrite,
        Permission::DnsProvidersWrite,
    ]));
    request.extensions_mut().insert(metadata());

    let response = temps_domains::configure_routes()
        .with_state(state)
        .oneshot(request)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["title"], "Ambiguous Managed DNS Zone");
    assert!(problem["detail"].as_str().unwrap().contains("example.com"));
    assert!(!problem["detail"]
        .as_str()
        .unwrap()
        .contains("must-not-be-published"));
}
