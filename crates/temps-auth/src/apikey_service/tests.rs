// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
use temps_entities::{api_keys, users};
use temps_test_utils::TestDatabase;

async fn setup_test_env() -> (TestDatabase, ApiKeyService, users::Model) {
    let db = TestDatabase::with_migrations().await.unwrap();

    // Generate unique email to avoid conflicts in parallel tests
    let test_id = uuid::Uuid::new_v4();
    let email = format!("test_{}@example.com", test_id);

    // Create a test user with unique email
    let user = users::ActiveModel {
        email: Set(email),
        name: Set("Test User".to_string()),
        email_verified: Set(true),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        mfa_enabled: Set(false),
        ..Default::default()
    };
    let user = user.insert(db.db.as_ref()).await.unwrap();

    // Keys are capped by their owner's current role, so the fixture owner
    // must actually hold the admin role the tests mint keys for.
    let user_service = crate::user_service::UserService::new(
        db.db.clone(),
        crate::mfa_guard::test_support::encryption(),
    );
    user_service.initialize_roles().await.unwrap();
    user_service
        .assign_role_by_type(user.id, temps_entities::types::RoleType::Admin)
        .await
        .unwrap();

    let api_key_service = ApiKeyService::new(db.db.clone());
    (db, api_key_service, user)
}

async fn create_test_api_key(
    db: &Arc<DbConnection>,
    user_id: i32,
    name: &str,
    role_type: &str,
) -> api_keys::Model {
    // Generate unique key_hash and key_prefix to avoid conflicts
    let unique_id = uuid::Uuid::new_v4();
    let api_key = api_keys::ActiveModel {
        name: Set(name.to_string()),
        key_hash: Set(format!("test_hash_{}", unique_id)),
        key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
        user_id: Set(user_id),
        role_type: Set(role_type.to_string()),
        permissions: Set(None),
        is_active: Set(true),
        expires_at: Set(Some(Utc::now() + Duration::days(365))),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    api_key.insert(db.as_ref()).await.unwrap()
}

#[tokio::test]
async fn api_key_scope_validation_precedes_database_and_retains_database_errors() {
    let service = ApiKeyService::new(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
    ));
    for (project_id, environment_id) in [(None, Some(9)), (Some(0), None)] {
        let error = service
            .create_api_key(
                1,
                CreateApiKeyRequest {
                    name: "invalid-scope".into(),
                    role_type: "reader".into(),
                    permissions: None,
                    expires_at: None,
                    project_id,
                    environment_id,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ApiKeyServiceError::ValidationError(_)));
    }
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("project lookup failed".into())])
        .into_connection();
    let service = ApiKeyService::new(Arc::new(db));
    let error = service
        .create_api_key(
            1,
            CreateApiKeyRequest {
                name: "project-scope".into(),
                role_type: "reader".into(),
                permissions: None,
                expires_at: None,
                project_id: Some(7),
                environment_id: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ApiKeyServiceError::DatabaseError(_)));
}

#[tokio::test]
async fn scoped_api_key_lifecycle_preserves_scope_and_permissions() {
    let (db, service, user) = setup_test_env().await;
    db.db.execute_unprepared("INSERT INTO projects(id,name,slug,repo_name,repo_owner,directory,main_branch,preset,created_at,updated_at) VALUES(7001,'scope-fixture','scope-fixture','app','example','.','main','static',now(),now()); INSERT INTO environments(id,name,slug,subdomain,host,project_id,created_at,updated_at,upstreams) VALUES(9001,'preview','preview','scope-preview','scope.example',7001,now(),now(),'[]')").await.unwrap();
    let make_request = |name: &str, project_id, environment_id| CreateApiKeyRequest {
        name: name.into(),
        role_type: "custom".into(),
        permissions: Some(vec!["deployments:create".into()]),
        expires_at: Some(Utc::now() + Duration::days(1)),
        project_id,
        environment_id,
    };
    let key = service
        .create_api_key(user.id, make_request("scoped", Some(7001), Some(9001)))
        .await
        .unwrap();
    assert!(key.api_key.starts_with("tkp_"));
    assert!(service.validate_api_key(&key.api_key).await.is_err());
    let auth = service
        .validate_api_key_with_scope(&key.api_key)
        .await
        .unwrap()
        .into_auth_context();
    assert_eq!(auth.project_id(), Some(7001));
    assert_eq!(auth.api_key_scope().unwrap().environment_id, Some(9001));
    assert!(auth.has_permission(&Permission::DeploymentsCreate));
    assert!(!auth.has_permission(&Permission::ProjectsWrite));
    assert!(!auth.is_instance_admin());
    let scoped_admin = crate::AuthContext::new_api_key(
        auth.user.clone().unwrap(),
        Some(Role::Admin),
        None,
        "scoped-admin".into(),
        99,
    )
    .with_api_key_scope(auth.api_key_scope().copied());
    assert!(scoped_admin.is_admin());
    assert!(!scoped_admin.is_instance_admin());
    let serialized = serde_json::to_string(&auth).unwrap();
    let restored: crate::AuthContext = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored.api_key_scope(), auth.api_key_scope());

    assert!(
        temps_authz::enforce_api_key_path(&auth, "/api/projects/7001/trigger-pipeline").is_ok()
    );
    assert!(
        temps_authz::enforce_api_key_path(&auth, "/api/projects/7002/trigger-pipeline").is_err()
    );
    use axum::extract::FromRequestParts;
    let mut parts = axum::http::Request::builder()
        .uri("/projects/7001/trigger-pipeline")
        .body(())
        .unwrap()
        .into_parts()
        .0;
    parts.extensions.insert(auth.clone());
    assert!(
        temps_authz::RequireAuth::from_request_parts(&mut parts, &())
            .await
            .is_ok()
    );
    parts
        .extensions
        .insert(axum::extract::OriginalUri("/api/settings".parse().unwrap()));
    assert_eq!(
        temps_authz::RequireAuth::from_request_parts(&mut parts, &())
            .await
            .err()
            .unwrap()
            .status_code,
        StatusCode::FORBIDDEN
    );
    let rotated = service.rotate_api_key(user.id, key.id).await.unwrap();
    assert!(rotated.api_key.starts_with("tkp_"));
    assert_eq!(
        (rotated.project_id, rotated.environment_id),
        (Some(7001), Some(9001))
    );
    assert!(service
        .validate_api_key_with_scope(&key.api_key)
        .await
        .is_err());
    assert!(service
        .validate_api_key_with_scope(&rotated.api_key)
        .await
        .is_ok());
    for (name, project, env) in [
        ("no-project", None, Some(9001)),
        ("missing", Some(7002), None),
        ("wrong-project", Some(7001), Some(9002)),
        ("invalid", Some(0), None),
    ] {
        assert!(
            service
                .create_api_key(user.id, make_request(name, project, env))
                .await
                .is_err(),
            "{name}"
        );
    }
    db.db
        .execute_unprepared("UPDATE environments SET deleted_at=now() WHERE id=9001")
        .await
        .unwrap();
    assert!(service
        .validate_api_key_with_scope(&rotated.api_key)
        .await
        .is_err());
    assert!(service
        .create_api_key(user.id, make_request("deleted", Some(7001), Some(9001)))
        .await
        .is_err());
}

