// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Each operation as it is serialized into the audit log.
    records: Mutex<Vec<serde_json::Value>>,
}

impl RecordingAudit {
    fn records(&self) -> Vec<serde_json::Value> {
        self.records.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
        self.operations
            .lock()
            .unwrap()
            .push(operation.operation_type());
        self.records
            .lock()
            .unwrap()
            .push(serde_json::from_str(&operation.serialize()?)?);
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
    let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
    let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
    let managed_record_service = Arc::new(temps_dns::services::ManagedDnsRecordService::new(
        db.clone(),
        provider_service.clone(),
        encryption.clone(),
    ));
    let state = Arc::new(DnsAppState {
        domain_delivery_service: Arc::new(
            temps_dns::services::domain_delivery::DomainDeliveryService::new(
                db.clone(),
                managed_record_service.clone(),
                encryption,
            ),
        ),
        managed_record_service,
        project_access_checker: None,
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit_service: Arc::new(NoopAudit),
    });
    configure_routes().with_state(state)
}

fn router_with_db(
    db: Arc<sea_orm::DatabaseConnection>,
    encryption: Arc<temps_core::EncryptionService>,
) -> axum::Router {
    router_with_audit(db, encryption, Arc::new(NoopAudit))
}

fn router_with_audit(
    db: Arc<sea_orm::DatabaseConnection>,
    encryption: Arc<temps_core::EncryptionService>,
    audit_service: Arc<dyn AuditLogger>,
) -> axum::Router {
    let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
    let managed_record_service = Arc::new(temps_dns::services::ManagedDnsRecordService::new(
        db.clone(),
        provider_service.clone(),
        Arc::new(temps_core::EncryptionService::new_from_password("test")),
    ));
    let state = Arc::new(DnsAppState {
        domain_delivery_service: Arc::new(
            temps_dns::services::domain_delivery::DomainDeliveryService::new(
                db.clone(),
                managed_record_service.clone(),
                encryption,
            ),
        ),
        managed_record_service,
        project_access_checker: None,
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit_service,
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
    let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
    let audit = Arc::new(RecordingAudit::default());
    let managed_record_service = Arc::new(temps_dns::services::ManagedDnsRecordService::new(
        db.clone(),
        provider_service.clone(),
        Arc::new(temps_core::EncryptionService::new_from_password("test")),
    ));
    let state = Arc::new(DnsAppState {
        domain_delivery_service: Arc::new(
            temps_dns::services::domain_delivery::DomainDeliveryService::new(
                db.clone(),
                managed_record_service.clone(),
                encryption,
            ),
        ),
        managed_record_service,
        project_access_checker: None,
        record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
        provider_service,
        queue: Arc::new(NoopQueue),
        audit_service: audit.clone(),
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
    let service = DnsProviderService::new(db.clone(), encryption.clone());

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

    let service = DnsProviderService::new(db.clone(), encryption.clone());
    let fresh = service
        .add_managed_domain(
            target_provider.id,
            AddManagedDomainRequest {
                domain: "  *.Fresh.Example.NET. ".to_string(),
                auto_manage: false,
                proxied_by_default: false,
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

#[tokio::test]
async fn project_reader_can_list_delivery_profiles_without_dns_provider_access() {
    // An empty table: the count is the only query, there is no page to fetch.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(0)])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db, test_encryption()),
        "/delivery-profiles",
        vec![Permission::ProjectsRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        page,
        serde_json::json!({"items": [], "total": 0, "page": 1, "page_size": 20})
    );
}

#[tokio::test]
async fn delivery_profiles_still_require_project_or_dns_read_access() {
    let status = request(
        Method::GET,
        "/delivery-profiles",
        vec![Permission::ProjectsWrite],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

fn unauthenticated_request(method: Method, uri: &str, body: &str) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(metadata());
    request
}

fn bunny_profile_model() -> temps_entities::delivery_profiles::Model {
    let now = chrono::Utc::now();
    temps_entities::delivery_profiles::Model {
        id: 9,
        name: "Edge CDN".into(),
        provider_kind: "bunny".into(),
        bunny_pull_zone_id: Some(4242),
        bunny_hostname: Some("temps-edge.b-cdn.net".into()),
        bunny_api_key_encrypted: Some("ciphertext".into()),
        created_at: now,
        updated_at: now,
    }
}

async fn list_profiles_as(permissions: Vec<Permission>) -> serde_json::Value {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(1)])
            .append_query_results([vec![bunny_profile_model()]])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db, test_encryption()),
        "/delivery-profiles",
        permissions,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1, "{page}");
    page
}

#[tokio::test]
async fn project_reader_sees_only_profile_identity_and_kind() {
    let page = list_profiles_as(vec![Permission::ProjectsRead]).await;
    let profile = &page["items"][0];
    assert_eq!(profile["id"], 9);
    assert_eq!(profile["name"], "Edge CDN");
    assert_eq!(profile["provider_kind"], "bunny");
    assert!(profile["bunny_pull_zone_id"].is_null(), "{profile}");
    assert!(profile["bunny_hostname"].is_null(), "{profile}");
    assert!(!page.to_string().contains("ciphertext"));
}

#[tokio::test]
async fn dns_provider_reader_sees_full_profile_details() {
    let page = list_profiles_as(vec![Permission::DnsProvidersRead]).await;
    let profile = &page["items"][0];
    assert_eq!(profile["bunny_pull_zone_id"], 4242);
    assert_eq!(profile["bunny_hostname"], "temps-edge.b-cdn.net");
    assert!(!page.to_string().contains("ciphertext"));
}

fn test_encryption() -> Arc<temps_core::EncryptionService> {
    Arc::new(temps_core::EncryptionService::new_from_password("test"))
}

/// The single row sea-orm's paginator reads its `COUNT(*)` from.
fn count_rows(total: i64) -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
    vec![std::collections::BTreeMap::from([(
        "num_items",
        sea_orm::Value::BigInt(Some(total)),
    )])]
}

/// GET `uri` and return the status with the JSON body (`null` when empty).
async fn send_json(
    router: axum::Router,
    uri: impl AsRef<str>,
    permissions: Vec<Permission>,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .oneshot(request_for(Method::GET, uri, permissions, Body::empty()))
        .await
        .expect("router response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    if body.is_empty() {
        return (status, serde_json::Value::Null);
    }
    let json = serde_json::from_slice(&body)
        .unwrap_or_else(|error| panic!("{status} body is not JSON ({error}): {body:?}"));
    (status, json)
}

/// SQL the mock database executed, with bound values inlined. Call it after
/// the router (which holds the other handles) has been dropped.
fn executed_sql(db: Arc<sea_orm::DatabaseConnection>) -> Vec<String> {
    Arc::try_unwrap(db)
        .expect("the router released its database handles")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements().iter().map(ToString::to_string))
        .collect()
}

fn item_ids(page: &serde_json::Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("page without items: {page}"))
        .iter()
        .map(|item| item["id"].as_i64().expect("item id"))
        .collect()
}

#[tokio::test]
async fn delivery_profile_list_defaults_to_twenty_newest_first() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(25)])
            .append_query_results([vec![bunny_profile_model()]])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db.clone(), test_encryption()),
        "/delivery-profiles",
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 25);
    assert_eq!(page["page"], 1);
    assert_eq!(page["page_size"], 20);

    let sql = executed_sql(db);
    assert_eq!(sql.len(), 2, "count, then one page: {sql:#?}");
    assert!(sql[0].contains("COUNT(*)"), "{}", sql[0]);
    assert!(
        sql[1].contains(
            r#"ORDER BY "delivery_profiles"."created_at" DESC, "delivery_profiles"."id" DESC"#
        ),
        "{}",
        sql[1]
    );
    assert!(sql[1].ends_with("LIMIT 20 OFFSET 0"), "{}", sql[1]);
}

