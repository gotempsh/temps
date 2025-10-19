use chrono::Utc;
use temps_auth::{AuthContext, AuthSource, Permission, Role};
use temps_entities::users;

// Helper function to create a mock user
fn create_mock_user() -> users::Model {
    users::Model {
        id: 1,
        name: "Test User".to_string(),
        email: "test@example.com".to_string(),
        password_hash: Some("hashed_password".to_string()),
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn test_auth_context_new_session() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user.clone(), Role::User);

    assert_eq!(context.user.id, user.id);
    assert_eq!(context.user.email, user.email);
    assert_eq!(context.effective_role, Role::User);
    assert!(context.custom_permissions.is_none());
    assert!(context.is_session());
    assert!(!context.is_cli_token());
    assert!(!context.is_api_key());

    if let AuthSource::Session { user: source_user } = &context.source {
        assert_eq!(source_user.id, user.id);
    } else {
        panic!("Expected Session auth source");
    }
}

#[test]
fn test_auth_context_new_cli_token() {
    let user = create_mock_user();
    let context = AuthContext::new_cli_token(user.clone(), Role::Admin);

    assert_eq!(context.user.id, user.id);
    assert_eq!(context.effective_role, Role::Admin);
    assert!(context.custom_permissions.is_none());
    assert!(!context.is_session());
    assert!(context.is_cli_token());
    assert!(!context.is_api_key());

    if let AuthSource::CliToken { user: source_user } = &context.source {
        assert_eq!(source_user.id, user.id);
    } else {
        panic!("Expected CliToken auth source");
    }
}

#[test]
fn test_auth_context_new_api_key_with_role() {
    let user = create_mock_user();
    let context = AuthContext::new_api_key(
        user.clone(),
        Some(Role::Reader),
        None,
        "test-key".to_string(),
        42,
    );

    assert_eq!(context.user.id, user.id);
    assert_eq!(context.effective_role, Role::Reader);
    assert!(context.custom_permissions.is_none());
    assert!(!context.is_session());
    assert!(!context.is_cli_token());
    assert!(context.is_api_key());

    if let AuthSource::ApiKey {
        user: source_user,
        role,
        permissions,
        key_name,
        key_id,
    } = &context.source
    {
        assert_eq!(source_user.id, user.id);
        assert_eq!(role, &Some(Role::Reader));
        assert!(permissions.is_none());
        assert_eq!(key_name, "test-key");
        assert_eq!(*key_id, 42);
    } else {
        panic!("Expected ApiKey auth source");
    }
}

#[test]
fn test_auth_context_new_api_key_with_custom_permissions() {
    let user = create_mock_user();
    let custom_permissions = vec![Permission::ProjectsRead, Permission::AnalyticsRead];

    let context = AuthContext::new_api_key(
        user.clone(),
        None,
        Some(custom_permissions.clone()),
        "custom-key".to_string(),
        123,
    );

    assert_eq!(context.user.id, user.id);
    assert_eq!(context.effective_role, Role::Custom);
    assert_eq!(context.custom_permissions, Some(custom_permissions));
    assert!(context.is_api_key());

    if let AuthSource::ApiKey {
        role,
        permissions,
        key_name,
        key_id,
        ..
    } = &context.source
    {
        assert!(role.is_none());
        assert_eq!(
            permissions,
            &Some(vec![Permission::ProjectsRead, Permission::AnalyticsRead])
        );
        assert_eq!(key_name, "custom-key");
        assert_eq!(*key_id, 123);
    } else {
        panic!("Expected ApiKey auth source");
    }
}

#[test]
fn test_has_permission_with_role() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user, Role::Admin);

    // Admin should have system admin permission
    assert!(context.has_permission(&Permission::SystemAdmin));
    assert!(context.has_permission(&Permission::ProjectsRead));
    assert!(context.has_permission(&Permission::ProjectsWrite));
}