// API Key Creation Tests

#[tokio::test]
async fn test_create_api_key_with_admin_role() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Admin API Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: Some(Utc::now() + Duration::days(30)),
    };

    let response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    assert_eq!(response.name, "Admin API Key");
    assert_eq!(response.role_type, "admin");
    assert!(response.api_key.starts_with("tk_"));
    assert_eq!(response.api_key.len(), 43); // tk_ + 40 chars
    assert!(response.permissions.is_none());
}

#[tokio::test]
async fn test_create_api_key_with_custom_role() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Custom API Key".to_string(),
        role_type: "custom".to_string(),
        permissions: Some(vec![
            "projects:read".to_string(),
            "deployments:read".to_string(),
        ]),
        expires_at: None,
    };

    let response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    assert_eq!(response.name, "Custom API Key");
    assert_eq!(response.role_type, "custom");
    assert!(response.permissions.is_some());
    let perms = response.permissions.unwrap();
    assert_eq!(perms.len(), 2);
    assert!(perms.contains(&"projects:read".to_string()));
}

#[tokio::test]
async fn test_create_api_key_custom_without_permissions_fails() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Invalid Custom Key".to_string(),
        role_type: "custom".to_string(),
        permissions: None,
        expires_at: None,
    };

    let result = api_key_service.create_api_key(user.id, request).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
}

#[tokio::test]
async fn test_create_api_key_with_invalid_role() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Invalid Role Key".to_string(),
        role_type: "invalid_role".to_string(),
        permissions: None,
        expires_at: None,
    };

    let result = api_key_service.create_api_key(user.id, request).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
}

#[tokio::test]
async fn test_create_api_key_duplicate_name_fails() {
    let (_db, api_key_service, user) = setup_test_env().await;

    // Create first key
    let request1 = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Duplicate Name".to_string(),
        role_type: "reader".to_string(),
        permissions: None,
        expires_at: None,
    };

    api_key_service
        .create_api_key(user.id, request1)
        .await
        .unwrap();

    // Try to create second key with same name
    let request2 = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Duplicate Name".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: None,
    };

    let result = api_key_service.create_api_key(user.id, request2).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
}

#[tokio::test]
async fn test_create_api_key_with_invalid_permission() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Invalid Permission Key".to_string(),
        role_type: "custom".to_string(),
        permissions: Some(vec!["invalid:permission".to_string()]),
        expires_at: None,
    };

    let result = api_key_service.create_api_key(user.id, request).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
}

// API Key Listing Tests

#[tokio::test]
async fn test_list_api_keys() {
    let (db, api_key_service, user) = setup_test_env().await;

    // Create multiple API keys
    create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
    create_test_api_key(&db.db, user.id, "Key 2", "reader").await;
    create_test_api_key(&db.db, user.id, "Key 3", "user").await;

    let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

    assert_eq!(response.total, 3);
    assert_eq!(response.api_keys.len(), 3);
    // Should be ordered by created_at DESC
    assert_eq!(response.api_keys[0].name, "Key 3");
    assert_eq!(response.api_keys[1].name, "Key 2");
    assert_eq!(response.api_keys[2].name, "Key 1");
}

#[tokio::test]
async fn test_list_api_keys_pagination() {
    let (db, api_key_service, user) = setup_test_env().await;

    // Create 5 API keys
    for i in 1..=5 {
        create_test_api_key(&db.db, user.id, &format!("Key {}", i), "reader").await;
    }

    // Get first page
    let page1 = api_key_service.list_api_keys(user.id, 1, 2).await.unwrap();
    assert_eq!(page1.total, 5);
    assert_eq!(page1.api_keys.len(), 2);

    // Get second page
    let page2 = api_key_service.list_api_keys(user.id, 2, 2).await.unwrap();
    assert_eq!(page2.total, 5);
    assert_eq!(page2.api_keys.len(), 2);

    // Get third page
    let page3 = api_key_service.list_api_keys(user.id, 3, 2).await.unwrap();
    assert_eq!(page3.total, 5);
    assert_eq!(page3.api_keys.len(), 1);
}

#[tokio::test]
async fn test_list_api_keys_empty() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

    assert_eq!(response.total, 0);
    assert_eq!(response.api_keys.len(), 0);
}

// API Key Retrieval Tests

#[tokio::test]
async fn test_get_api_key() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let response = api_key_service
        .get_api_key(user.id, api_key.id)
        .await
        .unwrap();

    assert_eq!(response.id, api_key.id);
    assert_eq!(response.name, "Test Key");
    assert_eq!(response.role_type, "admin");
}

#[tokio::test]
async fn test_get_api_key_not_found() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let result = api_key_service.get_api_key(user.id, 999999).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
}

#[tokio::test]
async fn test_get_api_key_wrong_user() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    // Try to get key with different user ID
    let result = api_key_service.get_api_key(user.id + 1, api_key.id).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
}

// API Key Update Tests

