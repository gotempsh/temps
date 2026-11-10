// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::broker::UnavailableTokenlessBroker;
use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
use std::collections::BTreeMap;

pub(crate) fn encryption() -> Arc<EncryptionService> {
    Arc::new(EncryptionService::new(&"ab".repeat(32)).unwrap())
}
fn service(db: MockDatabase) -> ServiceAccessService {
    ServiceAccessService::new(
        Arc::new(db.into_connection()),
        encryption(),
        Arc::new(UnavailableTokenlessBroker),
    )
}
fn db() -> MockDatabase {
    MockDatabase::new(DatabaseBackend::Postgres)
}
// Service saves execute the management lock and immutable scope insert.
fn service_write_db() -> MockDatabase {
    db().append_exec_results((0..2).map(|_| sea_orm::MockExecResult {
        last_insert_id: 0,
        rows_affected: 0,
    }))
}
fn project_row() -> BTreeMap<String, sea_orm::Value> {
    BTreeMap::from([("id".into(), 7.into())])
}
fn request() -> SaveAccessService {
    SaveAccessService {
        vault_id: None,
        name: " API ".into(),
        policy: policy(),
        max_ttl_seconds: 900,
        enabled: true,
        credential: None,
        clear_credential: false,
        expected_version: None,
    }
}
fn policy() -> SandboxAccessPolicy {
    SandboxAccessPolicy {
        provider: None,
        github_repository: None,
        origin: "https://api.example.com".into(),
        path_prefixes: vec!["/v1".into()],
        methods: vec![AccessMethod::Get],
    }
}
fn row() -> services::Model {
    services::Model {
        retired_at: None,
        id: "svc_test".into(),
        project_id: Some(7),
        name: "API".into(),
        policy: serde_json::to_value(policy()).unwrap(),
        max_ttl_seconds: 900,
        enabled: true,
        credential_encrypted: None,
        version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
fn grant() -> grants::Model {
    let now = Utc::now();
    grants::Model {
        assignment_id: None,
        id: "grant_test".into(),
        project_id: Some(7),
        service_id: "svc_test".into(),
        sandbox_id: 42,
        service_version: 1,
        policy_snapshot: serde_json::to_value(policy()).unwrap(),
        created_by: Some(1),
        version: 1,
        created_at: now,
        expires_at: now + Duration::seconds(60),
        revoked_at: None,
    }
}
fn sandbox() -> sandboxes::Model {
    sandboxes::Model {
        id: 42,
        public_id: "sbx_test".into(),
        user_id: Some(1),
        agent_run_id: None,
        name: "test".into(),
        status: "running".into(),
        lifecycle: "workspace".into(),
        project_id: Some(7),
        source_repo_url: None,
        image: None,
        work_dir: "/workspace".into(),
        timeout_secs: 900,
        metadata: None,
        backend: Some("docker".into()),
        node_id: None,
        egress_policy: Default::default(),
        created_at: Utc::now(),
        last_activity_at: Utc::now(),
        expires_at: Utc::now() + Duration::seconds(900),
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: vec![],
    }
}
fn grant_request() -> RequestServiceGrant {
    RequestServiceGrant {
        service_id: "svc_test".into(),
        service_version: 1,
        ttl_seconds: 60,
    }
}
fn page() -> AccessPageQuery {
    AccessPageQuery {
        page: None,
        page_size: None,
    }
}

#[test]
fn service_save_rejects_destinations_the_sdk_cannot_represent() {
    let mut req = request();
    // Individually valid DNS labels can still exceed the SDK's total host bound.
    req.policy.origin = format!("https://{}", vec!["a".repeat(63); 5].join("."));
    assert!(matches!(
        validate_service(&mut req),
        Err(AccessError::Validation {
            field: "origin",
            ..
        })
    ));
}

#[test]
fn validates_and_normalizes_service_policy() {
    let mut req = request();
    validate_service(&mut req).unwrap();
    assert_eq!(req.name, "API");
    for origin in [
        "http://api.example.com",
        "https://user:password@api.example.com",
        "https://api.example.com/path",
        "https://127.0.0.1",
        "https://[::1]",
        "https://localhost",
        "https://api.example.com:8443",
        "https://api.example.com?key=value",
        "https://api.example.com#fragment",
        "https://*.example.com",
    ] {
        let mut req = request();
        req.policy.origin = origin.into();
        assert!(validate_service(&mut req).is_err(), "{origin}");
    }
    for path in [
        "relative",
        "/../v1",
        "/v1/%2f",
        "/v1?key=value",
        "/v1/*",
        "/v1//other",
    ] {
        let mut req = request();
        req.policy.path_prefixes = vec![path.into()];
        assert!(validate_service(&mut req).is_err(), "{path}");
    }
}
#[test]
fn rejects_secret_and_lifetime_boundaries() {
    for ttl in [0, 59, 86401, i32::MAX] {
        let mut req = request();
        req.max_ttl_seconds = ttl;
        assert!(validate_service(&mut req).is_err());
    }
    for ttl in [60, 86400] {
        let mut req = request();
        req.max_ttl_seconds = ttl;
        validate_service(&mut req).unwrap();
    }
    for secret in ["".into(), "a\nb".into(), "a b".into(), "x".repeat(16385)] {
        let mut req = request();
        req.credential = Some(temps_core::secret_string::SecretString::new(secret));
        assert!(validate_service(&mut req).is_err());
    }
    let mut req = request();
    req.credential = Some("test-value".into());
    req.clear_credential = true;
    assert!(validate_service(&mut req).is_err());
    let mut req = request();
    req.name = " ".into();
    assert!(validate_service(&mut req).is_err());
    let mut req = request();
    req.policy.methods.clear();
    assert!(validate_service(&mut req).is_err());
    let mut req = request();
    req.policy.path_prefixes.clear();
    assert!(validate_service(&mut req).is_err());
}
#[test]
fn intent_status_does_not_claim_enforcement_and_expiry_is_exact() {
    let s = row();
    let g = grant();
    let now = g.created_at;
    let result = grant_response(g.clone(), &s, "sbx_test", now).unwrap();
    assert!(!result.connected);
    assert_eq!(result.status, ServiceGrantStatus::PendingApproval);
    assert!(result.reason.contains("has not been approved"));
    assert_eq!(
        grant_response(g.clone(), &s, "sbx_test", g.expires_at)
            .unwrap()
            .status,
        ServiceGrantStatus::Expired
    );
    let mut disabled = s.clone();
    disabled.enabled = false;
    assert_eq!(
        grant_response(g.clone(), &disabled, "sbx_test", now)
            .unwrap()
            .status,
        ServiceGrantStatus::ServiceDisabled
    );
    let mut changed = s;
    changed.version = 2;
    assert_eq!(
        grant_response(g.clone(), &changed, "sbx_test", now)
            .unwrap()
            .status,
        ServiceGrantStatus::PolicyChanged
    );
    let mut revoked = g;
    revoked.revoked_at = Some(now);
    assert_eq!(
        grant_response(revoked, &changed, "sbx_test", now)
            .unwrap()
            .status,
        ServiceGrantStatus::Revoked
    );
}
#[test]
fn pagination_and_secret_response_are_bounded() {
    assert_eq!(page().normalized(), (1, 20));
    assert_eq!(
        AccessPageQuery {
            page: Some(0),
            page_size: Some(500)
        }
        .normalized(),
        (1, 100)
    );
    let mut s = row();
    s.credential_encrypted = Some("ciphertext-must-not-be-returned".into());
    let response = serde_json::to_string(&service_response(s).unwrap()).unwrap();
    assert!(response.contains("\"has_credential\":true"));
    assert!(!response.contains("ciphertext"));
}
#[tokio::test]
async fn list_not_found_and_database_failures_are_contextual() {
    let s = service(db().append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]));
    assert!(matches!(
        s.list_services(7, page()).await,
        Err(AccessError::NotFound {
            project_id: Some(7),
            ..
        })
    ));
    let s = service(db().append_query_errors([DbErr::Custom("sensitive-driver-message".into())]));
    let err = s.list_services(7, page()).await.unwrap_err();
    assert!(!err.to_string().contains("sensitive-driver-message"));
    assert!(err.to_string().contains("project 7"));
}
#[tokio::test]
async fn saves_only_encrypted_credentials_and_supports_rotation() {
    let mut saved = row();
    saved.credential_encrypted = Some("encrypted".into());
    let s = service(
        service_write_db()
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![saved.clone()]]),
    );
    let mut req = request();
    req.credential = Some("test-bearer-value".into());
    let response = s.save_service(7, None, req).await.unwrap();
    assert!(response.has_credential);
    let db = Arc::try_unwrap(s.db).unwrap();
    let log = format!("{:?}", db.into_transaction_log());
    assert!(!log.contains("test-bearer-value"));
    assert!(log.contains("credential_encrypted"));
    assert!(log.contains("INSERT INTO"));
    let mut updated = saved.clone();
    updated.version = 2;
    updated.credential_encrypted = None;
    let s = service(db_with_update(saved, updated));
    let mut req = request();
    req.clear_credential = true;
    req.expected_version = Some(1);
    assert!(
        !s.save_service(7, Some("svc_test".into()), req)
            .await
            .unwrap()
            .has_credential
    );
}
fn db_with_update(old: services::Model, new: services::Model) -> MockDatabase {
    service_write_db()
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![old], vec![new]])
}
#[tokio::test]
async fn service_update_detects_stale_versions_and_missing_rows() {
    let s = service(
        service_write_db()
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![row()]]),
    );
    let mut req = request();
    req.expected_version = Some(2);
    assert!(matches!(
        s.save_service(7, Some("svc_test".into()), req).await,
        Err(AccessError::Conflict { version: 2, .. })
    ));
    let s = service(
        service_write_db()
            .append_query_results([vec![project_row()]])
            .append_query_results([Vec::<services::Model>::new()]),
    );
    let mut req = request();
    req.expected_version = Some(1);
    assert!(
        matches!(s.save_service(7,Some("missing".into()),req).await,Err(AccessError::NotFound{id,..}) if id=="missing")
    );
}
#[tokio::test]
async fn grant_creation_pins_policy_and_never_provisions() {
    let s = service(
        db().append_query_results([vec![row()]])
            .append_query_results([vec![sandbox()]])
            .append_query_results([vec![grant()]]),
    );
    let result = s
        .request_grant(7, "sbx_test", 1, false, grant_request())
        .await
        .unwrap();
    assert_eq!(result.status, ServiceGrantStatus::PendingApproval);
    assert_eq!(result.policy, policy());
    assert!(!result.connected);
    let log = format!(
        "{:?}",
        Arc::try_unwrap(s.db).unwrap().into_transaction_log()
    );
    assert!(log.contains("user_id"));
    assert!(log.contains("project_id"));
    assert!(log.contains("FOR UPDATE"));
}
#[tokio::test]
async fn grant_creation_rejects_wrong_owner_stale_disabled_and_excess_ttl() {
    let s = service(
        db().append_query_results([vec![row()]])
            .append_query_results([Vec::<sandboxes::Model>::new()]),
    );
    assert!(
        matches!(s.request_grant(7,"sbx_other",1,false,grant_request()).await,Err(AccessError::NotFound{id,..}) if id=="sbx_other")
    );
    for (version, enabled, ttl) in [(2, true, 60), (1, false, 60), (1, true, 901)] {
        let mut r = row();
        r.enabled = enabled;
        let s = service(db().append_query_results([vec![r]]));
        let mut req = grant_request();
        req.service_version = version;
        req.ttl_seconds = ttl;
        assert!(s.request_grant(7, "sbx_test", 1, false, req).await.is_err());
    }
}
#[tokio::test]
async fn revoke_is_scoped_versioned_and_idempotent() {
    let g = grant();
    let mut revoked = g.clone();
    revoked.version = 2;
    revoked.revoked_at = Some(Utc::now());
    let s = service(
        db().append_query_results([vec![sandbox()]])
            .append_query_results([vec![g]])
            .append_query_results([vec![row()]])
            .append_query_results([vec![revoked.clone()]]),
    );
    let response = s
        .revoke_grant(7, "sbx_test", "grant_test", 1, false, 1)
        .await
        .unwrap();
    assert_eq!(response.status, ServiceGrantStatus::Revoked);
    assert_eq!(response.enforcement, GrantEnforcement::Stopping);
    assert!(!response.connected);
    assert_eq!(response.version, 2);
    let s = service(
        db().append_query_results([vec![sandbox()]])
            .append_query_results([vec![revoked]])
            .append_query_results([vec![row()]]),
    );
    assert_eq!(
        s.revoke_grant(7, "sbx_test", "grant_test", 1, false, 2)
            .await
            .unwrap()
            .version,
        2
    );
    let s = service(
        db().append_query_results([vec![sandbox()]])
            .append_query_results([vec![grant()]]),
    );
    assert!(matches!(
        s.revoke_grant(7, "sbx_test", "grant_test", 1, false, 9)
            .await,
        Err(AccessError::Conflict { .. })
    ));
}
#[tokio::test]
async fn list_services_uses_bounded_page_and_stable_order() {
    let s = service(
        db().append_query_results([vec![project_row()]])
            .append_query_results([vec![BTreeMap::from([(
                "num_items".to_string(),
                1_i64.into(),
            )])]])
            .append_query_results([vec![row()]]),
    );
    assert_eq!(s.list_services(7, page()).await.unwrap().items.len(), 1);
    let log = format!(
        "{:?}",
        Arc::try_unwrap(s.db).unwrap().into_transaction_log()
    );
    assert!(log.contains("LIMIT"));
    assert!(log.contains("created_at"));
}
#[tokio::test]
async fn list_grants_joins_services_without_n_plus_one() {
    let s = service(
        db().append_query_results([vec![sandbox()]])
            .append_query_results([vec![BTreeMap::from([(
                "num_items".to_string(),
                1_i64.into(),
            )])]])
            .append_query_results([vec![(grant(), row())]])
            .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]),
    );
    let result = s
        .list_grants(7, "sbx_test", 1, false, page())
        .await
        .unwrap();
    assert_eq!(result.total, 1);
    assert!(!result.items[0].connected);
}