#[test]
fn test_has_permission_with_custom_permissions() {
    let user = create_mock_user();
    let custom_permissions = vec![Permission::ProjectsRead, Permission::AnalyticsRead];

    let context = AuthContext::new_api_key(
        user,
        None,
        Some(custom_permissions),
        "custom-key".to_string(),
        1,
    );

    // Should have custom permissions
    assert!(context.has_permission(&Permission::ProjectsRead));
    assert!(context.has_permission(&Permission::AnalyticsRead));

    // Should not have permissions not in the custom list
    assert!(!context.has_permission(&Permission::ProjectsWrite));
    assert!(!context.has_permission(&Permission::SystemAdmin));
}

#[test]
fn test_has_permission_user_role() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user, Role::User);

    // User should have these permissions
    assert!(context.has_permission(&Permission::ProjectsRead));
    assert!(context.has_permission(&Permission::ProjectsWrite));

    // User should not have admin permissions
    assert!(!context.has_permission(&Permission::SystemAdmin));
}

#[test]
fn test_has_permission_reader_role() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user, Role::Reader);

    // Reader should have read permissions
    assert!(context.has_permission(&Permission::ProjectsRead));
    assert!(context.has_permission(&Permission::AnalyticsRead));

    // Reader should not have write permissions
    assert!(!context.has_permission(&Permission::ProjectsWrite));
    assert!(!context.has_permission(&Permission::SystemAdmin));
}

#[test]
fn test_has_role() {
    let user = create_mock_user();

    let admin_context = AuthContext::new_session(user.clone(), Role::Admin);
    assert!(admin_context.has_role(&Role::Admin));
    assert!(!admin_context.has_role(&Role::User));

    let user_context = AuthContext::new_session(user, Role::User);
    assert!(user_context.has_role(&Role::User));
    assert!(!user_context.has_role(&Role::Admin));
}

#[test]
fn test_is_admin() {
    let user = create_mock_user();

    let admin_context = AuthContext::new_session(user.clone(), Role::Admin);
    assert!(admin_context.is_admin());

    let user_context = AuthContext::new_session(user, Role::User);
    assert!(!user_context.is_admin());
}

#[test]
fn test_user_id() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user.clone(), Role::User);
    assert_eq!(context.user_id(), user.id);
}

#[test]
fn test_api_key_info() {
    let user = create_mock_user();

    // Session should not have API key info
    let session_context = AuthContext::new_session(user.clone(), Role::User);
    assert!(session_context.api_key_info().is_none());

    // API key should have key info
    let api_context =
        AuthContext::new_api_key(user, Some(Role::Reader), None, "test-key".to_string(), 42);

    let key_info = api_context.api_key_info();
    assert!(key_info.is_some());
    let (key_name, key_id) = key_info.unwrap();
    assert_eq!(key_name, "test-key");
    assert_eq!(key_id, 42);
}

#[test]
fn test_auth_source_variants() {
    let user = create_mock_user();

    // Test Session
    let session_context = AuthContext::new_session(user.clone(), Role::User);
    assert!(session_context.is_session());
    assert!(!session_context.is_cli_token());
    assert!(!session_context.is_api_key());

    // Test CLI Token
    let cli_context = AuthContext::new_cli_token(user.clone(), Role::User);
    assert!(!cli_context.is_session());
    assert!(cli_context.is_cli_token());
    assert!(!cli_context.is_api_key());

    // Test API Key
    let api_context =
        AuthContext::new_api_key(user, Some(Role::Reader), None, "key".to_string(), 1);
    assert!(!api_context.is_session());
    assert!(!api_context.is_cli_token());
    assert!(api_context.is_api_key());
}

#[test]
fn test_auth_context_serialization() {
    let user = create_mock_user();
    let context = AuthContext::new_session(user, Role::Admin);

    let serialized = serde_json::to_string(&context).unwrap();
    let deserialized: AuthContext = serde_json::from_str(&serialized).unwrap();

    assert_eq!(context.user.id, deserialized.user.id);
    assert_eq!(context.user.email, deserialized.user.email);
    assert_eq!(context.effective_role, deserialized.effective_role);
    assert_eq!(context.custom_permissions, deserialized.custom_permissions);
}
