// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn scope_change_ignores_order_and_duplicates_and_reports_the_new_scope() {
    assert_eq!(
        scope_change(vec![2, 1], vec!["web".into()], &[1, 2, 2], &["web".into()]),
        None
    );
    assert_eq!(
        scope_change(vec![1], vec![], &[3, 1], &["worker".into(), "api".into()]),
        Some(serde_json::json!({
            "environment_ids": [1, 3],
            "compose_services": ["api", "worker"],
        }))
    );
    assert_eq!(
        scope_change(vec![1], vec![], &[], &[]),
        Some(serde_json::json!({"environment_ids": [], "compose_services": []})),
        "widening to every environment is a scope change"
    );
}
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

fn make_encryption_service() -> Arc<EncryptionService> {
    Arc::new(
        EncryptionService::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap(),
    )
}

fn make_secret_model(id: i32, project_id: i32, key: &str, value: &str) -> secrets::Model {
    secrets::Model {
        id,
        project_id,
        environment_id: None,
        key: key.to_string(),
        value: value.to_string(),
        include_in_preview: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn test_validate_secret_key_accepts_valid() {
    assert!(validate_secret_key("DB_PASSWORD").is_ok());
    assert!(validate_secret_key("_underscore").is_ok());
    assert!(validate_secret_key("api_key_2").is_ok());
    assert!(validate_secret_key("A").is_ok());
}

#[test]
fn test_validate_secret_key_rejects_empty() {
    let err = validate_secret_key("").unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
}

#[test]
fn test_validate_secret_key_rejects_leading_digit() {
    let err = validate_secret_key("1FOO").unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
}

#[test]
fn test_validate_secret_key_rejects_special_chars() {
    let err = validate_secret_key("FOO-BAR").unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
    let err = validate_secret_key("FOO.BAR").unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
    let err = validate_secret_key("FOO/BAR").unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
}

#[test]
fn test_validate_secret_key_rejects_over_255_chars() {
    let long = "A".repeat(256);
    let err = validate_secret_key(&long).unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
}

#[test]
fn test_secret_scope_allows_same_key_in_disjoint_environments() {
    assert!(!secret_scope_overlaps(&[2], &[1]));
}

#[test]
fn test_secret_scope_rejects_shared_environment() {
    assert!(secret_scope_overlaps(&[1, 2], &[2, 3]));
}

#[test]
fn test_secret_scope_rejects_global_overlap_in_both_directions() {
    assert!(secret_scope_overlaps(&[], &[1]));
    assert!(secret_scope_overlaps(&[1], &[]));
}

#[test]
fn test_encrypt_decrypt_roundtrip() {
    let svc = make_encryption_service();
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = SecretService::new(db, svc);

    let plaintext = "my_super_secret_password_123";
    let encrypted = service.encrypt_value("DB_PASSWORD", plaintext).unwrap();
    assert_ne!(encrypted, plaintext);

    let decrypted = service.decrypt_value(1, "DB_PASSWORD", &encrypted).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_decrypt_invalid_returns_typed_error() {
    let svc = make_encryption_service();
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = SecretService::new(db, svc);

    let err = service
        .decrypt_value(7, "KEY", "not-valid-base64!!!")
        .unwrap_err();
    assert!(matches!(
        err,
        SecretError::DecryptionFailed { secret_id: 7, .. }
    ));
}

#[tokio::test]
async fn test_create_rejects_value_over_limit() {
    let svc = make_encryption_service();
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = SecretService::new(db, svc);

    let big = "x".repeat(SECRET_VALUE_MAX_BYTES + 1);
    let err = service
        .create(10, vec![], "BIG_SECRET".to_string(), big, false, vec![])
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SecretError::ValueTooLarge {
            size, limit, ..
        } if size == SECRET_VALUE_MAX_BYTES + 1 && limit == SECRET_VALUE_MAX_BYTES
    ));
}

#[tokio::test]
async fn test_create_rejects_invalid_key() {
    let svc = make_encryption_service();
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = SecretService::new(db, svc);

    let err = service
        .create(
            10,
            vec![],
            "bad-key!".to_string(),
            "v".to_string(),
            false,
            vec![],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
}

#[tokio::test]
async fn refusals_of_oversized_names_do_not_echo_them_in_full() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = SecretService::new(db, make_encryption_service());
    let huge = "s".repeat(200_000);

    let err = service
        .create(
            10,
            vec![],
            "API_TOKEN".to_string(),
            "v".to_string(),
            false,
            vec![huge.clone()],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SecretError::InvalidComposeService { .. }));
    let message = err.to_string();
    assert!(message.len() < 512, "{} bytes", message.len());
    assert!(message.contains("exceeds 255"), "{message}");
    assert!(message.contains('…'), "{message}");

    let err = service
        .create(10, vec![], huge, "v".to_string(), false, vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, SecretError::InvalidKey { .. }));
    let message = err.to_string();
    assert!(message.len() < 512, "{} bytes", message.len());
    assert!(message.contains("exceeds 255"), "{message}");
}

