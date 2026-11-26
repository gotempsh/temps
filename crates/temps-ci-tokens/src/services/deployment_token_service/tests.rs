// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use chrono::Duration;
use sea_orm::{ActiveModelTrait, Set};
use temps_entities::{projects, users};
use temps_test_utils::TestDatabase;

/// Test encryption key for creating DeploymentTokenService in tests
const TEST_ENCRYPTION_KEY: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Set up a test environment with Docker-based PostgreSQL.
/// Returns None if Docker is not available (test should skip gracefully).
async fn setup_test_env() -> Option<(TestDatabase, DeploymentTokenService, projects::Model)> {
    let db = match TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            println!("Docker not available, skipping test: {}", e);
            return None;
        }
    };

    // Create a test user (needed for FK constraint on deployment_tokens.created_by)
    let user = users::ActiveModel {
        name: Set("Test User".to_string()),
        email: Set(format!("test-{}@example.com", uuid::Uuid::new_v4())),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    user.insert(db.db.as_ref()).await.unwrap();

    // Create a test project
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        slug: Set(format!("test-project-{}", uuid::Uuid::new_v4())),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("test-repo".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    let project = project.insert(db.db.as_ref()).await.unwrap();

    let encryption_service = Arc::new(EncryptionService::new(TEST_ENCRYPTION_KEY).unwrap());

    let service = DeploymentTokenService::new(db.db.clone(), encryption_service);
    Some((db, service, project))
}

#[tokio::test]
async fn test_create_deployment_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Test Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: Some(vec![
            "visitors:enrich".to_string(),
            "emails:send".to_string(),
        ]),
        expires_at: None,
    };

    let response = service
        .create_token(project.id, Some(1), request)
        .await
        .unwrap();

    assert_eq!(response.name, "Test Token");
    assert_eq!(response.project_id, project.id);
    assert!(response.token.starts_with("dt_"));
    assert_eq!(response.token.len(), 43); // dt_ + 40 chars
    assert!(response.permissions.is_some());
}

#[tokio::test]
async fn test_create_deployment_token_default_permissions() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Default Perms Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None, // Should default to full access
        expires_at: None,
    };

    let response = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    let perms = response.permissions.unwrap();
    assert!(perms.contains(&"*".to_string()));
}

#[tokio::test]
async fn test_create_deployment_token_invalid_permission() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Invalid Perm Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: Some(vec!["invalid:permission".to_string()]),
        expires_at: None,
    };

    let result = service.create_token(project.id, None, request).await;

    assert!(result.is_err());
    matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::ValidationError(_)
    );
}

#[tokio::test]
async fn test_create_deployment_token_duplicate_name() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request1 = CreateDeploymentTokenRequest {
        name: "Duplicate Name".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    service
        .create_token(project.id, None, request1)
        .await
        .unwrap();

    let request2 = CreateDeploymentTokenRequest {
        name: "Duplicate Name".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    let result = service.create_token(project.id, None, request2).await;

    assert!(result.is_err());
    matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::Conflict(_)
    );
}

#[tokio::test]
async fn test_list_deployment_tokens() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    // Create multiple tokens
    for i in 1..=3 {
        let request = CreateDeploymentTokenRequest {
            name: format!("Token {}", i),
            environment_id: None,
            deployment_id: None,
            permissions: None,
            expires_at: None,
        };
        service
            .create_token(project.id, None, request)
            .await
            .unwrap();
    }

    let response = service.list_tokens(project.id, 1, 10).await.unwrap();

    assert_eq!(response.total, 3);
    assert_eq!(response.tokens.len(), 3);
}

#[tokio::test]
async fn test_get_deployment_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Get Token Test".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    let retrieved = service.get_token(project.id, created.id).await.unwrap();

    assert_eq!(retrieved.id, created.id);
    assert_eq!(retrieved.name, "Get Token Test");
}

#[tokio::test]
async fn test_update_deployment_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Original Name".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    let update_request = UpdateDeploymentTokenRequest {
        name: Some("Updated Name".to_string()),
        is_active: Some(false),
        permissions: None,
        expires_at: None,
    };

    let updated = service
        .update_token(project.id, created.id, update_request)
        .await
        .unwrap();

    assert_eq!(updated.name, "Updated Name");
    assert!(!updated.is_active);
}

#[tokio::test]
async fn test_delete_deployment_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Delete Me".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    service.delete_token(project.id, created.id).await.unwrap();

    let result = service.get_token(project.id, created.id).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_rotate_deployment_token_invalidates_old_secret_and_issues_new_one() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Rotate Me".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: Some(vec!["visitors:enrich".to_string()]),
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    // Old token works before rotation.
    assert!(service.validate_token(&created.token).await.is_ok());

    let rotated = service.rotate_token(project.id, created.id).await.unwrap();

    // Same row: id, name, project scope, and permissions unchanged.
    assert_eq!(rotated.id, created.id);
    assert_eq!(rotated.name, created.name);
    assert_eq!(rotated.project_id, created.project_id);
    assert_eq!(rotated.permissions, created.permissions);

    // A brand new plaintext token was minted.
    assert_ne!(rotated.token, created.token);
    assert!(rotated.token.starts_with("dt_"));

    // The old token must no longer validate ...
    let old_result = service.validate_token(&created.token).await;
    assert!(old_result.is_err());
    assert!(matches!(
        old_result.unwrap_err(),
        DeploymentTokenServiceError::Unauthorized(_)
    ));

    // ... while the new token does.
    let (validated_project_id, _, permissions) =
        service.validate_token(&rotated.token).await.unwrap();
    assert_eq!(validated_project_id, project.id);
    assert_eq!(permissions, vec![DeploymentTokenPermission::VisitorsEnrich]);
}