#[tokio::test]
async fn test_update_api_key_name() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Old Name", "admin").await;

    let request = UpdateApiKeyRequest {
        name: Some("New Name".to_string()),
        is_active: None,
        permissions: None,
        expires_at: None,
    };

    let response = api_key_service
        .update_api_key(user.id, api_key.id, request)
        .await
        .unwrap();

    assert_eq!(response.name, "New Name");
    assert!(response.is_active);
}

#[tokio::test]
async fn test_update_api_key_deactivate() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let request = UpdateApiKeyRequest {
        name: None,
        is_active: Some(false),
        permissions: None,
        expires_at: None,
    };

    let response = api_key_service
        .update_api_key(user.id, api_key.id, request)
        .await
        .unwrap();

    assert!(!response.is_active);
}

#[tokio::test]
async fn test_update_api_key_expiration() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let new_expiry = Utc::now() + Duration::days(7);
    let request = UpdateApiKeyRequest {
        name: None,
        is_active: None,
        permissions: None,
        expires_at: Some(Some(new_expiry)),
    };

    let response = api_key_service
        .update_api_key(user.id, api_key.id, request)
        .await
        .unwrap();

    assert!(response.expires_at.is_some());
    let expires_at = response.expires_at.unwrap();
    assert!((expires_at.timestamp() - new_expiry.timestamp()).abs() < 2);
}

#[tokio::test]
async fn test_update_api_key_duplicate_name_fails() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key1 = create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
    let _api_key2 = create_test_api_key(&db.db, user.id, "Key 2", "reader").await;

    let request = UpdateApiKeyRequest {
        name: Some("Key 2".to_string()), // Try to rename to existing name
        is_active: None,
        permissions: None,
        expires_at: None,
    };

    let result = api_key_service
        .update_api_key(user.id, api_key1.id, request)
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
}

#[tokio::test]
async fn test_update_api_key_not_found() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = UpdateApiKeyRequest {
        name: Some("New Name".to_string()),
        is_active: None,
        permissions: None,
        expires_at: None,
    };

    let result = api_key_service
        .update_api_key(user.id, 999999, request)
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
}

// API Key Deletion Tests

#[tokio::test]
async fn test_delete_api_key() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    api_key_service
        .delete_api_key(user.id, api_key.id)
        .await
        .unwrap();

    // Verify key is deleted
    let result = api_key_service.get_api_key(user.id, api_key.id).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_api_key_not_found() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let result = api_key_service.delete_api_key(user.id, 999999).await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
}

#[tokio::test]
async fn test_delete_api_key_wrong_user() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let result = api_key_service
        .delete_api_key(user.id + 1, api_key.id)
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
}

// API Key Rotation Tests

#[tokio::test]
async fn test_rotate_api_key_invalidates_old_secret_and_issues_new_one() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let create_response = api_key_service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                project_id: None,
                environment_id: None,
                name: "Rotate Me".to_string(),
                role_type: "admin".to_string(),
                permissions: None,
                expires_at: Some(Utc::now() + Duration::days(30)),
            },
        )
        .await
        .unwrap();

    // Old secret works before rotation.
    assert!(api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .is_ok());

    let rotated = api_key_service
        .rotate_api_key(user.id, create_response.id)
        .await
        .unwrap();

    // Same key record: id, name, role, and expiry policy unchanged.
    assert_eq!(rotated.id, create_response.id);
    assert_eq!(rotated.name, create_response.name);
    assert_eq!(rotated.role_type, create_response.role_type);
    assert_eq!(rotated.expires_at, create_response.expires_at);

    // A brand new plaintext secret was minted.
    assert_ne!(rotated.api_key, create_response.api_key);
    assert!(rotated.api_key.starts_with("tk_"));

    // The old secret must no longer authenticate ...
    let old_result = api_key_service
        .validate_api_key(&create_response.api_key)
        .await;
    assert!(old_result.is_err());
    assert!(matches!(
        old_result.unwrap_err(),
        ApiKeyServiceError::Unauthorized(_)
    ));

    // ... while the new secret does.
    let (validated_user, role, _, _, key_id) = api_key_service
        .validate_api_key(&rotated.api_key)
        .await
        .unwrap();
    assert_eq!(validated_user.id, user.id);
    assert_eq!(role, Some(Role::Admin));
    assert_eq!(key_id, create_response.id);
}

#[tokio::test]
async fn test_rotate_api_key_not_found() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let result = api_key_service.rotate_api_key(user.id, 999999).await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ApiKeyServiceError::NotFound(_)
    ));
}

#[tokio::test]
async fn test_rotate_api_key_wrong_user_is_not_found() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let result = api_key_service
        .rotate_api_key(user.id + 1, api_key.id)
        .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ApiKeyServiceError::NotFound(_)
    ));
}

// API Key Validation Tests

#[tokio::test]
async fn test_validate_api_key_success() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Valid Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: Some(Utc::now() + Duration::days(30)),
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    let (validated_user, role, permissions, key_name, key_id) = api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .unwrap();

    assert_eq!(validated_user.id, user.id);
    assert!(role.is_some());
    assert_eq!(role.unwrap(), Role::Admin);
    assert!(permissions.is_none());
    assert_eq!(key_name, "Valid Key");
    assert_eq!(key_id, create_response.id);
}

#[tokio::test]
async fn test_validate_api_key_custom_permissions() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Custom Key".to_string(),
        role_type: "custom".to_string(),
        permissions: Some(vec!["projects:read".to_string()]),
        expires_at: None,
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    let (validated_user, role, permissions, _, _) = api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .unwrap();

    assert_eq!(validated_user.id, user.id);
    assert!(role.is_none());
    assert!(permissions.is_some());
    let perms = permissions.unwrap();
    assert_eq!(perms.len(), 1);
    assert_eq!(perms[0], Permission::ProjectsRead);
}

#[tokio::test]
async fn test_validate_api_key_invalid() {
    let (_db, api_key_service, _user) = setup_test_env().await;

    let result = api_key_service
        .validate_api_key("tk_invalidkey123456")
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
}