#[tokio::test]
async fn write_failures_roll_back_without_leaking_driver_details() {
    let s = service(
        service_write_db()
            .append_query_results([vec![project_row()]])
            .append_query_errors([DbErr::Custom("secret-write-error".into())]),
    );
    let error = s.save_service(7, None, request()).await.unwrap_err();
    assert!(matches!(
        error,
        AccessError::Database {
            operation: "save service",
            project_id: Some(7),
            ..
        }
    ));
    assert!(!error.to_string().contains("secret-write-error"));
    let log = format!(
        "{:?}",
        Arc::try_unwrap(s.db).unwrap().into_transaction_log()
    );
    assert!(log.contains("ROLLBACK"));
}

#[tokio::test]
async fn grants_missing_resources_and_storage_failures_explain_the_operation() {
    let s = service(db().append_query_results([Vec::<services::Model>::new()]));
    assert!(
        matches!(s.request_grant(7, "sbx_test", 1, false, grant_request()).await,
        Err(AccessError::NotFound { kind: "service", project_id: Some(7), id }) if id == "svc_test")
    );
    let s = service(db().append_query_errors([DbErr::Custom("read error".into())]));
    assert!(matches!(
        s.request_grant(7, "sbx_test", 1, false, grant_request())
            .await,
        Err(AccessError::Database {
            operation: "lock requested service",
            ..
        })
    ));
    let s = service(
        db().append_query_results([vec![sandbox()]])
            .append_query_results([Vec::<grants::Model>::new()]),
    );
    assert!(
        matches!(s.revoke_grant(7,"sbx_test","grant_missing",1,false,1).await,
        Err(AccessError::NotFound { kind: "grant", id, .. }) if id == "grant_missing")
    );
    let s = service(db().append_query_errors([DbErr::Custom("sandbox read error".into())]));
    assert!(matches!(
        s.revoke_grant(7, "sbx_test", "grant_test", 1, false, 1)
            .await,
        Err(AccessError::Database {
            operation: "find sandbox",
            ..
        })
    ));
    let s = service(db().append_query_results([Vec::<sandboxes::Model>::new()]));
    assert!(matches!(s.list_grants(7,"missing",1,false,page()).await,
        Err(AccessError::NotFound { kind: "sandbox", id, .. }) if id == "missing"));
    let s = service(db().append_query_errors([DbErr::Custom("sandbox read error".into())]));
    assert!(matches!(
        s.list_grants(7, "sbx_test", 1, false, page()).await,
        Err(AccessError::Database {
            operation: "find sandbox",
            ..
        })
    ));
}