#[tokio::test]
async fn test_rotate_deployment_token_not_found() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let result = service.rotate_token(project.id, 999999).await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::NotFound(_)
    ));
}

#[tokio::test]
async fn test_rotate_deployment_token_wrong_project_is_not_found() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Scoped Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    // Rotating with the wrong project_id must not find the token.
    let result = service.rotate_token(project.id + 1, created.id).await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::NotFound(_)
    ));
}

#[tokio::test]
async fn test_validate_deployment_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Validate Me".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: Some(vec!["visitors:enrich".to_string()]),
        expires_at: None,
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    let (validated_project_id, validated_env_id, permissions) =
        service.validate_token(&created.token).await.unwrap();

    assert_eq!(validated_project_id, project.id);
    assert!(validated_env_id.is_none());
    assert_eq!(permissions.len(), 1);
    assert_eq!(permissions[0], DeploymentTokenPermission::VisitorsEnrich);
}

#[tokio::test]
async fn test_validate_expired_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    let request = CreateDeploymentTokenRequest {
        name: "Expired Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: Some(Utc::now() + Duration::hours(1)),
    };

    let created = service
        .create_token(project.id, None, request)
        .await
        .unwrap();

    // A past expiry is refused on create, so let the stored one lapse.
    let stored = DeploymentTokenEntity::find_by_id(created.id)
        .one(_db.db.as_ref())
        .await
        .unwrap()
        .unwrap();
    let mut stored: DeploymentTokenActiveModel = stored.into();
    stored.expires_at = Set(Some(Utc::now() - Duration::days(1)));
    stored.update(_db.db.as_ref()).await.unwrap();

    let result = service.validate_token(&created.token).await;

    assert!(result.is_err());
    matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::Unauthorized(_)
    );
}

#[tokio::test]
async fn test_validate_invalid_token() {
    let Some((_db, service, _project)) = setup_test_env().await else {
        return;
    };

    let result = service.validate_token("dt_invalidtoken123456").await;

    assert!(result.is_err());
    matches!(
        result.unwrap_err(),
        DeploymentTokenServiceError::Unauthorized(_)
    );
}

#[tokio::test]
async fn test_token_generation_format() {
    // This test doesn't need Docker — generate_token is a pure function.
    // Use a mock DB connection to construct the service.
    let mock_db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let encryption_service = Arc::new(EncryptionService::new(TEST_ENCRYPTION_KEY).unwrap());
    let service = DeploymentTokenService::new(mock_db, encryption_service);

    let token = service.generate_token();

    assert!(token.starts_with("dt_"));
    assert_eq!(token.len(), 43); // dt_ + 40 random chars

    // Check that it only contains valid characters
    let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for c in token[3..].chars() {
        assert!(valid_chars.contains(c));
    }
}

#[tokio::test]
async fn test_token_hash_consistency() {
    // This test doesn't need Docker — hash_token is a pure function.
    let mock_db =
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
    let encryption_service = Arc::new(EncryptionService::new(TEST_ENCRYPTION_KEY).unwrap());
    let service = DeploymentTokenService::new(mock_db, encryption_service);

    let token = "dt_testtoken123456";

    let hash1 = service.hash_token(token);
    let hash2 = service.hash_token(token);

    // Same input should produce same hash
    assert_eq!(hash1, hash2);

    // Hash should be 64 chars (SHA256 hex)
    assert_eq!(hash1.len(), 64);
}

// ============================================
// Unit tests that don't require Docker
// ============================================

/// Test DeploymentTokenPermission parsing from string
#[test]
fn test_permission_from_str() {
    assert_eq!(
        DeploymentTokenPermission::from_str("visitors:enrich"),
        Some(DeploymentTokenPermission::VisitorsEnrich)
    );
    assert_eq!(
        DeploymentTokenPermission::from_str("emails:send"),
        Some(DeploymentTokenPermission::EmailsSend)
    );
    assert_eq!(
        DeploymentTokenPermission::from_str("analytics:read"),
        Some(DeploymentTokenPermission::AnalyticsRead)
    );
    assert_eq!(
        DeploymentTokenPermission::from_str("events:write"),
        Some(DeploymentTokenPermission::EventsWrite)
    );
    assert_eq!(
        DeploymentTokenPermission::from_str("errors:read"),
        Some(DeploymentTokenPermission::ErrorsRead)
    );
    assert_eq!(
        DeploymentTokenPermission::from_str("*"),
        Some(DeploymentTokenPermission::FullAccess)
    );
    assert_eq!(DeploymentTokenPermission::from_str("invalid:perm"), None);
    assert_eq!(DeploymentTokenPermission::from_str(""), None);
}

