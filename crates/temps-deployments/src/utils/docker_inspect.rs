//! Docker image inspection utilities
//!
//! Provides functions to inspect Docker images and extract configuration like exposed ports

use bollard::Docker;
use tracing::{debug, warn};

/// Extract exposed ports from a Docker image
///
/// This function inspects a Docker image and returns a list of ports that are exposed
/// via EXPOSE directives in the Dockerfile.
///
/// # Arguments
/// * `docker` - Bollard Docker client
/// * `image_name` - Name of the image to inspect (e.g., "nginx:latest", "myapp:v1.0")
///
/// # Returns
/// * `Ok(Vec<u16>)` - List of exposed ports (e.g., [80, 443])
/// * `Err(anyhow::Error)` - If image inspection fails
///
/// # Example
/// ```no_run
/// use bollard::Docker;
/// use temps_deployments::utils::docker_inspect::get_exposed_ports;
///
/// # async fn example() -> Result<(), anyhow::Error> {
/// let docker = Docker::connect_with_local_defaults()?;
/// let ports = get_exposed_ports(&docker, "nginx:latest").await?;
/// println!("Exposed ports: {:?}", ports); // [80, 443]
/// # Ok(())
/// # }
/// ```
pub async fn get_exposed_ports(docker: &Docker, image_name: &str) -> anyhow::Result<Vec<u16>> {
    debug!("🔍 Inspecting Docker image: {}", image_name);

    // Inspect the image to get its configuration
    let image_info = docker
        .inspect_image(image_name)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to inspect image {}: {}", image_name, e))?;

    // Extract exposed ports from the image config
    let mut exposed_ports = Vec::new();

    if let Some(config) = image_info.config {
        if let Some(exposed_ports_map) = config.exposed_ports {
            // exposed_ports_map is a HashMap<String, HashMap<String, String>>
            // Keys are in format like "80/tcp", "443/tcp", "8080/udp"
            for (port_spec, _) in exposed_ports_map {
                // Parse port from format "80/tcp" or "443/tcp"
                if let Some(port_str) = port_spec.split('/').next() {
                    if let Ok(port) = port_str.parse::<u16>() {
                        debug!("✅ Found exposed port: {} (from {})", port, port_spec);
                        exposed_ports.push(port);
                    } else {
                        warn!("⚠️  Failed to parse port from spec: {}", port_spec);
                    }
                }
            }
        }
    }

    if exposed_ports.is_empty() {
        debug!("⚠️  No exposed ports found in image {}", image_name);
    } else {
        debug!(
            "📋 Image {} exposes {} port(s): {:?}",
            image_name,
            exposed_ports.len(),
            exposed_ports
        );
    }

    Ok(exposed_ports)
}

/// Get the primary exposed port from a Docker image
///
/// Returns the first exposed port found in the image, or None if no ports are exposed.
/// This is useful when you need a single port value.
///
/// # Arguments
/// * `docker` - Bollard Docker client
/// * `image_name` - Name of the image to inspect
///
/// # Returns
/// * `Ok(Some(port))` - First exposed port found
/// * `Ok(None)` - No exposed ports found
/// * `Err(anyhow::Error)` - If image inspection fails
pub async fn get_primary_port(docker: &Docker, image_name: &str) -> anyhow::Result<Option<u16>> {
    let ports = get_exposed_ports(docker, image_name).await?;
    Ok(ports.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require Docker to be running and will pull images if not available
    // They are marked as #[ignore] by default to avoid issues in CI

    #[tokio::test]
    #[ignore = "requires Docker and network access"]
    async fn test_get_exposed_ports_nginx() -> Result<(), anyhow::Error> {
        let docker = Docker::connect_with_local_defaults()?;

        // Nginx image exposes ports 80 and 443
        let ports = get_exposed_ports(&docker, "nginx:latest").await?;

        assert!(!ports.is_empty(), "Nginx should expose at least one port");
        assert!(
            ports.contains(&80),
            "Nginx should expose port 80, got {:?}",
            ports
        );

        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires Docker and network access"]
    async fn test_get_primary_port() -> Result<(), anyhow::Error> {
        let docker = Docker::connect_with_local_defaults()?;

        let primary_port = get_primary_port(&docker, "nginx:latest").await?;

        assert!(primary_port.is_some(), "Should find a primary port");
        assert_eq!(
            primary_port,
            Some(80),
            "Nginx primary port should be 80, got {:?}",
            primary_port
        );

        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_get_exposed_ports_nonexistent() {
        let docker = Docker::connect_with_local_defaults().unwrap();

        // This should fail because the image doesn't exist
        let result = get_exposed_ports(&docker, "nonexistent-image-12345:latest").await;

        assert!(result.is_err(), "Should fail for nonexistent image");
    }
}
