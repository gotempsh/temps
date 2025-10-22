use temps_core::config::{DatabaseConfig, PaginationParams};

#[test]
fn test_database_config_serialization() {
    let config = DatabaseConfig {
        url: "postgresql://localhost:5432/test".to_string(),
        max_connections: 10,
        min_connections: 1,
    };

    // Test serialization
    let serialized = serde_json::to_string(&config).unwrap();
    assert!(serialized.contains("postgresql://localhost:5432/test"));
    assert!(serialized.contains("10"));
    assert!(serialized.contains("1"));

    // Test deserialization
    let deserialized: DatabaseConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.url, config.url);
    assert_eq!(deserialized.max_connections, config.max_connections);
    assert_eq!(deserialized.min_connections, config.min_connections);
}

#[test]
fn test_pagination_params_default() {
    let params = PaginationParams::default();

    assert_eq!(params.page, Some(1));
    assert_eq!(params.page_size, Some(20));
    assert_eq!(params.sort_by, Some("created_at".to_string()));
    assert_eq!(params.sort_order, Some("desc".to_string()));
}

#[test]
fn test_pagination_params_normalize() {
    // Test default normalization
    let params = PaginationParams::default();
    let (page, page_size) = params.normalize();
    assert_eq!(page, 1);
    assert_eq!(page_size, 20);

    // Test custom values
    let params = PaginationParams {
        page: Some(5),
        page_size: Some(50),
        sort_by: None,
        sort_order: None,
    };
    let (page, page_size) = params.normalize();
    assert_eq!(page, 5);
    assert_eq!(page_size, 50);

    // Test boundary conditions
    let params = PaginationParams {
        page: Some(0),        // Should be normalized to 1
        page_size: Some(150), // Should be capped at 100
        sort_by: None,
        sort_order: None,
    };
    let (page, page_size) = params.normalize();
    assert_eq!(page, 1);
    assert_eq!(page_size, 100);

    // Test None values (should use defaults)
    let params = PaginationParams {
        page: None,
        page_size: None,
        sort_by: None,
        sort_order: None,
    };
    let (page, page_size) = params.normalize();
    assert_eq!(page, 1);
    assert_eq!(page_size, 20);

    // Test minimum page_size
    let params = PaginationParams {
        page: Some(1),
        page_size: Some(0), // Should be normalized to 1
        sort_by: None,
        sort_order: None,
    };
    let (page, page_size) = params.normalize();
    assert_eq!(page, 1);
    assert_eq!(page_size, 1);
}

#[test]
fn test_pagination_params_serialization() {
    let params = PaginationParams {
        page: Some(2),
        page_size: Some(10),
        sort_by: Some("name".to_string()),
        sort_order: Some("asc".to_string()),
    };

    // Test serialization
    let serialized = serde_json::to_string(&params).unwrap();

    // Test deserialization
    let deserialized: PaginationParams = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.page, params.page);
    assert_eq!(deserialized.page_size, params.page_size);
    assert_eq!(deserialized.sort_by, params.sort_by);
    assert_eq!(deserialized.sort_order, params.sort_order);
}