/// Test DeploymentTokenPermission as_str method
#[test]
fn test_permission_as_str() {
    assert_eq!(
        DeploymentTokenPermission::VisitorsEnrich.as_str(),
        "visitors:enrich"
    );
    assert_eq!(
        DeploymentTokenPermission::EmailsSend.as_str(),
        "emails:send"
    );
    assert_eq!(
        DeploymentTokenPermission::AnalyticsRead.as_str(),
        "analytics:read"
    );
    assert_eq!(
        DeploymentTokenPermission::EventsWrite.as_str(),
        "events:write"
    );
    assert_eq!(
        DeploymentTokenPermission::ErrorsRead.as_str(),
        "errors:read"
    );
    assert_eq!(DeploymentTokenPermission::FullAccess.as_str(), "*");
}

/// Test permission roundtrip (as_str -> from_str)
#[test]
fn test_permission_roundtrip() {
    let permissions = vec![
        DeploymentTokenPermission::VisitorsEnrich,
        DeploymentTokenPermission::EmailsSend,
        DeploymentTokenPermission::AnalyticsRead,
        DeploymentTokenPermission::EventsWrite,
        DeploymentTokenPermission::ErrorsRead,
        DeploymentTokenPermission::AiGatewayExecute,
        DeploymentTokenPermission::FlagsRead,
        DeploymentTokenPermission::BlobRead,
        DeploymentTokenPermission::BlobWrite,
        DeploymentTokenPermission::BlobDelete,
        DeploymentTokenPermission::KvRead,
        DeploymentTokenPermission::KvWrite,
        DeploymentTokenPermission::KvDelete,
        DeploymentTokenPermission::FullAccess,
    ];

    for perm in permissions {
        let as_string = perm.as_str();
        let parsed = DeploymentTokenPermission::from_str(as_string);
        assert_eq!(
            parsed,
            Some(perm.clone()),
            "Roundtrip failed for {:?}",
            perm
        );
    }
}

/// Test DeploymentTokenPermission::all() returns all variants
///
/// The length assertion is deliberate: adding a variant without listing it
/// here means it silently misses `all()`, and `all()` is what the UI offers
/// when scoping a token. Update both when adding a permission.
#[test]
fn test_permission_all() {
    let all = DeploymentTokenPermission::all();
    assert_eq!(all.len(), 14);
    assert!(all.contains(&DeploymentTokenPermission::VisitorsEnrich));
    assert!(all.contains(&DeploymentTokenPermission::EmailsSend));
    assert!(all.contains(&DeploymentTokenPermission::AnalyticsRead));
    assert!(all.contains(&DeploymentTokenPermission::EventsWrite));
    assert!(all.contains(&DeploymentTokenPermission::ErrorsRead));
    assert!(all.contains(&DeploymentTokenPermission::AiGatewayExecute));
    assert!(all.contains(&DeploymentTokenPermission::FlagsRead));
    assert!(all.contains(&DeploymentTokenPermission::BlobRead));
    assert!(all.contains(&DeploymentTokenPermission::BlobWrite));
    assert!(all.contains(&DeploymentTokenPermission::BlobDelete));
    assert!(all.contains(&DeploymentTokenPermission::KvRead));
    assert!(all.contains(&DeploymentTokenPermission::KvWrite));
    assert!(all.contains(&DeploymentTokenPermission::KvDelete));
    assert!(all.contains(&DeploymentTokenPermission::FullAccess));
}

/// Test DeploymentTokenPermission::grants() method
#[test]
fn test_permission_grants() {
    // FullAccess grants everything
    assert!(
        DeploymentTokenPermission::FullAccess.grants(&DeploymentTokenPermission::VisitorsEnrich)
    );
    assert!(DeploymentTokenPermission::FullAccess.grants(&DeploymentTokenPermission::EmailsSend));
    assert!(DeploymentTokenPermission::FullAccess.grants(&DeploymentTokenPermission::FullAccess));

    // Specific permission only grants itself
    assert!(DeploymentTokenPermission::VisitorsEnrich
        .grants(&DeploymentTokenPermission::VisitorsEnrich));
    assert!(
        !DeploymentTokenPermission::VisitorsEnrich.grants(&DeploymentTokenPermission::EmailsSend)
    );
    assert!(
        !DeploymentTokenPermission::EmailsSend.grants(&DeploymentTokenPermission::AnalyticsRead)
    );
}

/// Test token generation format without database
#[test]
fn test_token_generation_format_unit() {
    // We can't call generate_token without a service instance,
    // but we can test the format requirements
    let valid_token = "dt_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ1234";

    // Should start with dt_
    assert!(valid_token.starts_with("dt_"));

    // Should be 43 characters total (dt_ + 40 random)
    assert_eq!(valid_token.len(), 43);

    // Characters after prefix should be alphanumeric
    let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for c in valid_token[3..].chars() {
        assert!(
            valid_chars.contains(c),
            "Invalid character '{}' in token",
            c
        );
    }
}

/// Test token hash format
#[test]
fn test_token_hash_format_unit() {
    use sha2::{Digest, Sha256};

    let token = "dt_testtoken123456789012345678901234567890";

    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = hex::encode(hasher.finalize());

    // SHA256 hash should be 64 hex characters
    assert_eq!(hash.len(), 64);

    // Should only contain hex characters
    for c in hash.chars() {
        assert!(c.is_ascii_hexdigit(), "Invalid hex character: {}", c);
    }
}

/// Test hash consistency
#[test]
fn test_hash_consistency_unit() {
    use sha2::{Digest, Sha256};

    let token = "dt_testtoken123456789012345678901234567890";

    let hash1 = {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    };

    let hash2 = {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    };

    // Same input should produce same hash
    assert_eq!(hash1, hash2);
}

