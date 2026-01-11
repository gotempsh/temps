//! Integration tests for structured logging
//!
//! Demonstrates how existing code can use structured logging with minimal changes

use tempfile::TempDir;
use temps_logs::{LogLevel, LogService};

#[tokio::test]
async fn test_structured_logging_api() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    // All logs must use structured logging with explicit level
    service
        .append_structured_log("legacy-job", LogLevel::Info, "Starting deployment")
        .await
        .unwrap();

    service
        .append_structured_log("legacy-job", LogLevel::Success, "Deployment complete")
        .await
        .unwrap();

    // Read as structured logs (JSONL format)
    let logs = service.get_structured_logs("legacy-job").await.unwrap();
    assert_eq!(logs.len(), 2);
    assert!(logs[0].message.contains("Starting deployment"));
    assert_eq!(logs[0].level, LogLevel::Info);
    assert!(logs[1].message.contains("Deployment complete"));
    assert_eq!(logs[1].level, LogLevel::Success);
}

#[tokio::test]
async fn test_new_structured_logging_api() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    // New code can use structured logging methods
    service
        .log_info("deploy-456", "Deploying container image...")
        .await
        .unwrap();

    service
        .log_success("deploy-456", "Container is running")
        .await
        .unwrap();

    service
        .log_warning("deploy-456", "Waiting for application to be ready...")
        .await
        .unwrap();

    service
        .log_error("deploy-456", "Connectivity check failed, retrying...")
        .await
        .unwrap();

    service
        .log_success(
            "deploy-456",
            "Connectivity check passed - server responding with status 200 OK",
        )
        .await
        .unwrap();

    // Read structured logs
    let logs = service.get_structured_logs("deploy-456").await.unwrap();

    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0].level, LogLevel::Info);
    assert_eq!(logs[1].level, LogLevel::Success);
    assert_eq!(logs[2].level, LogLevel::Warning);
    assert_eq!(logs[3].level, LogLevel::Error);
    assert_eq!(logs[4].level, LogLevel::Success);

    // Verify line numbers
    assert_eq!(logs[0].line, 1);
    assert_eq!(logs[1].line, 2);
    assert_eq!(logs[2].line, 3);
    assert_eq!(logs[3].line, 4);
    assert_eq!(logs[4].line, 5);
}

#[tokio::test]
async fn test_search_functionality() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    // Simulate deployment log from your screenshot
    service
        .log_info("search-test", "Deploying container image...")
        .await
        .unwrap();
    service
        .log_info(
            "search-test",
            "Detected EXPOSE directive in image: port 3000",
        )
        .await
        .unwrap();
    service
        .log_success("search-test", "Container is running")
        .await
        .unwrap();
    service
        .log_info("search-test", "Health check URL: http://localhost:56521/")
        .await
        .unwrap();
    service
        .log_error(
            "search-test",
            "Connectivity check failed (error sending request)",
        )
        .await
        .unwrap();
    service
        .log_success("search-test", "Connectivity check passed")
        .await
        .unwrap();

    // Search for "container"
    let results = service
        .search_structured_logs("search-test", "container")
        .await
        .unwrap();
    assert_eq!(results.len(), 2); // "Deploying container", "Container is running"

    // Search for "check"
    let results = service
        .search_structured_logs("search-test", "check")
        .await
        .unwrap();
    assert_eq!(results.len(), 3); // Health check, failed check, passed check

    // Search for "failed"
    let results = service
        .search_structured_logs("search-test", "failed")
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].level, LogLevel::Error);
}

#[tokio::test]
async fn test_filter_by_level() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    // Create mixed-level logs
    service.log_info("filter-test", "Info 1").await.unwrap();
    service
        .log_success("filter-test", "Success 1")
        .await
        .unwrap();
    service.log_error("filter-test", "Error 1").await.unwrap();
    service
        .log_warning("filter-test", "Warning 1")
        .await
        .unwrap();
    service.log_info("filter-test", "Info 2").await.unwrap();
    service.log_error("filter-test", "Error 2").await.unwrap();

    // Filter by errors only
    let errors = service
        .filter_structured_logs_by_level("filter-test", LogLevel::Error)
        .await
        .unwrap();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|e| e.level == LogLevel::Error));

    // Filter by successes
    let successes = service
        .filter_structured_logs_by_level("filter-test", LogLevel::Success)
        .await
        .unwrap();
    assert_eq!(successes.len(), 1);
}