#[test]
fn a_refused_character_beyond_the_excerpt_is_not_quoted() {
    // 64 allowed characters, then the refused one: the excerpt stops before it.
    let past = format!("{}/", "s".repeat(64));
    let message = validate_compose_service_name(&past)
        .unwrap_err()
        .to_string();
    assert!(!message.contains('/'), "{message}");
    assert!(
        message.contains("invalid character at position 65"),
        "{message}"
    );
    let key_past = format!("{}/", "K".repeat(64));
    let message = validate_secret_key(&key_past).unwrap_err().to_string();
    assert!(!message.contains('/'), "{message}");
    assert!(
        message.contains("invalid character at position 65"),
        "{message}"
    );

    // The last character the excerpt shows is still quoted.
    let inside = format!("{}/", "s".repeat(63));
    let message = validate_compose_service_name(&inside)
        .unwrap_err()
        .to_string();
    assert!(message.contains("invalid character '/'"), "{message}");
    let key_inside = format!("{}/", "K".repeat(63));
    let message = validate_secret_key(&key_inside).unwrap_err().to_string();
    assert!(message.contains("invalid character '/'"), "{message}");
}

#[test]
fn short_refused_names_are_echoed_whole() {
    let err = validate_compose_service_name("web/api").unwrap_err();
    assert_eq!(
            err.to_string(),
            "Invalid compose service name 'web/api': invalid character '/' (allowed: A-Z, a-z, 0-9, '.', '_', '-')"
        );
    assert_eq!(echoed(&"x".repeat(64)), "x".repeat(64));
    assert_eq!(echoed(&"x".repeat(65)), format!("{}…", "x".repeat(64)));
}

#[tokio::test]
async fn test_create_rejects_duplicate_key() {
    let svc = make_encryption_service();
    let existing = make_secret_model(1, 10, "API_KEY", "cipher");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![(
                existing,
                Option::<secret_environments::Model>::None,
            )]])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let err = service
        .create(
            10,
            vec![],
            "API_KEY".to_string(),
            "new_value".to_string(),
            false,
            vec![],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SecretError::KeyAlreadyExists { .. }));
}

#[tokio::test]
async fn test_create_propagates_key_scope_lock_database_error() {
    let svc = make_encryption_service();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom(
                "advisory lock unavailable".to_string(),
            )])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let err = service
        .create(
            10,
            vec![],
            "API_KEY".to_string(),
            "value".to_string(),
            false,
            vec![],
        )
        .await
        .unwrap_err();

    assert!(matches!(err, SecretError::Database(_)));
}

/// The row a paginator's `COUNT(*)` query answers with.
fn count_row(n: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
    std::collections::BTreeMap::from([("num_items".to_string(), sea_orm::Value::BigInt(Some(n)))])
}

#[tokio::test]
async fn test_list_empty_returns_empty() {
    let svc = make_encryption_service();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(0)]])
            .append_query_results(vec![Vec::<secrets::Model>::new()])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);
    let (out, total) = service
        .list_page(10, None, PageQuery::default())
        .await
        .unwrap();
    assert!(out.is_empty());
    assert_eq!(total, 0);
}