/// Test different tokens produce different hashes
#[test]
fn test_different_tokens_different_hashes() {
    use sha2::{Digest, Sha256};

    let token1 = "dt_testtoken1111111111111111111111111111111";
    let token2 = "dt_testtoken2222222222222222222222222222222";

    let hash1 = {
        let mut hasher = Sha256::new();
        hasher.update(token1.as_bytes());
        hex::encode(hasher.finalize())
    };

    let hash2 = {
        let mut hasher = Sha256::new();
        hasher.update(token2.as_bytes());
        hex::encode(hasher.finalize())
    };

    // Different tokens should produce different hashes
    assert_ne!(hash1, hash2);
}

/// Test CreateDeploymentTokenRequest default values
#[test]
fn test_create_request_serialization() {
    let request = CreateDeploymentTokenRequest {
        name: "Test Token".to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: Some(vec!["visitors:enrich".to_string()]),
        expires_at: None,
    };

    // Verify fields
    assert_eq!(request.name, "Test Token");
    assert!(request.environment_id.is_none());
    assert!(request.permissions.is_some());
    assert_eq!(request.permissions.as_ref().unwrap().len(), 1);
    assert!(request.expires_at.is_none());
}

/// Test UpdateDeploymentTokenRequest with partial updates
#[test]
fn test_update_request_partial() {
    // Only update name
    let update1 = UpdateDeploymentTokenRequest {
        name: Some("New Name".to_string()),
        is_active: None,
        permissions: None,
        expires_at: None,
    };
    assert!(update1.name.is_some());
    assert!(update1.is_active.is_none());

    // Only update is_active
    let update2 = UpdateDeploymentTokenRequest {
        name: None,
        is_active: Some(false),
        permissions: None,
        expires_at: None,
    };
    assert!(update2.name.is_none());
    assert_eq!(update2.is_active, Some(false));

    // Update multiple fields
    let update3 = UpdateDeploymentTokenRequest {
        name: Some("Updated".to_string()),
        is_active: Some(true),
        permissions: Some(vec!["*".to_string()]),
        expires_at: None,
    };
    assert!(update3.name.is_some());
    assert!(update3.is_active.is_some());
    assert!(update3.permissions.is_some());
}

/// Test DeploymentTokenServiceError variants
#[test]
fn test_error_variants() {
    let not_found = DeploymentTokenServiceError::NotFound("Token not found".to_string());
    assert!(matches!(
        not_found,
        DeploymentTokenServiceError::NotFound(_)
    ));

    let validation = DeploymentTokenServiceError::ValidationError("Invalid".to_string());
    assert!(matches!(
        validation,
        DeploymentTokenServiceError::ValidationError(_)
    ));

    let conflict = DeploymentTokenServiceError::Conflict("Already exists".to_string());
    assert!(matches!(conflict, DeploymentTokenServiceError::Conflict(_)));

    let unauthorized = DeploymentTokenServiceError::Unauthorized("Bad token".to_string());
    assert!(matches!(
        unauthorized,
        DeploymentTokenServiceError::Unauthorized(_)
    ));
}

/// Test error to Problem conversion produces valid Problem instances
#[test]
fn test_error_to_problem_creation() {
    // Test that to_problem() doesn't panic and returns a Problem
    let not_found = DeploymentTokenServiceError::NotFound("Token 123 not found".to_string());
    let _problem = not_found.to_problem();

    let validation = DeploymentTokenServiceError::ValidationError("Invalid permission".to_string());
    let _problem = validation.to_problem();

    let conflict = DeploymentTokenServiceError::Conflict("Token already exists".to_string());
    let _problem = conflict.to_problem();

    let unauthorized = DeploymentTokenServiceError::Unauthorized("Invalid token".to_string());
    let _problem = unauthorized.to_problem();

    let internal_error =
        DeploymentTokenServiceError::InternalServerError("Connection failed".to_string());
    let _problem = internal_error.to_problem();
}

/// Test all permission variants exist
#[test]
fn test_all_permissions_covered() {
    // Ensure all permission strings are valid
    let all_perm_strings = vec![
        "visitors:enrich",
        "emails:send",
        "analytics:read",
        "events:write",
        "errors:read",
        "ai_gateway:execute",
        "flags:read",
        "blob:read",
        "blob:write",
        "blob:delete",
        "kv:read",
        "kv:write",
        "kv:delete",
        "*",
    ];

    for perm_str in all_perm_strings {
        let parsed = DeploymentTokenPermission::from_str(perm_str);
        assert!(
            parsed.is_some(),
            "Permission '{}' should be parseable",
            perm_str
        );
    }
}

/// Test token prefix extraction
#[test]
fn test_token_prefix_extraction() {
    let token = "dt_abcdefghijklmnopqrstuvwxyz1234567890abcd";

    // Token prefix is the first 8 characters of the token (as stored in DB)
    // The service stores: token.chars().take(8).collect::<String>()
    let prefix: String = token.chars().take(8).collect();
    assert_eq!(prefix.len(), 8); // "dt_abcde"
    assert!(prefix.starts_with("dt_"));
    assert_eq!(prefix, "dt_abcde");
}

