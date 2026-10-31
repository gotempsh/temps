// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
/// let ports = get_exposed_ports(&docker, TEST_IMAGE).await?;
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
            // exposed_ports_map is a Vec<String>
            // Items are in format like "80/tcp", "443/tcp", "8080/udp"
            for port_spec in exposed_ports_map {
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

    // These tests inspect real images (pulling nginx if it is missing), so
    // they skip at runtime when no Docker engine answers (CLAUDE.md: no
    // `#[ignore]` on Docker tests).
    const TEST_IMAGE: &str = "nginx:alpine";

    async fn docker() -> Option<Docker> {
        use futures::StreamExt;

        let docker = Docker::connect_with_local_defaults().ok()?;
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return None;
        }
        if docker.inspect_image(TEST_IMAGE).await.is_err() {
            let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
                .from_image(TEST_IMAGE)
                .build();
            let mut pull = docker.create_image(Some(options), None, None);
            while let Some(progress) = pull.next().await {
                if let Err(e) = progress {
                    println!("Cannot pull {}: {}, skipping", TEST_IMAGE, e);
                    return None;
                }
            }
        }
        Some(docker)
    }

    #[tokio::test]
    async fn test_get_exposed_ports_nginx() -> Result<(), anyhow::Error> {
        let Some(docker) = docker().await else {
            return Ok(());
        };

        // Nginx image exposes ports 80 and 443
        let ports = get_exposed_ports(&docker, TEST_IMAGE).await?;

        assert!(!ports.is_empty(), "Nginx should expose at least one port");
        assert!(
            ports.contains(&80),
            "Nginx should expose port 80, got {:?}",
            ports
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_primary_port() -> Result<(), anyhow::Error> {
        let Some(docker) = docker().await else {
            return Ok(());
        };

        let primary_port = get_primary_port(&docker, TEST_IMAGE).await?;

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
    async fn test_get_exposed_ports_nonexistent() {
        let Some(docker) = docker().await else {
            return;
        };

        // This should fail because the image doesn't exist
        let result = get_exposed_ports(&docker, "nonexistent-image-12345:latest").await;

        assert!(result.is_err(), "Should fail for nonexistent image");
    }
}