#[tokio::test]
async fn test_validate_api_key_expired() {
    let (_db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Expired Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: Some(Utc::now() + Duration::hours(1)),
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();
    // A past expiry is refused on create, so let the stored one lapse.
    let stored = ApiKeyEntity::find_by_id(create_response.id)
        .one(_db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let mut stored: ApiKeyActiveModel = stored.into();
    stored.expires_at = Set(Some(Utc::now() - Duration::days(1)));
    stored.update(_db.db.as_ref()).await.unwrap();

    let result = api_key_service
        .validate_api_key(&create_response.api_key)
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
}

#[tokio::test]
async fn test_validate_api_key_inactive() {
    let (_db, api_key_service, user) = setup_test_env().await;

    // Create and then deactivate key
    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Inactive Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: None,
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    api_key_service
        .deactivate_api_key(user.id, create_response.id)
        .await
        .unwrap();

    let result = api_key_service
        .validate_api_key(&create_response.api_key)
        .await;

    assert!(result.is_err());
    matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
}

#[tokio::test]
async fn test_validate_api_key_updates_last_used() {
    let (db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Track Usage Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: None,
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    // Validate the key
    api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .unwrap();

    // Check that last_used_at was updated
    let api_key = api_keys::Entity::find_by_id(create_response.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap();

    assert!(api_key.last_used_at.is_some());
}

#[tokio::test]
async fn test_validate_api_key_repeat_call_skips_last_used_update() {
    let (db, api_key_service, user) = setup_test_env().await;

    let request = CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: "Throttled Usage Key".to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: None,
    };

    let create_response = api_key_service
        .create_api_key(user.id, request)
        .await
        .unwrap();

    api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .unwrap();

    let after_first_call = api_keys::Entity::find_by_id(create_response.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .last_used_at
        .expect("first validation should write last_used_at");

    // Immediately validate again — within LAST_USED_UPDATE_INTERVAL, the
    // throttle should skip the write entirely, so last_used_at must be
    // byte-identical to what the first call wrote.
    api_key_service
        .validate_api_key(&create_response.api_key)
        .await
        .unwrap();

    let after_second_call = api_keys::Entity::find_by_id(create_response.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .last_used_at
        .expect("last_used_at should still be set");

    assert_eq!(
        after_first_call, after_second_call,
        "second validate_api_key call within the throttle window must not rewrite last_used_at"
    );
}

/// Rotation keeps the key id and resets `last_used_at`. When the old
/// secret was used moments before, the per-id throttle was already
/// primed, and before the first-use bypass the new secret's first request
/// was never recorded: the AI harness onboarding then waited forever.
#[tokio::test]
async fn first_use_of_a_rotated_secret_is_recorded_inside_the_throttle_window() {
    let (db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, admin_key_request("Rotated harness key"))
        .await
        .unwrap();
    service.validate_api_key(&created.api_key).await.unwrap();

    let rotated = service.rotate_api_key(user.id, created.id).await.unwrap();
    let reset = api_keys::Entity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reset.last_used_at, None, "rotation resets last_used_at");

    service.validate_api_key(&rotated.api_key).await.unwrap();
    let after = api_keys::Entity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert!(
        after.last_used_at.is_some(),
        "the new secret's first use must be written even though the throttle window is open"
    );
}

/// Validation read the old secret; the key was rotated before the usage
/// write ran. The write must not mark the replacement secret as used.
#[tokio::test]
async fn usage_write_racing_a_rotation_does_not_stamp_the_new_secret() {
    let (db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, admin_key_request("Interleaved rotation key"))
        .await
        .unwrap();
    let validated_row = api_keys::Entity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(validated_row.last_used_at, None);

    service.rotate_api_key(user.id, created.id).await.unwrap();
    service.record_last_used(&validated_row).await;

    let after = api_keys::Entity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(after.key_hash, validated_row.key_hash);
    assert_eq!(
        after.last_used_at, None,
        "a request that sent the rotated-out secret must not mark the new one used"
    );
}

/// A failed bookkeeping write must not fail authentication, and must not
/// leave the throttle primed: the next request retries the write.
#[tokio::test]
async fn failed_last_used_write_is_retried_on_the_next_request() {
    // No results queued: every statement, including the UPDATE, errors.
    let service = ApiKeyService::new(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
    ));
    let now = Utc::now();
    let key = api_keys::Model {
        id: 41,
        name: "write-fails".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_abcde".to_string(),
        user_id: 3,
        role_type: "admin".to_string(),
        permissions: None,
        is_active: true,
        expires_at: None,
        last_used_at: Some(now),
        created_at: now,
        updated_at: now,
        service_id: None,
        project_id: None,
        environment_id: None,
    };

    service.record_last_used(&key).await;

    assert!(
        service.last_used_throttle.should_update(key.id),
        "a failed write must clear the throttle entry so the next request retries"
    );
}

fn admin_key_request(name: &str) -> CreateApiKeyRequest {
    CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: name.to_string(),
        role_type: "admin".to_string(),
        permissions: None,
        expires_at: None,
    }
}

async fn set_created_at(db: &Arc<DbConnection>, key_id: i32, created_at: chrono::DateTime<Utc>) {
    let key = api_keys::Entity::find_by_id(key_id)
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let mut active: api_keys::ActiveModel = key.into();
    active.created_at = Set(created_at);
    active.update(db.as_ref()).await.unwrap();
}

#[tokio::test]
async fn ai_harness_status_is_missing_without_a_dedicated_key() {
    let (_db, service, user) = setup_test_env().await;
    let status = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(status.status, crate::ai_harness::AiHarnessState::Missing);
    assert_eq!(status.key_id, None);
}

#[tokio::test]
async fn ai_harness_status_waits_then_connects_when_the_dedicated_key_is_used() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(
            user.id,
            admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
        )
        .await
        .unwrap();

    let waiting = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(waiting.status, crate::ai_harness::AiHarnessState::Waiting);
    assert_eq!(waiting.key_id, Some(created.id));

    service.validate_api_key(&created.api_key).await.unwrap();

    let connected = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(
        connected.status,
        crate::ai_harness::AiHarnessState::Connected
    );
    let via = connected.connected_via.unwrap();
    assert_eq!(via.id, created.id);
    assert_eq!(
        via.kind,
        crate::ai_harness::AiHarnessCredentialKind::DedicatedKey
    );
}

/// The reported case: the copied connect command fell through to the
/// browser device login, which mints its own key. The harness works with
/// that key and never sends the dedicated one.
#[tokio::test]
async fn ai_harness_status_connects_through_a_later_device_login() {
    let (db, service, user) = setup_test_env().await;
    let harness = service
        .create_api_key(
            user.id,
            admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
        )
        .await
        .unwrap();
    set_created_at(&db.db, harness.id, Utc::now() - Duration::minutes(10)).await;
    let cli = service
        .create_api_key(user.id, admin_key_request("cli-device:laptop-1767225600"))
        .await
        .unwrap();

    // Minted but not used yet: still waiting.
    let before = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(before.status, crate::ai_harness::AiHarnessState::Waiting);

    service.validate_api_key(&cli.api_key).await.unwrap();

    let status = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(status.status, crate::ai_harness::AiHarnessState::Connected);
    let via = status.connected_via.unwrap();
    assert_eq!(via.id, cli.id);
    assert_eq!(
        via.kind,
        crate::ai_harness::AiHarnessCredentialKind::CliLogin
    );
    assert_eq!(status.key_id, Some(harness.id));
}

#[tokio::test]
async fn ai_harness_status_ignores_earlier_and_other_users_device_logins() {
    let (db, service, user) = setup_test_env().await;
    // A device login from before onboarding started, in active use.
    let earlier = service
        .create_api_key(user.id, admin_key_request("cli-device:desktop-1700000000"))
        .await
        .unwrap();
    set_created_at(&db.db, earlier.id, Utc::now() - Duration::days(2)).await;
    service.validate_api_key(&earlier.api_key).await.unwrap();

    service
        .create_api_key(
            user.id,
            admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
        )
        .await
        .unwrap();

    // Another user's fresh device login, also in use.
    let other = users::ActiveModel {
        email: Set(format!("other_{}@example.com", uuid::Uuid::new_v4())),
        name: Set("Other User".to_string()),
        email_verified: Set(true),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        mfa_enabled: Set(false),
        ..Default::default()
    }
    .insert(db.db.as_ref())
    .await
    .unwrap();
    let foreign = create_test_api_key(&db.db, other.id, "cli-device:other-1", "admin").await;
    let mut foreign: api_keys::ActiveModel = foreign.into();
    foreign.last_used_at = Set(Some(Utc::now()));
    foreign.update(db.db.as_ref()).await.unwrap();

    let status = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(status.status, crate::ai_harness::AiHarnessState::Waiting);
}

/// The console used to look for the dedicated key in the first 100 keys
/// (newest first); every `temps login` mints a key, so it fell off that
/// page. The status lookup does not paginate.
#[tokio::test]
async fn ai_harness_status_finds_the_dedicated_key_behind_a_full_page_of_newer_keys() {
    let (db, service, user) = setup_test_env().await;
    let harness = create_test_api_key(
        &db.db,
        user.id,
        crate::ai_harness::AI_HARNESS_KEY_NAME,
        "admin",
    )
    .await;
    set_created_at(&db.db, harness.id, Utc::now() - Duration::days(1)).await;
    let mut used: api_keys::ActiveModel = harness.clone().into();
    used.last_used_at = Set(Some(Utc::now()));
    used.update(db.db.as_ref()).await.unwrap();
    for index in 0..100 {
        create_test_api_key(&db.db, user.id, &format!("newer key {index}"), "admin").await;
    }

    let first_page = service.list_api_keys(user.id, 1, 100).await.unwrap();
    assert!(
        first_page.api_keys.iter().all(|key| key.id != harness.id),
        "precondition: the dedicated key is not on the first page of 100"
    );

    let status = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(status.status, crate::ai_harness::AiHarnessState::Connected);
    assert_eq!(status.key_id, Some(harness.id));
}

#[tokio::test]
async fn ai_harness_status_reports_a_deactivated_key_instead_of_offering_a_conflicting_create() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(
            user.id,
            admin_key_request(crate::ai_harness::AI_HARNESS_KEY_NAME),
        )
        .await
        .unwrap();
    service
        .deactivate_api_key(user.id, created.id)
        .await
        .unwrap();

    let status = service.ai_harness_status(user.id).await.unwrap();
    assert_eq!(status.status, crate::ai_harness::AiHarnessState::Missing);
    assert_eq!(status.key_id, Some(created.id));
    assert_eq!(
        status.setup_path,
        Some(format!("/settings/keys/{}", created.id))
    );
}