/// Test permission JSON serialization
#[test]
fn test_permission_serialization() {
    let perms = vec![
        DeploymentTokenPermission::VisitorsEnrich,
        DeploymentTokenPermission::EmailsSend,
    ];

    // Test that it can be serialized to JSON
    let json = serde_json::to_string(&perms).unwrap();
    assert!(json.contains("visitors_enrich") || json.contains("VisitorsEnrich"));

    // Test that it can be deserialized
    let parsed: Vec<DeploymentTokenPermission> = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.len(), 2);
}

/// Test CreateDeploymentTokenResponse structure
#[test]
fn test_create_response_structure() {
    let response = CreateDeploymentTokenResponse {
        id: 1,
        project_id: 100,
        environment_id: Some(50),
        deployment_id: None,
        name: "Test Token".to_string(),
        token: "dt_abcdefghij1234567890abcdefghij1234567890".to_string(),
        token_prefix: "dt_abcdef...".to_string(),
        permissions: Some(vec!["*".to_string()]),
        expires_at: None,
        created_at: Utc::now(),
    };

    assert_eq!(response.id, 1);
    assert_eq!(response.project_id, 100);
    assert_eq!(response.environment_id, Some(50));
    assert!(response.token.starts_with("dt_"));
    assert!(response.token_prefix.ends_with("..."));
}

/// Test DeploymentTokenResponse structure (without plain token)
#[test]
fn test_token_response_structure() {
    let response = DeploymentTokenResponse {
        id: 1,
        project_id: 100,
        environment_id: None,
        deployment_id: None,
        name: "API Token".to_string(),
        token_prefix: "dt_xyz123...".to_string(),
        permissions: Some(vec![
            "visitors:enrich".to_string(),
            "emails:send".to_string(),
        ]),
        is_active: true,
        expires_at: None,
        last_used_at: None,
        created_at: Utc::now(),
        created_by: Some(1),
    };

    assert_eq!(response.id, 1);
    assert_eq!(response.name, "API Token");
    // Note: DeploymentTokenResponse doesn't have a `token` field - only token_prefix
    assert!(response.token_prefix.ends_with("..."));
    assert_eq!(response.permissions.as_ref().unwrap().len(), 2);
}

/// Regression test for the production bug where the deploy pipeline got
/// handed a short-lived `workflow-run-*` token instead of a permanent
/// one. The container baked the expiring token into TEMPS_API_TOKEN at
/// create time, then 401'd every API call once the token's 2h window
/// closed.
///
/// `get_or_create_deployment_token` MUST skip any row with a non-NULL
/// `expires_at`, even if that row hasn't expired yet — those rows
/// belong to other subsystems (workflow runs, workspace sessions) and
/// are not intended for the deploy pipeline.
#[tokio::test]
async fn test_get_or_create_skips_short_lived_tokens() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    // Seed an active, non-expired, short-lived token that mimics a
    // workflow-run token (2h expiry, future).
    let short_lived = service
        .create_token(
            project.id,
            Some(1),
            CreateDeploymentTokenRequest {
                name: "workflow-run-fake-1".to_string(),
                environment_id: None,
                deployment_id: None,
                permissions: Some(vec!["*".to_string()]),
                expires_at: Some(Utc::now() + Duration::hours(2)),
            },
        )
        .await
        .unwrap();

    // Now ask the deploy pipeline's helper for a token.
    let returned = service
        .get_or_create_deployment_token(project.id, None, None)
        .await
        .unwrap();

    // The short-lived row's plaintext is in `short_lived.token`. The
    // returned token must NOT equal it — the function should have
    // fallen through to the create branch and minted a fresh
    // permanent row.
    assert_ne!(
        returned, short_lived.token,
        "deploy pipeline reused a short-lived (expiring) token — would 401 after expiry"
    );

    // And the newly-minted permanent row must exist with expires_at = NULL.
    let permanent = DeploymentTokenEntity::find()
        .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project.id))
        .filter(temps_entities::deployment_tokens::Column::ExpiresAt.is_null())
        .one(service.db.as_ref())
        .await
        .unwrap()
        .expect("get_or_create should have created a permanent token");
    assert!(permanent.name.starts_with("Auto-generated"));
}

/// Happy-path test: when a permanent token already exists for the
/// project, `get_or_create_deployment_token` reuses it instead of
/// minting a new one. Without this, every redeploy of every app would
/// orphan the previous deployment's token.
#[tokio::test]
async fn test_get_or_create_reuses_permanent_token() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    // First call: no token exists, so this creates a permanent one.
    let first = service
        .get_or_create_deployment_token(project.id, None, None)
        .await
        .unwrap();

    // Second call: must return the SAME plaintext.
    let second = service
        .get_or_create_deployment_token(project.id, None, None)
        .await
        .unwrap();

    assert_eq!(
        first, second,
        "get_or_create should reuse the existing permanent token"
    );
}

// ── Security: run token revocation ────────────────────────────────────────

