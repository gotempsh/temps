use bollard::Docker;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use temps_providers::externalsvc::{ExternalService, MongodbService, ServiceConfig, ServiceType};
use tokio::time::{sleep, Duration};

#[tokio::test]
#[ignore] // Run with: cargo test --test mongodb_container_start_test -- --ignored --nocapture
async fn test_mongodb_container_starts_successfully() {
    println!("🚀 MongoDB Container Start Test");
    println!("================================\n");

    // Connect to Docker
    let docker = Docker::connect_with_local_defaults().expect("Failed to connect to Docker");
    let docker = Arc::new(docker);

    // Find an available port
    let test_port = find_available_port(27017).expect("No available ports");
    println!("✓ Using port: {}", test_port);

    // Create a unique service name with timestamp
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let service_name = format!("test-{}", timestamp);

    println!("✓ Service name: {}", service_name);

    // Create MongoDB service
    let service = MongodbService::new(service_name.clone(), docker.clone());

    // Create service config WITHOUT password (should auto-generate)
    let mut parameters = HashMap::new();
    parameters.insert("host".to_string(), json!("localhost"));
    parameters.insert("port".to_string(), json!(test_port.to_string()));
    parameters.insert("database".to_string(), json!("admin"));
    parameters.insert("username".to_string(), json!("root"));
    // NO password - should be auto-generated

    let service_config = ServiceConfig {
        name: service_name.clone(),
        service_type: ServiceType::Mongodb,
        version: Some("7.0".to_string()),
        parameters: json!(parameters),
    };

    // Initialize the service (stores config)
    println!("\n📝 Initializing service...");
    let init_result = service.init(service_config).await;
    assert!(init_result.is_ok(), "Failed to init service: {:?}", init_result.err());
    println!("✓ Service initialized");

    // Start the service (creates and starts container)
    println!("\n🐳 Starting MongoDB container...");
    let start_result = service.start().await;

    if let Err(e) = &start_result {
        println!("❌ Failed to start container: {}", e);

        // Check if container was created
        let container_name = format!("temps-mongodb-{}", service_name);
        println!("\n🔍 Checking container status...");

        match docker.inspect_container(&container_name, None).await {
            Ok(info) => {
                println!("Container found!");
                println!("  Status: {:?}", info.state.as_ref().map(|s| &s.status));
                println!("  Running: {:?}", info.state.as_ref().map(|s| s.running));
                println!("  Health: {:?}", info.state.as_ref().and_then(|s| s.health.as_ref()));

                // Get logs
                println!("\n📋 Container logs:");
                println!("--------------------");
                use bollard::query_parameters::LogsOptions;
                use futures::StreamExt;

                let mut log_stream = docker.logs(
                    &container_name,
                    Some(LogsOptions {
                        stdout: true,
                        stderr: true,
                        tail: Some("50".to_string()),
                        ..Default::default()
                    }),
                );

                while let Some(log) = log_stream.next().await {
                    if let Ok(log) = log {
                        print!("{}", log);
                    }
                }
                println!("--------------------");
            }
            Err(e) => {
                println!("❌ Container not found: {}", e);
            }
        }

        // Cleanup
        let _ = service.cleanup().await;
        panic!("Container failed to start");
    }

    println!("✓ Container started");

    // Give it a moment to fully initialize
    println!("\n⏳ Waiting for MongoDB to be ready...");
    sleep(Duration::from_secs(5)).await;

    // Check if container is running
    let container_name = format!("temps-mongodb-{}", service_name);
    let inspect_result = docker.inspect_container(&container_name, None).await;

    assert!(inspect_result.is_ok(), "Failed to inspect container");

    let info = inspect_result.unwrap();
    let is_running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);

    println!("✓ Container is running: {}", is_running);
    assert!(is_running, "Container is not running");

    // Check health status
    if let Some(state) = info.state {
        if let Some(health) = state.health {
            println!("✓ Health status: {:?}", health.status);
        }
    }

    // Test health check
    println!("\n🏥 Testing health check...");
    let health_result = service.health_check().await;

    match &health_result {
        Ok(healthy) => println!("✓ Health check result: {}", healthy),
        Err(e) => println!("❌ Health check failed: {}", e),
    }

    // Get connection info
    println!("\n🔌 Getting connection info...");
    let connection_info = service.get_connection_info();

    match &connection_info {
        Ok(conn) => {
            println!("✓ Connection string: {}", conn);
            // Verify password is present (not empty)
            assert!(conn.contains("@"), "Connection string should contain password");
        }
        Err(e) => println!("❌ Failed to get connection info: {}", e),
    }

    // Cleanup
    println!("\n🧹 Cleaning up...");
    let cleanup_result = service.cleanup().await;
    assert!(cleanup_result.is_ok(), "Failed to cleanup: {:?}", cleanup_result.err());
    println!("✓ Cleaned up");

    // Verify container is removed
    let inspect_after_cleanup = docker.inspect_container(&container_name, None).await;
    assert!(inspect_after_cleanup.is_err(), "Container should be removed after cleanup");
    println!("✓ Container removed");

    println!("\n✅ All tests passed!");
}

fn find_available_port(start_port: u16) -> Option<u16> {
    use std::net::TcpListener;
    (start_port..start_port + 100).find(|&port| TcpListener::bind(("0.0.0.0", port)).is_ok())
}