#[tokio::test]
async fn destroyed_sandboxes_and_invalid_request_lifetimes_are_rejected() {
    let mut destroyed = sandbox();
    destroyed.status = "destroyed".into();
    let s = service(
        db().append_query_results([vec![row()]])
            .append_query_results([vec![destroyed]]),
    );
    assert!(matches!(
        s.request_grant(7, "sbx_test", 1, false, grant_request())
            .await,
        Err(AccessError::Validation {
            field: "sandbox_id",
            ..
        })
    ));
    for ttl in [0, 59, 86401] {
        let s = service(db());
        let mut req = grant_request();
        req.ttl_seconds = ttl;
        assert!(matches!(
            s.request_grant(7, "sbx_test", 1, false, req).await,
            Err(AccessError::Validation {
                field: "ttl_seconds",
                ..
            })
        ));
    }
}

#[test]
fn corrupted_policy_is_a_contextual_error_instead_of_an_empty_scope() {
    let mut service = row();
    service.policy = serde_json::json!({"unexpected": true});
    assert!(
        matches!(service_response(service), Err(AccessError::StoredPolicy { project_id: Some(7), id, .. }) if id == "svc_test")
    );
}

#[tokio::test]
async fn single_service_read_scopes_query_and_handles_storage_errors() {
    let svc = service(
        db().append_query_results([vec![project_row()]])
            .append_query_results([vec![row()]]),
    );
    assert_eq!(svc.get_service(7, "svc_test").await.unwrap().id, "svc_test");
    let log = format!(
        "{:?}",
        Arc::try_unwrap(svc.db).unwrap().into_transaction_log()
    );
    assert!(log.contains("project_id"));
    assert!(log.contains("svc_test"));
    let svc = service(
        db().append_query_results([vec![project_row()]])
            .append_query_results([Vec::<services::Model>::new()]),
    );
    assert!(
        matches!(svc.get_service(7, "missing").await, Err(AccessError::NotFound { id, project_id: Some(7), .. }) if id == "missing")
    );
    let svc = service(
        db().append_query_results([vec![project_row()]])
            .append_query_errors([DbErr::Custom("private driver details".into())]),
    );
    assert!(matches!(
        svc.get_service(7, "svc_test").await,
        Err(AccessError::Database {
            operation: "read service",
            ..
        })
    ));
}