#[tokio::test]
async fn test_log_with_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    let metadata = serde_json::json!({
        "container_id": "abc123",
        "port": 3000,
        "image": "nginx:latest"
    });

    service
        .append_structured_log_with_metadata(
            "metadata-test",
            LogLevel::Success,
            "Container deployed successfully",
            metadata,
        )
        .await
        .unwrap();

    let logs = service.get_structured_logs("metadata-test").await.unwrap();

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, LogLevel::Success);
    assert!(logs[0].metadata.is_some());

    let stored_metadata = logs[0].metadata.as_ref().unwrap();
    assert_eq!(stored_metadata["container_id"], "abc123");
    assert_eq!(stored_metadata["port"], 3000);
    assert_eq!(stored_metadata["image"], "nginx:latest");
}

#[tokio::test]
async fn test_json_serialization_for_frontend() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());

    service.log_info("ui-test", "Starting...").await.unwrap();
    service.log_success("ui-test", "Complete!").await.unwrap();
    service.log_warning("ui-test", "Waiting...").await.unwrap();
    service.log_error("ui-test", "Failed!").await.unwrap();

    let logs = service.get_structured_logs("ui-test").await.unwrap();

    // Verify log levels serialize correctly for frontend
    assert_eq!(logs[0].level, LogLevel::Info);
    assert_eq!(logs[1].level, LogLevel::Success);
    assert_eq!(logs[2].level, LogLevel::Warning);
    assert_eq!(logs[3].level, LogLevel::Error);

    // Example: Frontend will receive this JSON and render icons/colors
    for log in &logs {
        let json = serde_json::to_string(&log).unwrap();
        println!("{}", json);
        // Frontend maps: "info" -> ℹ️, "success" -> ✓, "warning" -> ⏳, "error" -> ✗
    }
}

#[tokio::test]
async fn test_real_world_deployment_scenario() {
    let temp_dir = TempDir::new().unwrap();
    let service = LogService::new(temp_dir.path().to_path_buf());
    let log_id = "deployment-789";

    // Simulate a real deployment workflow (like your screenshot)
    service
        .log_info(log_id, "🚀 Deploying container image...")
        .await
        .unwrap();
    service
        .log_info(log_id, "Detected EXPOSE directive in image: port 3000")
        .await
        .unwrap();
    service
        .log_info(log_id, "Detected EXPOSE directive in container: port 3000")
        .await
        .unwrap();

    let deployment_id = "1e6b4d9dc408dd4cd2859be27f4b7e1162ce4ce7b3d1ec99203ec8f4fe71e6cc57";
    service
        .log_success(log_id, format!("Deployment created: {}", deployment_id))
        .await
        .unwrap();

    service
        .log_warning(log_id, "⏳ Waiting for container to start...")
        .await
        .unwrap();
    service
        .log_success(log_id, "✓ Container is running")
        .await
        .unwrap();
    service
        .log_warning(log_id, "⏳ Waiting for application to be ready...")
        .await
        .unwrap();

    service
        .log_info(log_id, "Health check URL: http://localhost:56521/")
        .await
        .unwrap();
    service
        .log_error(
            log_id,
            "Connectivity check failed (error sending request for url (http://localhost:56521/)), retrying...",
        )
        .await
        .unwrap();

    service
        .log_success(
            log_id,
            "Connectivity check passed - server responding with status 200 OK (1/2)",
        )
        .await
        .unwrap();
    service
        .log_success(
            log_id,
            "Connectivity check passed - server responding with status 200 OK (2/2)",
        )
        .await
        .unwrap();

    // Read all logs
    let logs = service.get_structured_logs(log_id).await.unwrap();
    assert_eq!(logs.len(), 11);

    // Search for connectivity issues
    let connectivity_logs = service
        .search_structured_logs(log_id, "connectivity")
        .await
        .unwrap();
    assert_eq!(connectivity_logs.len(), 3);

    // Filter errors
    let errors = service
        .filter_structured_logs_by_level(log_id, LogLevel::Error)
        .await
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("failed"));

    // Filter successes
    let successes = service
        .filter_structured_logs_by_level(log_id, LogLevel::Success)
        .await
        .unwrap();
    assert_eq!(successes.len(), 4);

    // Backend returns JSON - frontend will handle rendering
    println!("\n=== Raw JSON logs (frontend will add icons/formatting) ===");
    for log in logs {
        let json = serde_json::to_value(&log).unwrap();
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    }
}