/// After `revoke_token_by_id` is called, `validate_token` must reject
/// the same token — verifying that immediate revocation works end-to-end.
///
/// This is the integration test required for security fix #2:
/// run-scoped tokens are now revoked at run completion so the remaining
/// expiry window cannot be abused.
#[tokio::test]
async fn test_revoke_token_by_id_makes_token_invalid() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    // Mint a run-scoped token exactly as the executor does.
    let created = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                name: "workflow-run-test-agent-1".to_string(),
                environment_id: None,
                deployment_id: None,
                // FullAccess ("*") as issued by the executor.
                permissions: Some(vec!["*".to_string()]),
                expires_at: Some(Utc::now() + Duration::seconds(720)),
            },
        )
        .await
        .unwrap();

    // Token must be valid immediately after creation.
    assert!(
        service.validate_token(&created.token).await.is_ok(),
        "token must validate before revocation"
    );

    // The executor-issued token must NOT use plain wildcard as its sole
    // protection — verify the permissions field is ["*"] (FullAccess) as
    // expected by the current implementation.
    let perms = created.permissions.as_deref().unwrap_or(&[]);
    assert_eq!(
        perms,
        &["*".to_string()],
        "run token must carry FullAccess permission"
    );

    // Revoke the token by its database ID.
    service.revoke_token_by_id(created.id).await.unwrap();

    // After revocation, validate_token must return Unauthorized.
    let result = service.validate_token(&created.token).await;
    assert!(result.is_err(), "token must be rejected after revocation");
    assert!(
        matches!(
            result.unwrap_err(),
            DeploymentTokenServiceError::Unauthorized(_)
        ),
        "rejection must be Unauthorized, not a database error"
    );
}

/// `revoke_token_by_id` on a non-existent token must silently succeed —
/// the executor cleanup path must not fail a completed run because the
/// token was already cleaned up.
#[tokio::test]
async fn test_revoke_token_by_id_nonexistent_is_noop() {
    let Some((_db, service, _project)) = setup_test_env().await else {
        return;
    };

    let result = service.revoke_token_by_id(99999).await;
    assert!(
        result.is_ok(),
        "revoking a non-existent token must not return an error"
    );
}

// ---- scope and input validation -----------------------------------------

/// A second project with one environment and one deployment in it.
async fn other_project_scope(
    db: &TestDatabase,
) -> (
    projects::Model,
    temps_entities::environments::Model,
    temps_entities::deployments::Model,
) {
    let project = projects::ActiveModel {
        name: Set("Other Project".to_string()),
        slug: Set(format!("other-project-{}", uuid::Uuid::new_v4())),
        repo_owner: Set("test-owner".to_string()),
        repo_name: Set("other-repo".to_string()),
        main_branch: Set("main".to_string()),
        preset: Set(temps_entities::preset::Preset::NextJs),
        directory: Set("/".to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db.db.as_ref())
    .await
    .unwrap();
    let (environment, deployment) = environment_with_deployment(db, project.id, "other").await;
    (project, environment, deployment)
}

async fn environment_with_deployment(
    db: &TestDatabase,
    project_id: i32,
    name: &str,
) -> (
    temps_entities::environments::Model,
    temps_entities::deployments::Model,
) {
    let environment = temps_entities::environments::ActiveModel {
        name: Set(name.to_string()),
        slug: Set(format!("{name}-{}", uuid::Uuid::new_v4())),
        subdomain: Set(format!("{name}-{}", uuid::Uuid::new_v4())),
        host: Set(String::new()),
        upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
        project_id: Set(project_id),
        ..Default::default()
    }
    .insert(db.db.as_ref())
    .await
    .unwrap();
    let deployment = temps_entities::deployments::ActiveModel {
        project_id: Set(project_id),
        environment_id: Set(environment.id),
        slug: Set(format!("{name}-deployment-{}", uuid::Uuid::new_v4())),
        state: Set("completed".to_string()),
        metadata: Set(Some(
            temps_entities::deployments::DeploymentMetadata::default(),
        )),
        ..Default::default()
    }
    .insert(db.db.as_ref())
    .await
    .unwrap();
    (environment, deployment)
}

fn named(name: &str) -> CreateDeploymentTokenRequest {
    CreateDeploymentTokenRequest {
        name: name.to_string(),
        environment_id: None,
        deployment_id: None,
        permissions: None,
        expires_at: None,
    }
}

async fn token_count(db: &TestDatabase) -> u64 {
    DeploymentTokenEntity::find()
        .count(db.db.as_ref())
        .await
        .unwrap()
}

#[tokio::test]
async fn create_accepts_an_environment_and_deployment_of_its_own_project() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let (environment, deployment) = environment_with_deployment(&db, project.id, "prod").await;

    let created = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                environment_id: Some(environment.id),
                deployment_id: Some(deployment.id),
                ..named("scoped")
            },
        )
        .await
        .expect("an environment and deployment of the project are accepted");

    assert_eq!(created.project_id, project.id);
    assert_eq!(created.environment_id, Some(environment.id));
    assert_eq!(created.deployment_id, Some(deployment.id));
}

#[tokio::test]
async fn create_refuses_an_environment_of_another_project() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let (_, other_environment, _) = other_project_scope(&db).await;

    let error = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                environment_id: Some(other_environment.id),
                ..named("cross-project")
            },
        )
        .await
        .expect_err("another project's environment must be refused");

    match &error {
        DeploymentTokenServiceError::EnvironmentNotInProject {
            project_id,
            environment_id,
        } => {
            assert_eq!(*project_id, project.id);
            assert_eq!(*environment_id, other_environment.id);
        }
        other => panic!("expected EnvironmentNotInProject, got {other:?}"),
    }
    assert_eq!(error.to_problem().status_code, StatusCode::NOT_FOUND);
    assert_eq!(token_count(&db).await, 0, "nothing is stored");
}