#[tokio::test]
async fn delivery_profile_list_clamps_page_size_and_applies_requested_sort() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(250)])
            .append_query_results([vec![bunny_profile_model()]])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db.clone(), test_encryption()),
        "/delivery-profiles?page=3&page_size=1000&sort_by=name&sort_order=ASC",
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page"], 3);
    assert_eq!(page["page_size"], 100);
    assert_eq!(page["total"], 250);

    let sql = executed_sql(db);
    assert_eq!(sql.len(), 2, "{sql:#?}");
    assert!(
        sql[1].contains(r#"ORDER BY "delivery_profiles"."name" ASC, "delivery_profiles"."id" ASC"#),
        "{}",
        sql[1]
    );
    assert!(sql[1].ends_with("LIMIT 100 OFFSET 200"), "{}", sql[1]);
}

#[tokio::test]
async fn delivery_profile_list_clamps_zero_page_and_page_size_to_one() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(3)])
            .append_query_results([vec![bunny_profile_model()]])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db.clone(), test_encryption()),
        "/delivery-profiles?page=0&page_size=0",
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page"], 1);
    assert_eq!(page["page_size"], 1);
    let sql = executed_sql(db);
    assert!(sql[1].ends_with("LIMIT 1 OFFSET 0"), "{}", sql[1]);
}