#[tokio::test]
async fn get_returns_one_secret_or_a_not_found_naming_it() {
    let svc = make_encryption_service();
    let row = make_secret_model(4, 10, "TOKEN", "ciphertext_blob");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![Vec::<(
                secret_environments::Model,
                Option<environments::Model>,
            )>::new()])
            .append_query_results(vec![Vec::<secret_compose_services::Model>::new()])
            .append_query_results(vec![Vec::<secrets::Model>::new()])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);
    let secret = service.get(10, 4).await.unwrap();
    assert_eq!((secret.id, secret.key.as_str()), (4, "TOKEN"));
    assert!(matches!(
        service.get(10, 5).await,
        Err(SecretError::NotFound {
            secret_id: 5,
            project_id: 10
        })
    ));
}

#[tokio::test]
async fn test_list_does_not_expose_ciphertext() {
    // list() returns SecretWithEnvironments, which has NO value field.
    // This test confirms the shape by pattern-matching.
    let svc = make_encryption_service();
    let row = make_secret_model(1, 10, "TOKEN", "ciphertext_blob");
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(1)]])
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![Vec::<(
                secret_environments::Model,
                Option<environments::Model>,
            )>::new()])
            // Compose-service scopes: none, so the secret goes to every
            // service in the stack.
            .append_query_results(vec![Vec::<secret_compose_services::Model>::new()])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);
    let (out, total) = service
        .list_page(10, None, PageQuery::default())
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].key, "TOKEN");
    assert!(
        out[0].compose_services.is_empty(),
        "no scope rows must read as 'every service', not 'no services'"
    );
    // There is no `value` field on SecretWithEnvironments — ciphertext
    // never leaves the service boundary via list().
}

#[tokio::test]
async fn test_get_for_deploy_returns_decrypted_project_scoped() {
    let svc = make_encryption_service();
    let plaintext = "redis://user:pass@host:6379/0";
    let encrypted = svc.encrypt_string(plaintext).unwrap();
    let row = make_secret_model(1, 10, "REDIS_URL", &encrypted);

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![Vec::<secret_environments::Model>::new()])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let out = service.get_for_deploy(10, None).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out.get("REDIS_URL").map(|s| s.as_str()), Some(plaintext));
}

#[tokio::test]
async fn test_get_for_deploy_skips_env_scoped_when_no_env_requested() {
    let svc = make_encryption_service();
    let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string("v").unwrap());

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![vec![secret_environments::Model {
                id: 1,
                secret_id: 1,
                environment_id: 99,
                created_at: chrono::Utc::now(),
            }]])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let out = service.get_for_deploy(10, None).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn test_get_for_deploy_includes_matching_env_binding() {
    let svc = make_encryption_service();
    let plaintext = "bound-value";
    let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string(plaintext).unwrap());

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![vec![secret_environments::Model {
                id: 1,
                secret_id: 1,
                environment_id: 42,
                created_at: chrono::Utc::now(),
            }]])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let out = service.get_for_deploy(10, Some(42)).await.unwrap();
    assert_eq!(out.get("BOUND").map(|s| s.as_str()), Some(plaintext));
}

#[tokio::test]
async fn test_get_for_deploy_excludes_non_matching_env_binding() {
    let svc = make_encryption_service();
    let row = make_secret_model(1, 10, "BOUND", &svc.encrypt_string("v").unwrap());

    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![row]])
            .append_query_results(vec![vec![secret_environments::Model {
                id: 1,
                secret_id: 1,
                environment_id: 42,
                created_at: chrono::Utc::now(),
            }]])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);

    let out = service.get_for_deploy(10, Some(7)).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn test_delete_not_found_returns_typed_error() {
    let svc = make_encryption_service();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<secrets::Model>::new()])
            .into_connection(),
    );
    let service = SecretService::new(db, svc);
    let err = service.delete(10, 999).await.unwrap_err();
    assert!(matches!(
        err,
        SecretError::NotFound {
            secret_id: 999,
            project_id: 10
        }
    ));
}
