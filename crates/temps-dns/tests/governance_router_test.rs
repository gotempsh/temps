use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use sea_orm::{DatabaseBackend, MockDatabase};
use temps_auth::{AuthContext, Permission};
use temps_core::{AuditLogger, AuditOperation, Job, JobQueue, JobReceiver, RequestMetadata};
use temps_dns::{
    handlers::{configure_routes, DnsAppState},
    services::{DnsProviderService, DnsRecordService},
};
use tower::ServiceExt;

struct NoopQueue;

#[async_trait]
impl JobQueue for NoopQueue {
    async fn send(&self, _job: Job) -> Result<(), temps_core::QueueError> {
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn JobReceiver> {
        panic!("permission-boundary tests never subscribe")
    }
}

struct NoopAudit;

#[async_trait]
impl AuditLogger for NoopAudit {
    async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> anyhow::Result<()> {
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

fn test_user() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 42,
        name: "DNS operator".to_string(),
        email: "dns@example.com".to_string(),
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
        "governance-test".to_string(),
        1,
    )
}

fn metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "governance-router-test".to_string(),
        headers: Default::default(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

fn router() -> axum::Router {
    // No query results are registered. A permission regression that touches the
    // service/DB before returning 403 therefore fails loudly instead of merely
    // returning the same status for the wrong reason.
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let provider_service = Arc::new(DnsProviderService::new(
        db,
        Arc::new(temps_core::EncryptionService::new_from_password("test")),
    ));
    let state = Arc::new(DnsAppState {
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit: Arc::new(NoopAudit),
    });
    configure_routes().with_state(state)
}

fn router_with_db(
    db: Arc<sea_orm::DatabaseConnection>,
    encryption: Arc<temps_core::EncryptionService>,
) -> axum::Router {
    let provider_service = Arc::new(DnsProviderService::new(db, encryption));
    let state = Arc::new(DnsAppState {
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit: Arc::new(NoopAudit),
    });
    configure_routes().with_state(state)
}

fn request_for(
    method: Method,
    uri: impl AsRef<str>,
    permissions: Vec<Permission>,
    body: impl Into<Body>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri.as_ref())
        .header("content-type", "application/json")
        .body(body.into())
        .unwrap();
    request.extensions_mut().insert(auth(permissions));
    request.extensions_mut().insert(metadata());
    request
}

async fn request(
    method: Method,
    uri: &str,
    permissions: Vec<Permission>,
    body: &str,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(auth(permissions));
    request.extensions_mut().insert(metadata());
    router().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn test_list_dns_providers_without_read_permission_returns_forbidden_before_db_touch() {
    let status = request(
        Method::GET,
        "/dns-providers",
        vec![Permission::DnsProvidersWrite],
        "",
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_remove_managed_domain_without_write_permission_returns_forbidden_before_db_touch() {
    let status = request(
        Method::DELETE,
        "/dns-providers/7/domains/example.com",
        vec![Permission::DnsProvidersRead],
        "",
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_add_auto_managed_domain_without_automation_permission_returns_forbidden_before_db_touch(
) {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains",
        vec![Permission::DnsProvidersWrite],
        r#"{"domain":"example.com","auto_manage":true,"sync_generated_records":false}"#,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_add_sync_enabled_domain_without_automation_permission_returns_forbidden_before_db_touch(
) {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains",
        vec![Permission::DnsProvidersWrite],
        r#"{"domain":"example.com","auto_manage":false,"sync_generated_records":true}"#,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_add_non_automated_domain_does_not_require_automation_permission() {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains",
        vec![Permission::DnsProvidersWrite],
        r#"{"domain":"example.com","auto_manage":false,"sync_generated_records":false}"#,
    )
    .await;

    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "dns:providers:write alone must authorize auto_manage=false; the empty mock DB may fail later"
    );
}

#[tokio::test]
async fn test_apply_hostname_mode_with_dns_sync_without_automation_permission_returns_forbidden_before_db_touch(
) {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains/example.com/apply-hostname-mode",
        vec![Permission::DnsProvidersWrite],
        r#"{"mode":"flat","sync_dns":true}"#,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_apply_hostname_mode_without_dns_sync_does_not_require_automation_permission() {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains/example.com/apply-hostname-mode",
        vec![Permission::DnsProvidersWrite],
        r#"{"mode":"flat","sync_dns":false}"#,
    )
    .await;

    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "dns:providers:write must retain access when sync_dns is false; the empty mock DB may fail later"
    );
}

#[tokio::test]
async fn test_add_managed_domain_success_emits_governance_audit() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::dns_providers;

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping DNS router audit test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let provider = dns_providers::ActiveModel {
        name: Set("manual-dns".to_string()),
        provider_type: Set("manual".to_string()),
        credentials: Set(encryption.encrypt_string("{}").unwrap()),
        is_active: Set(true),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert provider");
    let provider_service = Arc::new(DnsProviderService::new(db, encryption));
    let audit = Arc::new(RecordingAudit::default());
    let state = Arc::new(DnsAppState {
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit: audit.clone(),
    });
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/dns-providers/{}/domains", provider.id))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"domain":"example.com","auto_manage":false,"sync_generated_records":false}"#,
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(auth(vec![Permission::DnsProvidersWrite]));
    request.extensions_mut().insert(metadata());

    let response = configure_routes()
        .with_state(state)
        .oneshot(request)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        audit.operations.lock().unwrap().as_slice(),
        ["DNS_MANAGED_DOMAIN_ADDED"]
    );
}

#[tokio::test]
async fn test_update_existing_automated_domain_enabling_sync_requires_automation_permission() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping DNS update permission test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let provider = dns_providers::ActiveModel {
        name: Set("manual-dns".to_string()),
        provider_type: Set("manual".to_string()),
        credentials: Set(encryption.encrypt_string("{}").unwrap()),
        is_active: Set(true),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert provider");
    dns_managed_domains::ActiveModel {
        provider_id: Set(provider.id),
        domain: Set("example.com".to_string()),
        auto_manage: Set(true),
        verified: Set(true),
        generated_hostname_mode: Set("standard".to_string()),
        sync_generated_records: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert managed domain");
    let uri = format!("/dns-providers/{}/domains/example.com", provider.id);

    let forbidden = router_with_db(db.clone(), encryption.clone())
        .oneshot(request_for(
            Method::PATCH,
            &uri,
            vec![Permission::DnsProvidersWrite],
            Body::from(r#"{"sync_generated_records":true}"#),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let authorized = router_with_db(db, encryption)
        .oneshot(request_for(
            Method::PATCH,
            &uri,
            vec![
                Permission::DnsProvidersWrite,
                Permission::DnsAutomationWrite,
            ],
            Body::from(r#"{"sync_generated_records":true}"#),
        ))
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body = axum::body::to_bytes(authorized.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response["auto_manage"], true);
    assert_eq!(response["sync_generated_records"], true);
}

#[tokio::test]
async fn test_list_zones_for_inactive_provider_rejects_before_credentials_are_decrypted() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::dns_providers;

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping inactive-provider zones test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let provider = dns_providers::ActiveModel {
        name: Set("disabled-cloudflare".to_string()),
        provider_type: Set("cloudflare".to_string()),
        credentials: Set("not-valid-ciphertext".to_string()),
        is_active: Set(false),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert inactive provider");

    let response = router_with_db(db, encryption)
        .oneshot(request_for(
            Method::GET,
            format!("/dns-providers/{}/zones", provider.id),
            vec![Permission::DnsProvidersRead],
            Body::empty(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["title"], "DNS Provider Is Inactive");
    assert!(problem["detail"]
        .as_str()
        .unwrap()
        .contains("disabled-cloudflare"));
}

#[tokio::test]
async fn test_find_provider_for_duplicate_zone_skips_inactive_provider_candidate() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping duplicate provider candidate test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let mut provider_ids = Vec::new();
    for (name, domain, is_active) in [
        ("inactive-provider", " EXAMPLE.COM. ", false),
        ("active-provider", "example.com", true),
    ] {
        let provider = dns_providers::ActiveModel {
            name: Set(name.to_string()),
            provider_type: Set("manual".to_string()),
            credentials: Set(encryption.encrypt_string("{}").unwrap()),
            is_active: Set(is_active),
            description: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert provider");
        dns_managed_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set(domain.to_string()),
            auto_manage: Set(true),
            verified: Set(true),
            generated_hostname_mode: Set("standard".to_string()),
            sync_generated_records: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert duplicate managed domain");
        provider_ids.push((provider.id, is_active));
    }
    let active_provider_id = provider_ids
        .iter()
        .find_map(|(id, active)| active.then_some(*id))
        .unwrap();
    let service = DnsProviderService::new(db, encryption);

    let (provider, managed) = service
        .find_provider_for_domain("app.example.com")
        .await
        .expect("provider lookup")
        .expect("active provider candidate");

    assert_eq!(provider.id, active_provider_id);
    assert!(provider.is_active);
    assert_eq!(managed.provider_id, active_provider_id);
}

#[tokio::test]
#[serial_test::serial(dns_governance_db)]
async fn test_add_managed_domain_canonical_duplicate_returns_conflict() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_dns::services::AddManagedDomainRequest;
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping canonical duplicate router test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let encrypted_credentials = encryption.encrypt_string("{}").unwrap();

    let existing_provider = dns_providers::ActiveModel {
        name: Set("existing-provider".to_string()),
        provider_type: Set("manual".to_string()),
        credentials: Set(encrypted_credentials.clone()),
        is_active: Set(true),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert existing provider");
    dns_managed_domains::ActiveModel {
        provider_id: Set(existing_provider.id),
        domain: Set("  *.EXAMPLE.COM. ".to_string()),
        auto_manage: Set(false),
        verified: Set(true),
        generated_hostname_mode: Set("standard".to_string()),
        sync_generated_records: Set(false),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert legacy non-canonical managed domain");
    let target_provider = dns_providers::ActiveModel {
        name: Set("target-provider".to_string()),
        provider_type: Set("manual".to_string()),
        credentials: Set(encrypted_credentials),
        is_active: Set(true),
        description: Set(None),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert target provider");

    let response = router_with_db(db.clone(), encryption.clone())
        .oneshot(request_for(
            Method::POST,
            format!("/dns-providers/{}/domains", target_provider.id),
            vec![Permission::DnsProvidersWrite],
            Body::from(
                r#"{"domain":"example.com","auto_manage":false,"sync_generated_records":false}"#,
            ),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["title"], "Managed DNS Domain Already Exists");
    assert!(problem["detail"]
        .as_str()
        .unwrap()
        .contains("canonicalizes to 'example.com', which is already managed"));

    let service = DnsProviderService::new(db, encryption);
    let fresh = service
        .add_managed_domain(
            target_provider.id,
            AddManagedDomainRequest {
                domain: "  *.Fresh.Example.NET. ".to_string(),
                auto_manage: false,
                generated_hostname_mode: None,
                sync_generated_records: false,
            },
        )
        .await
        .expect("add a fresh non-canonical managed domain");
    assert_eq!(fresh.domain, "fresh.example.net");
}

#[tokio::test]
#[serial_test::serial(dns_governance_db)]
async fn test_find_provider_for_canonical_duplicate_eligible_zones_returns_ambiguity() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    use temps_dns::errors::DnsError;
    use temps_entities::{dns_managed_domains, dns_providers};

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping ambiguous managed-zone test: {error}");
            return;
        }
        Err(error) => panic!("failed to create test database: {error}"),
    };
    let db = test_db.db.clone();
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let encrypted_credentials = encryption.encrypt_string("{}").unwrap();
    let mut longest_zones = Vec::new();

    for (name, domain) in [
        ("longest-one", "api.example.com"),
        ("longest-two", "  *.API.EXAMPLE.COM. "),
        ("parent", "example.com"),
    ] {
        let provider = dns_providers::ActiveModel {
            name: Set(name.to_string()),
            provider_type: Set("manual".to_string()),
            credentials: Set(encrypted_credentials.clone()),
            is_active: Set(true),
            description: Set(None),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert active provider");
        let managed = dns_managed_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set(domain.to_string()),
            auto_manage: Set(true),
            verified: Set(true),
            generated_hostname_mode: Set("standard".to_string()),
            sync_generated_records: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert eligible managed domain");
        if name.starts_with("longest") {
            longest_zones.push((provider.id, managed));
        }
    }
    let service = DnsProviderService::new(db.clone(), encryption);

    let error = service
        .find_provider_for_domain("app.api.example.com")
        .await
        .expect_err("equivalent longest eligible zones must fail closed");
    match error {
        DnsError::AmbiguousManagedDomain {
            requested_domain,
            canonical_zone,
            managed_domain_ids,
            provider_ids,
        } => {
            assert_eq!(requested_domain, "app.api.example.com");
            assert_eq!(canonical_zone, "api.example.com");
            assert_eq!(managed_domain_ids.len(), 2);
            assert_eq!(provider_ids.len(), 2);
            assert!(longest_zones
                .iter()
                .all(|(provider_id, managed)| provider_ids.contains(provider_id)
                    && managed_domain_ids.contains(&managed.id)));
        }
        other => panic!("expected AmbiguousManagedDomain, got {other:?}"),
    }

    for (_, managed) in longest_zones {
        let mut active: dns_managed_domains::ActiveModel = managed.into();
        active.auto_manage = Set(false);
        active
            .update(db.as_ref())
            .await
            .expect("make tied longest zone ineligible");
    }
    let (provider, managed) = service
        .find_provider_for_domain("app.api.example.com")
        .await
        .expect("lookup with only parent eligible")
        .expect("shorter parent must remain a valid fallback");
    assert_eq!(provider.name, "parent");
    assert_eq!(managed.domain, "example.com");
}