#[tokio::test]
async fn delivery_profile_search_filters_both_the_count_and_the_page() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(1)])
            .append_query_results([vec![bunny_profile_model()]])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db.clone(), test_encryption()),
        "/delivery-profiles?search=%20Edge%20&sort_by=name",
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1);

    let sql = executed_sql(db);
    assert_eq!(sql.len(), 2, "count, then one page: {sql:#?}");
    for statement in &sql {
        assert!(
            statement.contains(r#""delivery_profiles"."name" ILIKE '%Edge%'"#),
            "the trimmed term filters every query: {statement}"
        );
    }
}

#[tokio::test]
async fn delivery_profile_search_longer_than_any_name_is_rejected_before_any_query() {
    // `router()` has no query results, so a database access would be a 500.
    let (status, problem) = send_json(
        router(),
        format!("/delivery-profiles?search={}", "a".repeat(101)),
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(
        problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("101 characters")),
        "{problem}"
    );
}

#[tokio::test]
async fn delivery_profile_page_past_the_end_is_empty_without_fetching_rows() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([count_rows(25)])
            .into_connection(),
    );
    let (status, page) = send_json(
        router_with_db(db.clone(), test_encryption()),
        // Far enough that `page * page_size` would overflow u64 if computed.
        "/delivery-profiles?page=18446744073709551615&page_size=100",
        vec![Permission::DnsProvidersRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"], serde_json::json!([]));
    assert_eq!(page["total"], 25);
    assert_eq!(executed_sql(db).len(), 1, "only the count may run");
}

#[tokio::test]
async fn delivery_lists_reject_unknown_sort_values_before_touching_the_database() {
    // `router()` has no query results, so any DB access would surface as a
    // 500 instead of the validation error.
    for (uri, allowed) in [
        (
            "/delivery-profiles?sort_by=bunny_api_key_encrypted",
            "allowed values: created_at, name",
        ),
        (
            "/delivery-profiles?sort_by=hostname",
            "allowed values: created_at, name",
        ),
        (
            "/delivery-profiles?sort_order=sideways",
            "allowed values: asc, desc",
        ),
        (
            "/projects/1/domain-delivery-bindings?sort_by=name",
            "allowed values: created_at, hostname, updated_at",
        ),
        (
            "/projects/1/domain-delivery-bindings?sort_order=newest",
            "allowed values: asc, desc",
        ),
    ] {
        let (status, problem) = send_json(
            router(),
            uri,
            vec![Permission::ProjectsRead, Permission::DnsProvidersRead],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {problem}");
        assert_eq!(problem["title"], "Validation Error", "{uri}");
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(detail.contains(allowed), "{uri}: {detail}");
    }
}

async fn get_profile_as(
    permissions: Vec<Permission>,
    rows: Vec<temps_entities::delivery_profiles::Model>,
) -> (StatusCode, serde_json::Value) {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .into_connection(),
    );
    send_json(
        router_with_db(db, test_encryption()),
        "/delivery-profiles/9",
        permissions,
    )
    .await
}

#[tokio::test]
async fn dns_provider_reader_gets_one_profile_with_provider_details() {
    let (status, profile) = get_profile_as(
        vec![Permission::DnsProvidersRead],
        vec![bunny_profile_model()],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["id"], 9);
    assert_eq!(profile["name"], "Edge CDN");
    assert_eq!(profile["bunny_pull_zone_id"], 4242);
    assert_eq!(profile["bunny_hostname"], "temps-edge.b-cdn.net");
    assert!(!profile.to_string().contains("ciphertext"));
}

#[tokio::test]
async fn project_reader_gets_one_profile_without_provider_details() {
    let (status, profile) =
        get_profile_as(vec![Permission::ProjectsRead], vec![bunny_profile_model()]).await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["id"], 9);
    assert_eq!(profile["provider_kind"], "bunny");
    assert!(profile["bunny_pull_zone_id"].is_null(), "{profile}");
    assert!(profile["bunny_hostname"].is_null(), "{profile}");
    assert!(!profile.to_string().contains("ciphertext"));
}

#[tokio::test]
async fn missing_delivery_profile_returns_not_found_naming_the_id() {
    let (status, problem) = get_profile_as(vec![Permission::ProjectsRead], Vec::new()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("Delivery profile 9 not found"), "{detail}");
    assert_eq!(problem["title"], "Delivery Profile Not Found", "{problem}");
}