#[tokio::test]
async fn create_refuses_a_deleted_or_unknown_environment() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let (environment, _) = environment_with_deployment(&db, project.id, "gone").await;
    let mut deleted: temps_entities::environments::ActiveModel = environment.clone().into();
    deleted.deleted_at = Set(Some(Utc::now()));
    deleted.update(db.db.as_ref()).await.unwrap();

    for environment_id in [environment.id, i32::MAX] {
        let error = service
            .create_token(
                project.id,
                None,
                CreateDeploymentTokenRequest {
                    environment_id: Some(environment_id),
                    ..named(&format!("env-{environment_id}"))
                },
            )
            .await
            .expect_err("a deleted or unknown environment must be refused");
        assert!(
            matches!(
                error,
                DeploymentTokenServiceError::EnvironmentNotInProject { .. }
            ),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn create_refuses_a_deployment_of_another_project() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let (_, _, other_deployment) = other_project_scope(&db).await;

    let error = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                deployment_id: Some(other_deployment.id),
                ..named("cross-project-deployment")
            },
        )
        .await
        .expect_err("another project's deployment must be refused");

    match &error {
        DeploymentTokenServiceError::DeploymentNotInProject {
            project_id,
            deployment_id,
        } => {
            assert_eq!(*project_id, project.id);
            assert_eq!(*deployment_id, other_deployment.id);
        }
        other => panic!("expected DeploymentNotInProject, got {other:?}"),
    }
    assert_eq!(error.to_problem().status_code, StatusCode::NOT_FOUND);
    assert_eq!(token_count(&db).await, 0);
}

#[tokio::test]
async fn create_refuses_a_deployment_of_a_different_environment() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let (production, _) = environment_with_deployment(&db, project.id, "production").await;
    let (staging, staging_deployment) =
        environment_with_deployment(&db, project.id, "staging").await;

    let error = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                environment_id: Some(production.id),
                deployment_id: Some(staging_deployment.id),
                ..named("mismatched")
            },
        )
        .await
        .expect_err("a deployment of another environment must be refused");

    match &error {
        DeploymentTokenServiceError::DeploymentNotInEnvironment {
            deployment_id,
            environment_id,
            deployment_environment_id,
        } => {
            assert_eq!(*deployment_id, staging_deployment.id);
            assert_eq!(*environment_id, production.id);
            assert_eq!(*deployment_environment_id, staging.id);
        }
        other => panic!("expected DeploymentNotInEnvironment, got {other:?}"),
    }
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_refuses_a_missing_project_with_not_found() {
    let Some((db, service, _project)) = setup_test_env().await else {
        return;
    };

    let error = service
        .create_token(i32::MAX, None, named("orphan"))
        .await
        .expect_err("a missing project must be refused");

    assert!(
        matches!(
            error,
            DeploymentTokenServiceError::ProjectNotFound { project_id } if project_id == i32::MAX
        ),
        "{error:?}"
    );
    assert_eq!(error.to_problem().status_code, StatusCode::NOT_FOUND);
    assert_eq!(token_count(&db).await, 0);
}

#[tokio::test]
async fn create_refuses_an_expiry_in_the_past() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let past = Utc::now() - Duration::days(1);

    let error = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                expires_at: Some(past),
                ..named("already-expired")
            },
        )
        .await
        .expect_err("a token that can never work must be refused");

    assert!(
        matches!(error, DeploymentTokenServiceError::ExpiryInPast { expires_at } if expires_at == past),
        "{error:?}"
    );
    let problem = error.to_problem();
    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    assert_eq!(token_count(&db).await, 0);
}

#[tokio::test]
async fn create_trims_the_name_and_refuses_a_blank_one() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };

    for blank in ["", "   ", "\t\n"] {
        let error = service
            .create_token(project.id, None, named(blank))
            .await
            .expect_err("a blank name must be refused");
        assert!(
            matches!(error, DeploymentTokenServiceError::InvalidName { .. }),
            "{blank:?}: {error:?}"
        );
        assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
    }

    let too_long = "x".repeat(TOKEN_NAME_MAX_CHARS + 1);
    let error = service
        .create_token(project.id, None, named(&too_long))
        .await
        .expect_err("an over-long name must be refused");
    assert!(matches!(
        error,
        DeploymentTokenServiceError::InvalidName { .. }
    ));
    assert!(
        !error.to_string().contains(&too_long),
        "the refusal does not echo the name"
    );

    let created = service
        .create_token(project.id, None, named("  padded  "))
        .await
        .unwrap();
    assert_eq!(created.name, "padded");
    let duplicate = service
        .create_token(project.id, None, named("padded "))
        .await
        .expect_err("the trimmed name is what must be unique");
    assert!(matches!(
        duplicate,
        DeploymentTokenServiceError::Conflict(_)
    ));
}

#[tokio::test]
async fn create_refuses_an_empty_permission_list() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };

    let error = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                permissions: Some(vec![]),
                ..named("powerless")
            },
        )
        .await
        .expect_err("a token that can do nothing must be refused");

    assert!(
        matches!(error, DeploymentTokenServiceError::EmptyPermissions { project_id } if project_id == project.id),
        "{error:?}"
    );
    assert_eq!(error.to_problem().status_code, StatusCode::BAD_REQUEST);
    assert_eq!(token_count(&db).await, 0);
}