#[tokio::test]
async fn ai_harness_status_surfaces_database_errors() {
    let service = ApiKeyService::new(Arc::new(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("api_keys lookup failed".into())])
            .into_connection(),
    ));
    let error = service.ai_harness_status(5).await.unwrap_err();
    assert!(matches!(error, ApiKeyServiceError::DatabaseError(_)));
}

#[tokio::test]
async fn delete_returns_the_deleted_key_and_names_the_missing_id() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, admin_key_request("Delete and describe"))
        .await
        .unwrap();
    let deleted = service.delete_api_key(user.id, created.id).await.unwrap();
    assert_eq!(deleted.id, created.id);
    assert_eq!(deleted.name, "Delete and describe");

    let error = service
        .delete_api_key(user.id, created.id)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApiKeyServiceError::NotFound(message) if message.contains(&created.id.to_string())),
        "{error}"
    );
}

// Activation/Deactivation Tests

#[tokio::test]
async fn test_deactivate_api_key() {
    let (db, api_key_service, user) = setup_test_env().await;

    let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

    let response = api_key_service
        .deactivate_api_key(user.id, api_key.id)
        .await
        .unwrap();

    assert!(!response.is_active);
}

#[tokio::test]
async fn test_activate_api_key() {
    let (db, api_key_service, user) = setup_test_env().await;

    // Create inactive key
    let unique_id = uuid::Uuid::new_v4();
    let api_key = api_keys::ActiveModel {
        name: Set("Inactive Key".to_string()),
        key_hash: Set(format!("test_hash_{}", unique_id)),
        key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
        user_id: Set(user.id),
        role_type: Set("admin".to_string()),
        is_active: Set(false), // Start as inactive
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let api_key = api_key.insert(db.db.as_ref()).await.unwrap();

    let response = api_key_service
        .activate_api_key(user.id, api_key.id)
        .await
        .unwrap();

    assert!(response.is_active);
}

// Helper Method Tests

#[tokio::test]
async fn test_generate_api_key_format() {
    let (_db, api_key_service, _user) = setup_test_env().await;

    let key = api_key_service.generate_api_key();

    assert!(key.starts_with("tk_"));
    assert_eq!(key.len(), 43); // tk_ + 40 random chars

    // Check that it only contains valid characters
    let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for c in key[3..].chars() {
        assert!(valid_chars.contains(c));
    }
}

#[tokio::test]
async fn test_hash_api_key_consistency() {
    let (_db, api_key_service, _user) = setup_test_env().await;

    let api_key = "tk_testkey123456";

    let hash1 = api_key_service.hash_api_key(api_key);
    let hash2 = api_key_service.hash_api_key(api_key);

    // Same input should produce same hash
    assert_eq!(hash1, hash2);

    // Hash should be 64 chars (SHA256 hex)
    assert_eq!(hash1.len(), 64);
}

#[tokio::test]
async fn test_hash_api_key_different_inputs() {
    let (_db, api_key_service, _user) = setup_test_env().await;

    let hash1 = api_key_service.hash_api_key("tk_key1");
    let hash2 = api_key_service.hash_api_key("tk_key2");

    // Different inputs should produce different hashes
    assert_ne!(hash1, hash2);
}

// ── Service ingest key ───────────────────────────────────────────────────────

#[test]
fn test_si_token_has_correct_prefix() {
    // Verify the prefix used for service ingest tokens
    let token = "si_ABCDEFGHIJ1234567890abcdefghij12345678901234567890";
    assert!(token.starts_with("si_"));
    assert!(token.len() > 40);
}

#[test]
fn test_service_ingest_key_name_format() {
    let service_name = "my-rustfs";
    let key_name = format!("metrics-ingest-{}", service_name);
    assert_eq!(key_name, "metrics-ingest-my-rustfs");
}

#[test]
fn test_service_ingest_role_type_string() {
    // The role_type stored in DB for service keys
    use crate::permissions::Role;
    let role = Role::MetricsIngest;
    assert_eq!(role.to_string(), "metrics_ingest");
    assert!(Role::from_str("metrics_ingest").is_some());
}

#[test]
fn test_metrics_ingest_role_has_no_permissions() {
    use crate::permissions::Role;
    let perms = Role::MetricsIngest.permissions();
    assert!(
        perms.is_empty(),
        "MetricsIngest role should have no UI permissions"
    );
}

// Owner-role ceiling tests

#[test]
fn admin_role_holds_every_other_predefined_role_permission() {
    // The cap keeps a predefined role intact only while the owner's role
    // covers it. Admin-owned keys of any predefined role must therefore
    // survive the cap unchanged, or every admin's restricted keys would
    // silently degrade to custom grants.
    for role in Role::all() {
        for permission in role.permissions() {
            assert!(
                Role::Admin.has_permission(permission),
                "Admin must hold {permission:?} (granted by {role:?})"
            );
        }
    }
}

#[test]
fn cap_keeps_a_role_the_owner_still_covers() {
    let (role, permissions) = cap_api_key_grant(Some(Role::Admin), None, &Role::Admin);
    assert_eq!(role, Some(Role::Admin));
    assert!(permissions.is_none());

    let (role, permissions) = cap_api_key_grant(Some(Role::PlatformAdmin), None, &Role::Admin);
    assert_eq!(role, Some(Role::PlatformAdmin));
    assert!(permissions.is_none());
}

#[test]
fn cap_degrades_an_admin_key_owned_by_a_demoted_user() {
    let (role, permissions) = cap_api_key_grant(Some(Role::Admin), None, &Role::User);
    assert_eq!(role, None, "an admin key must lose its admin role");
    let permissions = permissions.expect("a capped key carries an explicit grant");
    for permission in &permissions {
        assert!(Role::User.has_permission(permission));
    }
    assert!(!permissions.contains(&Permission::UsersManage));
    assert!(!permissions.contains(&Permission::SettingsWrite));

    let ctx = crate::context::AuthContext::new_api_key(
        users::Model {
            id: 1,
            name: "demoted".into(),
            email: "demoted@example.com".into(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        role,
        Some(permissions),
        "demoted-key".into(),
        1,
    );
    assert!(!ctx.is_admin());
    assert!(!ctx.is_instance_admin());
    assert!(!ctx.has_permission(&Permission::UsersManage));
}

#[test]
fn cap_filters_custom_permissions_to_the_owner() {
    let (role, permissions) = cap_api_key_grant(
        None,
        Some(vec![Permission::ProjectsRead, Permission::UsersManage]),
        &Role::User,
    );
    assert_eq!(role, None);
    assert_eq!(permissions, Some(vec![Permission::ProjectsRead]));
}

#[test]
fn cap_leaves_service_ingest_keys_unchanged() {
    // metrics_ingest keys carry no permission set; any owner covers them.
    let (role, permissions) = cap_api_key_grant(Some(Role::MetricsIngest), None, &Role::User);
    assert_eq!(role, Some(Role::MetricsIngest));
    assert!(permissions.is_none());
}

async fn mint_key(service: &ApiKeyService, user_id: i32, name: &str, role: &str) -> String {
    service
        .create_api_key(
            user_id,
            CreateApiKeyRequest {
                project_id: None,
                environment_id: None,
                name: name.to_string(),
                role_type: role.to_string(),
                permissions: None,
                expires_at: Some(Utc::now() + Duration::days(30)),
            },
        )
        .await
        .unwrap()
        .api_key
}

#[tokio::test]
async fn admin_key_loses_admin_grant_when_owner_is_demoted() {
    let (db, api_key_service, user) = setup_test_env().await;
    let admin_key = mint_key(&api_key_service, user.id, "admin-key", "admin").await;
    let user_key = mint_key(&api_key_service, user.id, "user-key", "user").await;

    let (_, role, permissions, _, _) = api_key_service.validate_api_key(&admin_key).await.unwrap();
    assert_eq!(role, Some(Role::Admin));
    assert!(permissions.is_none());

    crate::user_service::UserService::new(
        db.db.clone(),
        crate::mfa_guard::test_support::encryption(),
    )
    .remove_role_from_user(user.id, temps_entities::types::RoleType::Admin)
    .await
    .unwrap();

    let (_, role, permissions, _, _) = api_key_service.validate_api_key(&admin_key).await.unwrap();
    assert_eq!(role, None, "the demoted owner's admin key keeps no role");
    let permissions = permissions.unwrap();
    assert!(!permissions.contains(&Permission::UsersManage));
    assert!(permissions
        .iter()
        .all(|permission| Role::User.has_permission(permission)));

    // A key that never exceeded the owner keeps working unchanged.
    let (_, role, permissions, _, _) = api_key_service.validate_api_key(&user_key).await.unwrap();
    assert_eq!(role, Some(Role::User));
    assert!(permissions.is_none());

    // Re-promotion restores the stored grant: nothing was rewritten.
    crate::user_service::UserService::new(
        db.db.clone(),
        crate::mfa_guard::test_support::encryption(),
    )
    .assign_role_by_type(user.id, temps_entities::types::RoleType::Admin)
    .await
    .unwrap();
    let (_, role, _, _, _) = api_key_service.validate_api_key(&admin_key).await.unwrap();
    assert_eq!(role, Some(Role::Admin));
}

// Error Conversion Tests

#[tokio::test]
async fn test_error_to_problem_conversion() {
    let db_error =
        ApiKeyServiceError::DatabaseError(sea_orm::DbErr::RecordNotFound("test".to_string()));
    let problem = db_error.to_problem();
    assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);

    let not_found = ApiKeyServiceError::NotFound("Key not found".to_string());
    let problem = not_found.to_problem();
    assert_eq!(problem.status_code, StatusCode::NOT_FOUND);

    let validation = ApiKeyServiceError::ValidationError("Invalid input".to_string());
    let problem = validation.to_problem();
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

    let unauthorized = ApiKeyServiceError::Unauthorized("Invalid key".to_string());
    let problem = unauthorized.to_problem();
    assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);

    let conflict = ApiKeyServiceError::Conflict("Name exists".to_string());
    let problem = conflict.to_problem();
    assert_eq!(problem.status_code, StatusCode::CONFLICT);
}

// ---- input validation ----------------------------------------------------

fn key_request(name: &str, role_type: &str) -> CreateApiKeyRequest {
    CreateApiKeyRequest {
        project_id: None,
        environment_id: None,
        name: name.to_string(),
        role_type: role_type.to_string(),
        permissions: None,
        expires_at: None,
    }
}

async fn key_count(db: &TestDatabase, user_id: i32) -> u64 {
    ApiKeyEntity::find()
        .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
        .count(db.db.as_ref())
        .await
        .unwrap()
}

#[tokio::test]
async fn create_refuses_an_expiry_in_the_past_for_every_key() {
    let (db, service, user) = setup_test_env().await;
    let past = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let error = service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                expires_at: Some(past),
                ..key_request("b", "reader")
            },
        )
        .await
        .expect_err("a key that is already expired must be refused");

    assert!(
        matches!(&error, ApiKeyServiceError::ExpiryInPast { expires_at, .. } if *expires_at == past),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("expires_at 2020-01-01T00:00:00Z is in the past"),
        "{error}"
    );
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
    assert_eq!(key_count(&db, user.id).await, 0);
}

