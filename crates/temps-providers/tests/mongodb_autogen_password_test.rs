use serde_json::json;
use std::collections::HashMap;
use temps_providers::externalsvc::{ExternalService, MongodbService, ServiceConfig, ServiceType};

#[tokio::test]
async fn test_mongodb_password_auto_generation() {
    // Create a test service config WITHOUT password
    let mut parameters = HashMap::new();
    parameters.insert("host".to_string(), json!("localhost"));
    parameters.insert("port".to_string(), json!("27017"));
    parameters.insert("database".to_string(), json!("admin"));
    parameters.insert("username".to_string(), json!("root"));
    // Note: NO password parameter provided

    let service_config = ServiceConfig {
        name: "test-mongodb".to_string(),
        service_type: ServiceType::Mongodb,
        version: Some("7.0".to_string()),
        parameters: serde_json::to_value(parameters).unwrap(),
    };

    // Create MongoDB service
    let docker =
        bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
    let service = MongodbService::new("test-autogen".to_string(), std::sync::Arc::new(docker));

    // Configure the service - this should auto-generate a password
    let result = service.init(service_config).await;

    assert!(result.is_ok(), "Service configuration should succeed");

    // Get connection info - this should include the auto-generated password
    let connection_info = service.get_connection_info();

    assert!(
        connection_info.is_ok(),
        "Should be able to get connection info"
    );

    let connection_string = connection_info.unwrap();
    println!("Connection string: {}", connection_string);

    // Verify connection string has format: mongodb://username:password@host:port
    assert!(connection_string.starts_with("mongodb://"));
    assert!(connection_string.contains("root:"));
    assert!(connection_string.contains("@localhost:27017"));

    // Extract password from connection string
    let password_part = connection_string
        .split("root:")
        .nth(1)
        .unwrap()
        .split('@')
        .next()
        .unwrap();

    println!("Auto-generated password: {}", password_part);

    // Verify password is not empty and has reasonable length (default is 16 chars)
    assert!(!password_part.is_empty(), "Password should not be empty");
    assert!(
        password_part.len() >= 16,
        "Password should be at least 16 characters"
    );

    // Verify password contains only alphanumeric characters
    assert!(
        password_part.chars().all(|c| c.is_alphanumeric()),
        "Password should be alphanumeric"
    );

    println!("✓ Password auto-generation test passed!");
}

#[tokio::test]
async fn test_mongodb_custom_password_preserved() {
    // Create a test service config WITH custom password
    let mut parameters = HashMap::new();
    parameters.insert("host".to_string(), json!("localhost"));
    parameters.insert("port".to_string(), json!("27017"));
    parameters.insert("database".to_string(), json!("admin"));
    parameters.insert("username".to_string(), json!("root"));
    parameters.insert("password".to_string(), json!("my-custom-password-123"));

    let service_config = ServiceConfig {
        name: "test-mongodb".to_string(),
        service_type: ServiceType::Mongodb,
        version: Some("7.0".to_string()),
        parameters: serde_json::to_value(parameters).unwrap(),
    };

    // Create MongoDB service
    let docker =
        bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
    let service = MongodbService::new("test-custom".to_string(), std::sync::Arc::new(docker));

    // Configure the service - this should use the custom password
    let result = service.init(service_config).await;

    assert!(result.is_ok(), "Service configuration should succeed");

    // Get connection info
    let connection_string = service.get_connection_info().unwrap();
    println!("Connection string: {}", connection_string);

    // Verify connection string uses the custom password
    assert!(connection_string.contains("root:my-custom-password-123@"));

    println!("✓ Custom password preservation test passed!");
}

#[test]
fn test_password_generation_uniqueness() {
    use temps_providers::externalsvc::mongodb::generate_password;

    // Generate multiple passwords
    let password1 = generate_password();
    let password2 = generate_password();
    let password3 = generate_password();

    println!("Generated passwords:");
    println!("  1: {}", password1);
    println!("  2: {}", password2);
    println!("  3: {}", password3);

    // Verify each password is unique
    assert_ne!(password1, password2, "Passwords should be unique");
    assert_ne!(password2, password3, "Passwords should be unique");
    assert_ne!(password1, password3, "Passwords should be unique");

    // Verify all passwords have correct length
    assert_eq!(password1.len(), 16);
    assert_eq!(password2.len(), 16);
    assert_eq!(password3.len(), 16);

    // Verify all passwords are alphanumeric
    assert!(password1.chars().all(|c| c.is_alphanumeric()));
    assert!(password2.chars().all(|c| c.is_alphanumeric()));
    assert!(password3.chars().all(|c| c.is_alphanumeric()));

    println!("✓ Password uniqueness test passed!");
}