#[tokio::test]
async fn update_null_expiry_removes_it_and_an_absent_one_keeps_it() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };
    let expiry = Utc::now() + Duration::days(7);
    let created = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                expires_at: Some(expiry),
                ..named("expiring")
            },
        )
        .await
        .unwrap();

    // Leaving expires_at out keeps the stored expiry.
    let request: UpdateDeploymentTokenRequest =
        serde_json::from_str(r#"{"name":"renamed"}"#).unwrap();
    assert_eq!(request.expires_at, None);
    let renamed = service
        .update_token(project.id, created.id, request)
        .await
        .unwrap();
    assert_eq!(renamed.name, "renamed");
    assert_eq!(
        renamed.expires_at.map(|t| t.timestamp()),
        Some(expiry.timestamp())
    );

    // An explicit null removes it.
    let request: UpdateDeploymentTokenRequest =
        serde_json::from_str(r#"{"expires_at":null}"#).unwrap();
    assert_eq!(request.expires_at, Some(None));
    let cleared = service
        .update_token(project.id, created.id, request)
        .await
        .unwrap();
    assert_eq!(cleared.expires_at, None);
    assert_eq!(cleared.name, "renamed", "other fields are kept");
    let stored = service.get_token(project.id, created.id).await.unwrap();
    assert_eq!(stored.expires_at, None);
}

#[tokio::test]
async fn update_refuses_a_past_expiry_blank_name_and_empty_permissions() {
    let Some((_db, service, project)) = setup_test_env().await else {
        return;
    };
    let created = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                permissions: Some(vec!["analytics:read".to_string()]),
                ..named("kept")
            },
        )
        .await
        .unwrap();

    let refusals = [
        UpdateDeploymentTokenRequest {
            expires_at: Some(Some(Utc::now() - Duration::minutes(1))),
            ..Default::default()
        },
        UpdateDeploymentTokenRequest {
            name: Some("  ".to_string()),
            ..Default::default()
        },
        UpdateDeploymentTokenRequest {
            permissions: Some(vec![]),
            ..Default::default()
        },
    ];
    for request in refusals {
        let error = service
            .update_token(project.id, created.id, request)
            .await
            .expect_err("the update must be refused");
        assert_eq!(
            error.to_problem().status_code,
            StatusCode::BAD_REQUEST,
            "{error:?}"
        );
    }

    let stored = service.get_token(project.id, created.id).await.unwrap();
    assert_eq!(stored.name, "kept");
    assert_eq!(stored.expires_at, None);
    assert_eq!(stored.permissions, Some(vec!["analytics:read".to_string()]));
}

#[tokio::test]
async fn update_accepts_the_stored_expiry_of_an_expired_token_unchanged() {
    let Some((db, service, project)) = setup_test_env().await else {
        return;
    };
    let created = service
        .create_token(
            project.id,
            None,
            CreateDeploymentTokenRequest {
                expires_at: Some(Utc::now() + Duration::hours(1)),
                ..named("lapsed")
            },
        )
        .await
        .unwrap();
    // A past expiry is refused on create, so let the stored one lapse.
    let lapsed = Utc::now() - Duration::days(3) - Duration::microseconds(123_456);
    let mut stored: DeploymentTokenActiveModel = DeploymentTokenEntity::find_by_id(created.id)
        .one(db.db.as_ref())
        .await
        .unwrap()
        .unwrap()
        .into();
    stored.expires_at = Set(Some(lapsed));
    stored.update(db.db.as_ref()).await.unwrap();

    // A client sends the token back as it read it, with a new name.
    let read =
        serde_json::to_value(service.get_token(project.id, created.id).await.unwrap()).unwrap();
    let request: UpdateDeploymentTokenRequest = serde_json::from_value(serde_json::json!({
        "name": "lapsed-renamed",
        "expires_at": read["expires_at"],
    }))
    .unwrap();
    let updated = service
        .update_token(project.id, created.id, request)
        .await
        .expect("an unchanged expiry is not a new one, even after it has passed");
    assert_eq!(updated.name, "lapsed-renamed");
    let kept: Option<chrono::DateTime<Utc>> =
        serde_json::from_value(read["expires_at"].clone()).unwrap();
    assert_eq!(updated.expires_at, kept);

    // A different past expiry is still refused.
    let error = service
        .update_token(
            project.id,
            created.id,
            UpdateDeploymentTokenRequest {
                expires_at: Some(Some(lapsed - Duration::days(1))),
                ..Default::default()
            },
        )
        .await
        .expect_err("a new expiry in the past must be refused");
    assert!(
        matches!(error, DeploymentTokenServiceError::ExpiryInPast { .. }),
        "{error:?}"
    );
}

#[test]
fn update_request_tells_null_expiry_from_an_absent_one() {
    let absent: UpdateDeploymentTokenRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(absent.expires_at, None);
    let null: UpdateDeploymentTokenRequest =
        serde_json::from_str(r#"{"expires_at":null}"#).unwrap();
    assert_eq!(null.expires_at, Some(None));
    let set: UpdateDeploymentTokenRequest =
        serde_json::from_str(r#"{"expires_at":"2030-01-01T00:00:00Z"}"#).unwrap();
    assert!(matches!(set.expires_at, Some(Some(_))));
}
