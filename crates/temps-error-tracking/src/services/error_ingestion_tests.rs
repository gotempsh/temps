use super::*;
use std::sync::Arc;
use temps_database::test_utils::TestDatabase;
use temps_entities::{error_events, error_groups, projects};

async fn setup_test_db() -> TestDatabase {
    TestDatabase::with_migrations()
        .await
        .expect("Failed to create test database")
}

async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
    use temps_entities::preset::Preset;
    use uuid::Uuid;

    let unique_slug = format!("test-project-{}", Uuid::new_v4());
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("/test".to_string()),
        main_branch: Set("main".to_string()),
        slug: Set(unique_slug),
        preset: Set(Preset::NextJs),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    project
        .insert(db.as_ref())
        .await
        .expect("Failed to create project")
        .id
}

fn create_test_error_data(project_id: i32) -> CreateErrorEventData {
    CreateErrorEventData {
        source: Some("test".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Cannot read property 'foo' of undefined".to_string()),
        stack_trace: Some(serde_json::json!([
            {
                "filename": "/app/index.js",
                "function": "doSomething",
                "lineno": 42
            }
        ])),
        project_id,
        ..Default::default()
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_creates_new_group() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;
    let error_data = create_test_error_data(project_id);

    let group_id = service
        .process_error_event(error_data)
        .await
        .expect("Failed to process error event");

    // Verify group was created
    let group = error_groups::Entity::find_by_id(group_id)
        .one(db.as_ref())
        .await
        .expect("Failed to fetch group")
        .expect("Group not found");

    assert_eq!(group.project_id, project_id);
    assert_eq!(group.total_count, 1);
    assert_eq!(group.status, "unresolved");
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_groups_similar_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;

    // Process first error
    let error_data1 = create_test_error_data(project_id);
    let group_id1 = service
        .process_error_event(error_data1.clone())
        .await
        .expect("Failed to process first error");

    // Process second identical error
    let error_data2 = error_data1.clone();
    let group_id2 = service
        .process_error_event(error_data2)
        .await
        .expect("Failed to process second error");

    // Should be grouped together
    assert_eq!(group_id1, group_id2);

    // Verify count was incremented
    let group = error_groups::Entity::find_by_id(group_id1)
        .one(db.as_ref())
        .await
        .expect("Failed to fetch group")
        .expect("Group not found");

    assert_eq!(group.total_count, 2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_generate_fingerprint_is_consistent() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let error_data = CreateErrorEventData {
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Test error".to_string()),
        stack_trace: Some(serde_json::json!([{"filename": "test.js", "function": "test"}])),
        project_id: 1,
        ..Default::default()
    };

    let fingerprint1 = service.generate_fingerprint(&error_data);
    let fingerprint2 = service.generate_fingerprint(&error_data);

    assert_eq!(fingerprint1, fingerprint2);
    assert!(!fingerprint1.is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn test_generate_fingerprint_differs_for_different_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let error_data1 = CreateErrorEventData {
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Error 1".to_string()),
        stack_trace: None,
        project_id: 1,
        ..Default::default()
    };

    let error_data2 = CreateErrorEventData {
        exception_type: Some("ReferenceError".to_string()),
        exception_value: Some("Error 2".to_string()),
        stack_trace: None,
        project_id: 1,
        ..Default::default()
    };

    let fingerprint1 = service.generate_fingerprint(&error_data1);
    let fingerprint2 = service.generate_fingerprint(&error_data2);

    assert_ne!(fingerprint1, fingerprint2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message1 = "Error: Connection failed at line 123";
    let message2 = "ERROR: CONNECTION FAILED AT LINE 123";

    let normalized1 = service.normalize_error_message(message1);
    let normalized2 = service.normalize_error_message(message2);

    assert_eq!(normalized1, normalized2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_creates_event() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;
    let error_data = create_test_error_data(project_id);

    let group_id = service
        .process_error_event(error_data)
        .await
        .expect("Failed to process error event");

    // Verify event was created
    let events = error_events::Entity::find()
        .filter(error_events::Column::ErrorGroupId.eq(group_id))
        .all(db.as_ref())
        .await
        .expect("Failed to fetch events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exception_type, "TypeError");
}

#[tokio::test]
#[serial_test::serial]
async fn test_extract_stack_signature() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let stack_trace = Some(serde_json::json!([
        {"filename": "/app/src/index.js", "function": "main"},
        {"filename": "/app/src/utils.js", "function": "helper"},
        {"filename": "/app/src/lib.js", "function": "doWork"},
    ]));

    let signature = service.extract_stack_signature(&stack_trace, 3);

    assert!(signature.contains("index.js"));
    assert!(signature.contains("main"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_uuids() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Resource 550e8400-e29b-41d4-a716-446655440000 not found";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<uuid>"));
    assert!(!normalized.contains("550e8400"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_hex_ids() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Transaction 0xdeadbeef1234 failed";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<hex_id>"));
    assert!(!normalized.contains("deadbeef"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_numbers() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: User 123456 failed to authenticate";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<num>"));
    assert!(!normalized.contains("123456"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_paths() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message1 = "Error: Cannot read /home/user/app/config.json";
    let normalized1 = service.normalize_error_message(message1);
    assert!(normalized1.contains("<path>"));

    let message2 = "Error: File C:\\Users\\Admin\\file.txt not found";
    let normalized2 = service.normalize_error_message(message2);
    assert!(normalized2.contains("<path>"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_urls() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Failed to fetch https://api.example.com/users/123";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<url>"));
    assert!(!normalized.contains("example.com"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_emails() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Email user@example.com already exists";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<email>"));
    assert!(!normalized.contains("user@example"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_ips() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Connection to 192.168.1.100 timeout";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<ip>"));
    assert!(!normalized.contains("192.168"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_table_refs() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Foreign key constraint failed on table users_123";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("users_<id>"));
    assert!(!normalized.contains("users_123"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_groups_similar_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    // These errors should normalize to the same message
    let message1 = "Error: User 12345 not found at 192.168.1.100";
    let message2 = "Error: User 67890 not found at 10.0.0.5";
    let message3 = "Error: User 99999 not found at 172.16.0.1";

    let normalized1 = service.normalize_error_message(message1);
    let normalized2 = service.normalize_error_message(message2);
    let normalized3 = service.normalize_error_message(message3);

    // All should normalize to the same pattern
    assert_eq!(normalized1, normalized2);
    assert_eq!(normalized2, normalized3);
    assert!(normalized1.contains("<num>"));
    assert!(normalized1.contains("<ip>"));
}