#[tokio::test]
async fn delivery_reads_without_authentication_return_unauthorized() {
    for uri in [
        "/delivery-profiles",
        "/delivery-profiles/9",
        "/projects/1/domain-delivery-bindings",
    ] {
        let status = router()
            .oneshot(unauthenticated_request(Method::GET, uri, ""))
            .await
            .expect("router response")
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn delivery_reads_without_read_access_return_forbidden_before_db_touch() {
    for uri in [
        "/delivery-profiles",
        "/delivery-profiles/9",
        "/projects/1/domain-delivery-bindings",
    ] {
        let status = request(Method::GET, uri, vec![Permission::ProjectsWrite], "").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
}

/// The generated clients are built from this document, so the paging
/// parameters and page bodies must be in it, not only in the router.
#[test]
fn delivery_reads_are_documented_with_paging_and_page_bodies() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(temps_dns::handlers::DnsApiDoc::openapi())
        .expect("OpenAPI document serializes");
    for (path, page_schema) in [
        ("/delivery-profiles", "DeliveryProfilePage"),
        (
            "/projects/{project_id}/domain-delivery-bindings",
            "DomainDeliveryBindingPage",
        ),
    ] {
        let get = &spec["paths"][path]["get"];
        let parameters: Vec<&str> = get["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("GET {path} documents no parameters"))
            .iter()
            .filter_map(|parameter| parameter["name"].as_str())
            .collect();
        for name in ["page", "page_size", "sort_by", "sort_order"] {
            assert!(
                parameters.contains(&name),
                "GET {path} lacks {name}: {parameters:?}"
            );
        }
        assert_eq!(
            get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            format!("#/components/schemas/{page_schema}"),
            "GET {path}"
        );
        assert!(
            get["responses"]["400"].is_object(),
            "GET {path} documents no 400"
        );
        let schema = &spec["components"]["schemas"][page_schema];
        for field in ["items", "total", "page", "page_size"] {
            assert!(
                schema["properties"][field].is_object(),
                "{page_schema} lacks {field}: {schema}"
            );
        }
    }
    let get_one = &spec["paths"]["/delivery-profiles/{profile_id}"]["get"];
    assert_eq!(
        get_one["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/DeliveryProfileResponse"
    );
    for status in ["401", "403", "404", "500"] {
        assert!(
            get_one["responses"][status].is_object(),
            "GET /delivery-profiles/{{profile_id}} documents no {status}"
        );
    }
}

/// A fresh migrated database, or `None` (and the test skips) when no
/// container runtime is available.
async fn migrated_database(test_name: &str) -> Option<temps_database::test_utils::TestDatabase> {
    match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error)
            if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
        {
            eprintln!("Docker unavailable; skipping {test_name}: {error}");
            None
        }
        Err(error) => panic!("failed to create test database for {test_name}: {error}"),
    }
}

/// Whole minutes from a fixed instant, so Postgres' microsecond timestamps
/// order exactly like the in-memory expectations.
fn at_minute(minute: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_767_225_600 + minute * 60, 0).expect("valid timestamp")
}

/// IDs of `rows` ordered by `key`, whose last element is the row ID: the
/// API's tie-breaker, applied in the same direction as the sort column.
fn ids_by<T, K: Ord>(rows: &[T], key: impl Fn(&T) -> (K, i32), descending: bool) -> Vec<i64> {
    let mut keys: Vec<(K, i32)> = rows.iter().map(key).collect();
    keys.sort();
    if descending {
        keys.reverse();
    }
    keys.into_iter().map(|(_, id)| i64::from(id)).collect()
}

async fn insert_returning_id(
    db: &sea_orm::DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> i32 {
    use sea_orm::ConnectionTrait;
    db.query_one(sea_orm::Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        values,
    ))
    .await
    .unwrap_or_else(|error| panic!("{sql}: {error}"))
    .unwrap_or_else(|| panic!("{sql}: no row returned"))
    .try_get("", "id")
    .unwrap_or_else(|error| panic!("{sql}: no id column: {error}"))
}

async fn insert_direct_profile(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> temps_entities::delivery_profiles::Model {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    temps_entities::delivery_profiles::ActiveModel {
        name: Set(name.to_string()),
        provider_kind: Set("direct".to_string()),
        bunny_pull_zone_id: Set(None),
        bunny_hostname: Set(None),
        bunny_api_key_encrypted: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap_or_else(|error| panic!("insert delivery profile {name}: {error}"))
}

/// The rows every binding of one project points at.
#[derive(Clone, Copy)]
struct BindingParents {
    project_id: i32,
    environment_id: i32,
    custom_domain_id: i32,
    dns_provider_id: i32,
}

async fn insert_binding_parents(
    db: &sea_orm::DatabaseConnection,
    slug: &str,
    dns_provider_id: i32,
) -> BindingParents {
    let project_id = insert_returning_id(
        db,
        "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, created_at, updated_at, slug) \
         VALUES ($1, 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), $1) RETURNING id",
        vec![slug.into()],
    )
    .await;
    let environment_id = insert_returning_id(
        db,
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, updated_at, project_id) \
         VALUES ('production', 'production', $1, $2, '[]', now(), now(), $3) RETURNING id",
        vec![
            format!("{slug}-production").into(),
            format!("{slug}-production.example.test").into(),
            project_id.into(),
        ],
    )
    .await;
    let custom_domain_id = insert_returning_id(
        db,
        "INSERT INTO project_custom_domains (project_id, environment_id, domain, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', now(), now()) RETURNING id",
        vec![
            project_id.into(),
            environment_id.into(),
            format!("{slug}.example.test").into(),
        ],
    )
    .await;
    BindingParents {
        project_id,
        environment_id,
        custom_domain_id,
        dns_provider_id,
    }
}

async fn insert_binding(
    db: &sea_orm::DatabaseConnection,
    parents: BindingParents,
    profile_id: i32,
    hostname: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> temps_entities::domain_delivery_bindings::Model {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};
    temps_entities::domain_delivery_bindings::ActiveModel {
        hostname: Set(hostname.clone()),
        project_id: Set(parents.project_id),
        environment_id: Set(parents.environment_id),
        custom_domain_id: Set(parents.custom_domain_id),
        profile_id: Set(profile_id),
        profile_source: Set("project".to_string()),
        dns_provider_id: Set(parents.dns_provider_id),
        zone: Set("example.test".to_string()),
        origin_target: Set("192.0.2.10".to_string()),
        record_type: Set("A".to_string()),
        proxied: Set(false),
        status: Set("applied".to_string()),
        last_error: Set(None),
        created_at: Set(created_at),
        updated_at: Set(updated_at),
        applied_at: Set(Some(updated_at)),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap_or_else(|error| panic!("insert delivery binding {hostname}: {error}"))
}

#[tokio::test]
async fn delivery_profile_pages_follow_the_requested_order_in_postgres() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set};

    let Some(test_db) = migrated_database("delivery profile paging").await else {
        return;
    };
    let db = test_db.db.clone();
    // created_at falls as IDs rise, and names follow a third permutation, so
    // each assertion only passes when the list really sorts by the requested
    // column. Profiles 3 and 4 share a created_at to exercise the ID
    // tie-breaker.
    let mut profiles = Vec::new();
    for i in 0..25_i64 {
        let minute = if i == 4 { -3 } else { -i };
        let name = format!("profile-{:02}", (i * 7) % 25);
        profiles.push(insert_direct_profile(db.as_ref(), &name, at_minute(minute)).await);
    }
    let newest_first = ids_by(&profiles, |p| (p.created_at, p.id), true);
    let oldest_first = ids_by(&profiles, |p| (p.created_at, p.id), false);
    let by_name = ids_by(&profiles, |p| (p.name.clone(), p.id), false);
    let tie_position = |id: i32| newest_first.iter().position(|item| *item == i64::from(id));
    assert!(
        tie_position(profiles[4].id) < tie_position(profiles[3].id),
        "equal created_at must fall back to ID descending"
    );

    let app = router_with_db(db.clone(), test_encryption());
    let reader = || vec![Permission::DnsProvidersRead];

    let (status, first) = send_json(app.clone(), "/delivery-profiles", reader()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["total"], 25);
    assert_eq!(first["page"], 1);
    assert_eq!(first["page_size"], 20);
    assert_eq!(item_ids(&first), newest_first[..20].to_vec());

    let (_, second) = send_json(app.clone(), "/delivery-profiles?page=2", reader()).await;
    assert_eq!(second["page"], 2);
    assert_eq!(second["total"], 25);
    assert_eq!(item_ids(&second), newest_first[20..].to_vec());

    let (_, clamped) = send_json(app.clone(), "/delivery-profiles?page_size=1000", reader()).await;
    assert_eq!(clamped["page_size"], 100);
    assert_eq!(item_ids(&clamped), newest_first);

    let (_, oldest) = send_json(
        app.clone(),
        "/delivery-profiles?sort_by=created_at&sort_order=asc",
        reader(),
    )
    .await;
    assert_eq!(item_ids(&oldest), oldest_first[..20].to_vec());

    let (_, named) = send_json(
        app.clone(),
        "/delivery-profiles?sort_by=name&sort_order=asc&page=2",
        reader(),
    )
    .await;
    assert_eq!(item_ids(&named), by_name[20..].to_vec());

    // The newest profile is a Bunny one: it leads page 1, and project readers
    // still only get its identity and kind.
    let bunny = temps_entities::delivery_profiles::ActiveModel {
        name: Set("edge-cdn".to_string()),
        provider_kind: Set("bunny".to_string()),
        bunny_pull_zone_id: Set(Some(4242)),
        bunny_hostname: Set(Some("temps-edge.b-cdn.net".to_string())),
        bunny_api_key_encrypted: Set(Some("ciphertext".to_string())),
        created_at: Set(at_minute(10)),
        updated_at: Set(at_minute(10)),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("insert Bunny profile");

    let (status, reduced) = send_json(
        app.clone(),
        "/delivery-profiles",
        vec![Permission::ProjectsRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reduced}");
    assert_eq!(reduced["total"], 26);
    let items = reduced["items"].as_array().expect("items");
    assert_eq!(items.len(), 20);
    assert_eq!(items[0]["id"], bunny.id);
    assert_eq!(items[0]["provider_kind"], "bunny");
    assert!(
        items
            .iter()
            .all(|item| item["bunny_pull_zone_id"].is_null() && item["bunny_hostname"].is_null()),
        "{reduced}"
    );
    assert!(!reduced.to_string().contains("ciphertext"));

    let (_, full) = send_json(app.clone(), "/delivery-profiles", reader()).await;
    assert_eq!(full["items"][0]["bunny_pull_zone_id"], 4242);
    assert_eq!(full["items"][0]["bunny_hostname"], "temps-edge.b-cdn.net");
    assert!(!full.to_string().contains("ciphertext"));

    let (status, one) = send_json(
        app.clone(),
        format!("/delivery-profiles/{}", bunny.id),
        vec![Permission::ProjectsRead],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["name"], "edge-cdn");
    assert!(one["bunny_pull_zone_id"].is_null(), "{one}");

    let (status, missing) = send_json(app, "/delivery-profiles/999999", reader()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
}

#[tokio::test]
async fn domain_delivery_binding_pages_stay_in_their_project_in_postgres() {
    let Some(test_db) = migrated_database("delivery binding paging").await else {
        return;
    };
    let db = test_db.db.clone();
    let dns_provider_id = insert_returning_id(
        db.as_ref(),
        "INSERT INTO dns_providers (name, provider_type, credentials, is_active, created_at, updated_at) \
         VALUES ('paging-dns', 'manual', '{}', true, now(), now()) RETURNING id",
        Vec::new(),
    )
    .await;
    let parents = insert_binding_parents(db.as_ref(), "paged-bindings", dns_provider_id).await;
    let other_parents =
        insert_binding_parents(db.as_ref(), "other-bindings", dns_provider_id).await;
    let profiles = [
        insert_direct_profile(db.as_ref(), "direct-even", at_minute(0)).await,
        insert_direct_profile(db.as_ref(), "direct-odd", at_minute(0)).await,
    ];

    // created_at falls as IDs rise (bindings 5 and 6 tie), while hostname
    // and updated_at follow two other permutations, so each sort is
    // distinguishable from the others and from ID order.
    let mut bindings = Vec::new();
    for i in 0..23_i64 {
        let created = if i == 6 { -5 } else { -i };
        bindings.push(
            insert_binding(
                db.as_ref(),
                parents,
                profiles[(i % 2) as usize].id,
                format!("app-{:02}.example.test", (i * 5) % 23),
                at_minute(created),
                at_minute((i * 3) % 23),
            )
            .await,
        );
    }
    // Newer than every binding above: if the project filter were missing
    // they would lead page 1.
    let mut other_bindings = Vec::new();
    for i in 0..2_i64 {
        other_bindings.push(
            insert_binding(
                db.as_ref(),
                other_parents,
                profiles[0].id,
                format!("other-{i}.example.test"),
                at_minute(100 + i),
                at_minute(100 + i),
            )
            .await,
        );
    }
    let newest_first = ids_by(&bindings, |b| (b.created_at, b.id), true);
    let by_hostname = ids_by(&bindings, |b| (b.hostname.clone(), b.id), false);
    let recently_updated = ids_by(&bindings, |b| (b.updated_at, b.id), true);
    let tie_position = |id: i32| newest_first.iter().position(|item| *item == i64::from(id));
    assert!(
        tie_position(bindings[6].id) < tie_position(bindings[5].id),
        "equal created_at must fall back to ID descending"
    );

    let app = router_with_db(db.clone(), test_encryption());
    let reader = || vec![Permission::ProjectsRead];
    let uri = format!("/projects/{}/domain-delivery-bindings", parents.project_id);

    let (status, first) = send_json(app.clone(), &uri, reader()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["total"], 23);
    assert_eq!(first["page"], 1);
    assert_eq!(first["page_size"], 20);
    assert_eq!(item_ids(&first), newest_first[..20].to_vec());
    for item in first["items"].as_array().expect("items") {
        assert_eq!(item["project_id"], parents.project_id, "{item}");
        let binding = bindings
            .iter()
            .find(|binding| item["id"] == binding.id)
            .expect("listed binding exists");
        let profile = profiles
            .iter()
            .find(|profile| profile.id == binding.profile_id)
            .expect("binding profile exists");
        assert_eq!(item["delivery_profile_id"], profile.id);
        assert_eq!(item["delivery_profile_name"], profile.name.as_str());
        assert_eq!(item["provider_kind"], "direct");
    }

    let (_, second) = send_json(app.clone(), format!("{uri}?page=2"), reader()).await;
    assert_eq!(second["page"], 2);
    assert_eq!(item_ids(&second), newest_first[20..].to_vec());

    let (_, clamped) = send_json(app.clone(), format!("{uri}?page_size=500"), reader()).await;
    assert_eq!(clamped["page_size"], 100);
    assert_eq!(clamped["total"], 23);
    assert_eq!(item_ids(&clamped), newest_first);

    let (_, by_host) = send_json(
        app.clone(),
        format!("{uri}?sort_by=hostname&sort_order=asc"),
        reader(),
    )
    .await;
    assert_eq!(item_ids(&by_host), by_hostname[..20].to_vec());
    let (_, by_host_rest) = send_json(
        app.clone(),
        format!("{uri}?sort_by=hostname&sort_order=Asc&page=2"),
        reader(),
    )
    .await;
    assert_eq!(item_ids(&by_host_rest), by_hostname[20..].to_vec());

    let (_, updated) = send_json(app.clone(), format!("{uri}?sort_by=updated_at"), reader()).await;
    assert_eq!(item_ids(&updated), recently_updated[..20].to_vec());

    let (_, past_end) = send_json(app.clone(), format!("{uri}?page=9"), reader()).await;
    assert_eq!(past_end["items"], serde_json::json!([]));
    assert_eq!(past_end["total"], 23);

    let (status, other) = send_json(
        app.clone(),
        format!(
            "/projects/{}/domain-delivery-bindings",
            other_parents.project_id
        ),
        reader(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other}");
    assert_eq!(other["total"], 2);
    assert_eq!(
        item_ids(&other),
        ids_by(&other_bindings, |b| (b.created_at, b.id), true)
    );

    let (status, missing) =
        send_json(app, "/projects/999999/domain-delivery-bindings", reader()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
}

#[tokio::test]
async fn test_add_proxied_by_default_domain_without_automation_permission_returns_forbidden() {
    let status = request(
        Method::POST,
        "/dns-providers/7/domains",
        vec![Permission::DnsProvidersWrite],
        r#"{"domain":"example.com","auto_manage":false,"sync_generated_records":false,"proxied_by_default":true}"#,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Write endpoints that must reject unauthenticated callers with 401 and
/// callers lacking DNS automation rights with 403, before any DB access (the
/// router's mock DB has no results registered).
fn guarded_write_endpoints() -> Vec<(Method, &'static str, &'static str)> {
    vec![
        (
            Method::POST,
            "/dns-records",
            r#"{"domain":"example.com","name":"app","content":{"type":"A","value":{"address":"192.0.2.1"}},"ttl":300}"#,
        ),
        (
            Method::DELETE,
            "/dns-records?domain=example.com&name=app&record_type=A",
            "",
        ),
        (
            Method::POST,
            "/dns-records/import",
            r#"{"domain":"example.com","name":"app","record_type":"A"}"#,
        ),
        (
            Method::POST,
            "/delivery-profiles",
            r#"{"name":"Edge","provider_kind":"direct"}"#,
        ),
        (Method::DELETE, "/delivery-profiles/9", ""),
        (
            Method::POST,
            "/projects/1/domain-delivery-bindings/preview",
            r#"{"hostname":"app.example.com","environment_id":2,"dns_provider_id":3,"zone":"example.com","origin_target":"192.0.2.1","delivery_profile_id":null}"#,
        ),
        (
            Method::POST,
            "/projects/1/domain-delivery-bindings/apply",
            r#"{"preview_id":"00000000-0000-4000-8000-000000000000","adopt_records":[]}"#,
        ),
        (Method::DELETE, "/projects/1/domain-delivery-bindings/5", ""),
    ]
}

#[tokio::test]
async fn dns_and_delivery_writes_without_authentication_return_unauthorized() {
    for (method, uri, body) in guarded_write_endpoints() {
        let status = router()
            .oneshot(unauthenticated_request(method.clone(), uri, body))
            .await
            .expect("router response")
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn dns_and_delivery_writes_without_dns_permissions_return_forbidden() {
    // A project writer with no DNS rights at all.
    for (method, uri, body) in guarded_write_endpoints() {
        let status = request(
            method.clone(),
            uri,
            vec![Permission::ProjectsRead, Permission::ProjectsWrite],
            body,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn dns_and_delivery_writes_without_automation_permission_return_forbidden() {
    // DNS provider writers still need the automation grant for these.
    for (method, uri, body) in guarded_write_endpoints() {
        let status = request(
            method.clone(),
            uri,
            vec![
                Permission::DnsProvidersRead,
                Permission::DnsProvidersWrite,
                Permission::ProjectsRead,
                Permission::ProjectsWrite,
            ],
            body,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn delivery_binding_writes_without_project_write_permission_return_forbidden() {
    for (method, uri, body) in guarded_write_endpoints()
        .into_iter()
        .filter(|(_, uri, _)| uri.starts_with("/projects/"))
    {
        let status = request(
            method.clone(),
            uri,
            vec![
                Permission::DnsProvidersRead,
                Permission::DnsProvidersWrite,
                Permission::DnsAutomationWrite,
                Permission::ProjectsRead,
            ],
            body,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn failed_hostname_mode_apply_is_audited_with_what_it_changed() {
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_entities::{dns_managed_domains, dns_providers};

    let Some(test_db) =
        migrated_database("failed_hostname_mode_apply_is_audited_with_what_it_changed").await
    else {
        return;
    };
    let db = test_db.db.clone();
    let encryption = test_encryption();
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
    let managed = dns_managed_domains::ActiveModel {
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
    let audit = Arc::new(RecordingAudit::default());

    // The zone does not govern the default preview domain, so the apply is
    // refused before it writes anything.
    let response = router_with_audit(db.clone(), encryption, audit.clone())
        .oneshot(request_for(
            Method::POST,
            format!(
                "/dns-providers/{}/domains/example.com/apply-hostname-mode",
                provider.id
            ),
            vec![
                Permission::DnsProvidersWrite,
                Permission::DnsAutomationWrite,
            ],
            Body::from(r#"{"mode":"flat","sync_dns":true}"#),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let records = audit.records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record["action"], "DNS_HOSTNAME_MODE_APPLY_FAILED");
    assert_eq!(record["provider_id"], provider.id);
    assert_eq!(record["domain"], "example.com");
    assert_eq!(record["context"]["user_id"], 42);
    let details = &record["details"];
    assert_eq!(details["mode"], "flat");
    assert_eq!(details["sync_dns"], true);
    assert_eq!(details["outcome"], "failed");
    assert_eq!(details["saved"], "nothing");
    assert_eq!(details["completed_changes"], serde_json::json!([]));
    let error = details["error"].as_str().unwrap_or_default();
    assert!(error.contains("does not govern"), "{error}");

    let stored = dns_managed_domains::Entity::find_by_id(managed.id)
        .one(db.as_ref())
        .await
        .expect("read managed domain")
        .expect("managed domain still exists");
    assert_eq!(stored.generated_hostname_mode, "standard");
}

#[tokio::test]
async fn failed_managed_record_writes_are_audited() {
    let Some(test_db) = migrated_database("failed_managed_record_writes_are_audited").await else {
        return;
    };
    let audit = Arc::new(RecordingAudit::default());
    // No provider manages the zone, so each write fails before it reaches one.
    let attempts = [
        (
            Method::POST,
            "/dns-records",
            r#"{"domain":"unmanaged.example","name":"app","content":{"type":"A","value":{"address":"203.0.113.10"}}}"#,
        ),
        (
            Method::DELETE,
            "/dns-records?domain=unmanaged.example&name=app&record_type=A",
            "",
        ),
        (
            Method::POST,
            "/dns-records/import",
            r#"{"domain":"unmanaged.example","name":"app","record_type":"A"}"#,
        ),
    ];
    for (method, uri, body) in attempts {
        let response = router_with_audit(test_db.db.clone(), test_encryption(), audit.clone())
            .oneshot(request_for(
                method.clone(),
                uri,
                vec![
                    Permission::DnsProvidersWrite,
                    Permission::DnsAutomationWrite,
                ],
                Body::from(body),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }

    let records = audit.records();
    let actions: Vec<&str> = records
        .iter()
        .map(|record| record["action"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        actions,
        [
            "MANAGED_DNS_RECORD_SET_FAILED",
            "MANAGED_DNS_RECORD_REMOVE_FAILED",
            "MANAGED_DNS_RECORD_IMPORT_FAILED",
        ]
    );
    for record in &records {
        assert_eq!(record["domain"], "unmanaged.example", "{record}");
        assert_eq!(record["name"], "app", "{record}");
        assert_eq!(record["record_type"], "A", "{record}");
        assert_eq!(record["context"]["user_id"], 42, "{record}");
        let error = record["error"].as_str().unwrap_or_default();
        assert!(error.contains("unmanaged.example"), "{record}");
    }
    // Only a set can fail after changing the record, so only a set says
    // whether it did.
    assert_eq!(records[0]["record_written"], false);
    assert!(records[1].get("record_written").is_none());
    assert!(records[2].get("record_written").is_none());
}