#[tokio::test]
async fn create_without_an_expiry_expires_in_one_year() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, key_request("defaulted", "reader"))
        .await
        .unwrap();
    let expires_at = created.expires_at.expect("API keys always expire");
    let in_a_year = Utc::now() + chrono::Duration::days(365);
    assert!(
        (expires_at - in_a_year).num_minutes().abs() < 5,
        "{expires_at}"
    );
}

#[tokio::test]
async fn create_trims_the_name_and_refuses_a_blank_one() {
    let (db, service, user) = setup_test_env().await;

    for blank in ["", "   ", "\t\n"] {
        let error = service
            .create_api_key(user.id, key_request(blank, "reader"))
            .await
            .expect_err("a blank name must be refused");
        assert!(
            matches!(error, ApiKeyServiceError::InvalidName { .. }),
            "{blank:?}: {error:?}"
        );
        assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
    }
    let too_long = "k".repeat(API_KEY_NAME_MAX_CHARS + 1);
    let error = service
        .create_api_key(user.id, key_request(&too_long, "reader"))
        .await
        .expect_err("an over-long name must be refused");
    assert!(matches!(error, ApiKeyServiceError::InvalidName { .. }));
    assert!(!error.to_string().contains(&too_long));
    assert_eq!(key_count(&db, user.id).await, 0);

    let created = service
        .create_api_key(user.id, key_request("  padded  ", "reader"))
        .await
        .unwrap();
    assert_eq!(created.name, "padded");
    let duplicate = service
        .create_api_key(user.id, key_request("padded ", "reader"))
        .await
        .expect_err("the trimmed name is what must be unique");
    assert!(matches!(duplicate, ApiKeyServiceError::Conflict(_)));
}