#[tokio::test]
async fn stored_bearer_cannot_follow_an_origin_change() {
    let mut old = row();
    old.credential_encrypted = Some(encryption().encrypt_string("original-test-secret").unwrap());
    let s = service(
        service_write_db()
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![old]]),
    );
    let mut req = request();
    req.expected_version = Some(1);
    req.policy.origin = "https://other.example.com".into();
    assert!(matches!(
        s.save_service(7, Some("svc_test".into()), req).await,
        Err(AccessError::Validation {
            field: "credential",
            ..
        })
    ));
    let log = format!(
        "{:?}",
        Arc::try_unwrap(s.db).unwrap().into_transaction_log()
    );
    assert!(
        !log.contains(" SET "),
        "an unsafe edit must roll back before writing"
    );
}

#[tokio::test]
async fn origin_edits_allow_explicit_replacement_or_clearing_and_same_origin_retains_secret() {
    for (origin, replacement, clear) in [
        (
            "https://other.example.com",
            Some("replacement-test-secret"),
            false,
        ),
        ("https://other.example.com", None, true),
        ("https://API.EXAMPLE.COM:443/", None, false),
    ] {
        let mut old = row();
        old.credential_encrypted =
            Some(encryption().encrypt_string("original-test-secret").unwrap());
        let mut updated = old.clone();
        updated.version = 2;
        let mut req = request();
        req.expected_version = Some(1);
        req.policy.origin = origin.into();
        req.credential = replacement.map(Into::into);
        req.clear_credential = clear;
        let s = service(db_with_update(old, updated));
        s.save_service(7, Some("svc_test".into()), req)
            .await
            .unwrap();
        let log = format!(
            "{:?}",
            Arc::try_unwrap(s.db).unwrap().into_transaction_log()
        );
        assert!(log.contains("UPDATE"));
        assert_eq!(
            log.split(" SET ")
                .nth(1)
                .unwrap()
                .split(" WHERE ")
                .next()
                .unwrap()
                .contains("credential_encrypted"),
            replacement.is_some() || clear
        );
        assert!(!log.contains("original-test-secret"));
        assert!(!log.contains("replacement-test-secret"));
    }
}
