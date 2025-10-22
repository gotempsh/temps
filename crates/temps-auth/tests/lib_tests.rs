// Integration tests that directly test the working modules
// without depending on the broken parts of the crate

use temps_auth::{Permission, Role}; // These should work from the lib.rs exports

#[test]
fn test_basic_permission_functionality() {
    // Test that we can use permissions from the crate
    let perm = Permission::ProjectsRead;
    assert_eq!(perm.to_string(), "projects:read");

    let parsed = Permission::from_str("projects:read");
    assert_eq!(parsed, Some(Permission::ProjectsRead));
}

#[test]
fn test_basic_role_functionality() {
    // Test that we can use roles from the crate
    let role = Role::Admin;
    assert_eq!(role.to_string(), "admin");

    let parsed = Role::from_str("admin");
    assert_eq!(parsed, Some(Role::Admin));

    // Test permissions
    assert!(role.has_permission(&Permission::SystemAdmin));
    assert!(role.has_permission(&Permission::ProjectsRead));
}

#[test]
fn test_role_permission_hierarchy() {
    // Admin should have more permissions than User
    let admin_perms = Role::Admin.permissions();
    let user_perms = Role::User.permissions();
    let reader_perms = Role::Reader.permissions();

    assert!(admin_perms.len() > user_perms.len());
    assert!(user_perms.len() > reader_perms.len());

    // All should have at least read permissions
    assert!(admin_perms.contains(&Permission::ProjectsRead));
    assert!(user_perms.contains(&Permission::ProjectsRead));
    assert!(reader_perms.contains(&Permission::ProjectsRead));

    // But only admin should have system admin
    assert!(admin_perms.contains(&Permission::SystemAdmin));
    assert!(!user_perms.contains(&Permission::SystemAdmin));
    assert!(!reader_perms.contains(&Permission::SystemAdmin));
}