#[tokio::test]
async fn create_refuses_permissions_with_a_predefined_role() {
    let (db, service, user) = setup_test_env().await;

    let error = service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                permissions: Some(vec!["projects:write".to_string()]),
                ..key_request("e", "reader")
            },
        )
        .await
        .expect_err("a permission list a predefined role would drop must be refused");
    match &error {
        ApiKeyServiceError::PermissionsNeedCustomRole { role_type } => {
            assert_eq!(role_type, "reader")
        }
        other => panic!("expected PermissionsNeedCustomRole, got {other:?}"),
    }
    assert!(error.to_string().contains("role_type custom"), "{error}");
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
    assert_eq!(key_count(&db, user.id).await, 0);

    // An empty list asks for nothing, so it is not refused.
    let created = service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                permissions: Some(vec![]),
                ..key_request("reader-key", "reader")
            },
        )
        .await
        .expect("an empty list with a predefined role is accepted");
    assert_eq!(created.permissions, None);
}

#[tokio::test]
async fn create_explains_an_environment_scope_without_a_project() {
    let (_db, service, user) = setup_test_env().await;

    let error = service
        .create_api_key(
            user.id,
            CreateApiKeyRequest {
                environment_id: Some(1),
                ..key_request("d", "reader")
            },
        )
        .await
        .expect_err("an environment needs its project");
    let message = error.to_string();
    assert!(
        message.contains(
            "environment_id 1 needs project_id: an API key scoped to an environment must also \
             name its project"
        ),
        "{message}"
    );
    assert!(
        !message.contains("Some(") && !message.contains("None"),
        "{message}"
    );
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
}

async fn custom_key(service: &ApiKeyService, user_id: i32, permissions: &[&str]) -> i32 {
    service
        .create_api_key(
            user_id,
            CreateApiKeyRequest {
                permissions: Some(permissions.iter().map(|p| p.to_string()).collect()),
                ..key_request("custom", "custom")
            },
        )
        .await
        .unwrap()
        .id
}

fn permissions_update(permissions: &[&str]) -> UpdateApiKeyRequest {
    UpdateApiKeyRequest {
        permissions: Some(permissions.iter().map(|p| p.to_string()).collect()),
        ..Default::default()
    }
}

#[tokio::test]
async fn update_refuses_a_permission_change_instead_of_ignoring_it() {
    let (_db, service, user) = setup_test_env().await;
    let id = custom_key(&service, user.id, &["projects:read"]).await;

    for requested in [
        &["deployments:read", "nonsense:perm"][..],
        &["projects:read", "deployments:read"],
        &[],
    ] {
        let error = service
            .update_api_key(user.id, id, permissions_update(requested))
            .await
            .expect_err("a permission change must be refused");
        assert!(
            matches!(error, ApiKeyServiceError::PermissionsImmutable { api_key_id } if api_key_id == id),
            "{requested:?}: {error:?}"
        );
        let problem = error.to_problem();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
        assert_eq!(
            problem.body.get("error_code"),
            Some(&serde_json::json!("API_KEY_PERMISSIONS_IMMUTABLE"))
        );
    }
    let stored = service.get_api_key(user.id, id).await.unwrap();
    assert_eq!(stored.permissions, Some(vec!["projects:read".to_string()]));

    // Sending the key's own permissions back, with other changes, is fine.
    let renamed = service
        .update_api_key(
            user.id,
            id,
            UpdateApiKeyRequest {
                name: Some("renamed".to_string()),
                ..permissions_update(&["projects:read"])
            },
        )
        .await
        .expect("unchanged permissions are not a change");
    assert_eq!(renamed.name, "renamed");
    assert_eq!(renamed.permissions, Some(vec!["projects:read".to_string()]));
}

#[tokio::test]
async fn update_refuses_permissions_for_a_predefined_role_key() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, key_request("admin-key", "admin"))
        .await
        .unwrap();

    let error = service
        .update_api_key(user.id, created.id, permissions_update(&["projects:read"]))
        .await
        .expect_err("a role key cannot gain a permission list");
    assert!(matches!(
        error,
        ApiKeyServiceError::PermissionsImmutable { .. }
    ));
    service
        .update_api_key(user.id, created.id, permissions_update(&[]))
        .await
        .expect("an empty list matches a role key's permissions");
}

#[tokio::test]
async fn update_keeps_or_extends_the_expiry_but_never_removes_it() {
    let (db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, key_request("expiring", "reader"))
        .await
        .unwrap();
    let stored_expiry = created.expires_at.expect("keys always expire");

    // `null` would make the key never expire: refused, nothing changes.
    let request: UpdateApiKeyRequest = serde_json::from_str(r#"{"expires_at":null}"#).unwrap();
    assert_eq!(request.expires_at, Some(None));
    let error = service
        .update_api_key(user.id, created.id, request)
        .await
        .expect_err("removing the expiry must be refused");
    assert!(
        matches!(error, ApiKeyServiceError::ExpiryRequired { api_key_id } if api_key_id == created.id),
        "{error:?}"
    );
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);

    // A past expiry is refused too.
    let error = service
        .update_api_key(
            user.id,
            created.id,
            UpdateApiKeyRequest {
                expires_at: Some(Some(Utc::now() - Duration::minutes(1))),
                ..Default::default()
            },
        )
        .await
        .expect_err("a past expiry must be refused");
    assert!(matches!(error, ApiKeyServiceError::ExpiryInPast { .. }));

    // Leaving it out keeps it.
    let request: UpdateApiKeyRequest = serde_json::from_str(r#"{"is_active":false}"#).unwrap();
    assert_eq!(request.expires_at, None);
    let updated = service
        .update_api_key(user.id, created.id, request)
        .await
        .unwrap();
    assert!(!updated.is_active);
    assert_eq!(
        updated.expires_at.map(|t| t.timestamp()),
        Some(stored_expiry.timestamp())
    );

    // A key stored without an expiry may be sent `null`: nothing changes.
    let mut model: ApiKeyActiveModel = ApiKeyEntity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .into();
    model.expires_at = Set(None);
    model.update(db.db.as_ref()).await.unwrap();
    let request: UpdateApiKeyRequest = serde_json::from_str(r#"{"expires_at":null}"#).unwrap();
    let unchanged = service
        .update_api_key(user.id, created.id, request)
        .await
        .expect("null for a key without an expiry changes nothing");
    assert_eq!(unchanged.expires_at, None);
}

#[tokio::test]
async fn update_trims_the_name_and_refuses_a_blank_one() {
    let (_db, service, user) = setup_test_env().await;
    let created = service
        .create_api_key(user.id, key_request("named", "reader"))
        .await
        .unwrap();

    let error = service
        .update_api_key(
            user.id,
            created.id,
            UpdateApiKeyRequest {
                name: Some("   ".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("a blank name must be refused");
    assert!(matches!(error, ApiKeyServiceError::InvalidName { .. }));
    let stored = service.get_api_key(user.id, created.id).await.unwrap();
    assert_eq!(stored.name, "named");

    let renamed = service
        .update_api_key(
            user.id,
            created.id,
            UpdateApiKeyRequest {
                name: Some("  trimmed ".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "trimmed");
}
