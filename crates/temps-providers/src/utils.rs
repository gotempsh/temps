use bollard::auth::DockerCredentials;
use bollard::query_parameters::CreateImageOptions;
use bollard::{models::NetworkCreateRequest, query_parameters::ListNetworksOptions, Docker};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use temps_core::retry::RetryConfig;
use tracing::{error, info, warn};

pub(crate) async fn ensure_network_exists(
    docker: &Docker,
) -> Result<(), Box<dyn std::error::Error>> {
    let network_name = temps_core::NETWORK_NAME.as_str();

    // Check if network exists
    let networks = docker.list_networks(None::<ListNetworksOptions>).await?;
    let network_exists = networks
        .iter()
        .any(|n| n.name.as_deref() == Some(network_name));

    if !network_exists {
        info!("Creating network: {}", network_name);
        let options = NetworkCreateRequest {
            name: network_name.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        };

        match docker.create_network(options).await {
            Ok(_) => info!("Successfully created network: {}", network_name),
            Err(e) => {
                error!("Failed to create network: {}", e);
                return Err(Box::new(e));
            }
        }
    }

    Ok(())
}

/// Create a Docker log configuration for external service containers.
/// Uses `json-file` driver with configurable size limits to prevent unbounded log growth.
///
/// Default: 20MB max per file, 3 rotated files = 60MB max total per container.
pub(crate) fn service_log_config(
    max_size: &str,
    max_file: u32,
) -> bollard::models::HostConfigLogConfig {
    let mut config = HashMap::new();
    config.insert("max-size".to_string(), max_size.to_string());
    config.insert("max-file".to_string(), max_file.to_string());

    bollard::models::HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(config),
    }
}

/// Create default Docker log configuration for external service containers.
/// 20MB max per file, 3 rotated files = 60MB max total.
pub(crate) fn default_service_log_config() -> bollard::models::HostConfigLogConfig {
    service_log_config("20m", 3)
}

/// Build a Docker port-binding map that maps `container_port_key`
/// (e.g. `"5432/tcp"`) to `host_port` on the loopback interface only.
///
/// This is the single source of truth for how managed-service containers
/// (Postgres, Redis, MongoDB, S3/RustFS) publish their ports to the host.
/// It must always bind to `127.0.0.1` — never `0.0.0.0` — so services are
/// reachable from the host and from other containers on the Docker network,
/// but never from outside the server without an explicit reverse proxy or
/// port-forward. This matches the documented behavior in
/// `docs/howto/set-up-managed-services`. See bherila/temps#29.
pub(crate) fn local_port_binding(
    container_port_key: &str,
    host_port: &str,
) -> HashMap<String, Option<Vec<bollard::models::PortBinding>>> {
    HashMap::from([(
        container_port_key.to_string(),
        Some(vec![bollard::models::PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(host_port.to_string()),
        }]),
    )])
}

/// True for a definitive "this image cannot be pulled, ever" response from
/// the registry (image doesn't exist, access denied, bad reference) — a 4xx
/// `DockerResponseServerError`. Everything else (a mid-stream drop like
/// "bytes remaining on stream", a hyper/IO error, a request timeout, or a
/// 5xx from the registry) is presumed transient. Matches this project's own
/// resilience rule: retry transient failures, not not-found/auth errors.
fn is_terminal_pull_error(e: &bollard::errors::Error) -> bool {
    matches!(
        e,
        bollard::errors::Error::DockerResponseServerError { status_code, .. }
            if (400..500).contains(status_code)
    )
}

/// Pull a Docker image, retrying transient stream failures (e.g. "bytes
/// remaining on stream" from a connection dropped mid-transfer) instead of
/// failing the whole provisioning attempt on the first hiccup. Does NOT
/// retry a terminal failure (image not found, access denied) — see
/// `is_terminal_pull_error` — so a typo'd image name fails fast instead of
/// burning ~20s on backoff for something that will never succeed.
///
/// `image` is the full reference including tag (e.g. `"mongo:8"`), matching
/// how every call site already builds it. Docker's daemon caches layers by
/// digest, so a retried pull only re-fetches whatever didn't finish, not the
/// whole image — cheap even for large images like MongoDB's. This is why a
/// bare retry (no attempt to resume mid-stream) is the right fix here rather
/// than something more elaborate.
///
/// Returns `Err(String)` with the image name and last error already folded
/// in, so callers can pass it straight to their existing error type/message
/// via `.map_err(...)` without needing to know the retry happened.
pub(crate) async fn pull_image_with_retry(
    docker: &Docker,
    image: &str,
    registry_credentials: Option<DockerCredentials>,
) -> Result<(), String> {
    let retry = RetryConfig::new(3)
        .with_base_delay(Duration::from_secs(2))
        .with_max_delay(Duration::from_secs(15));

    for attempt in 0..retry.max_attempts {
        let mut stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            registry_credentials.clone(),
        );

        let mut pull_err = None;
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                pull_err = Some(e);
                break;
            }
        }

        let Some(e) = pull_err else {
            return Ok(());
        };

        let is_last_attempt = attempt + 1 >= retry.max_attempts;
        if is_terminal_pull_error(&e) || is_last_attempt {
            warn!("Failed to pull image '{}': {}", image, e);
            return Err(format!("failed to pull image '{}': {}", image, e));
        }

        let delay = retry.compute_delay(attempt);
        warn!(
            "Pull attempt {}/{} for image '{}' failed, retrying in {:?}: {}",
            attempt + 1,
            retry.max_attempts,
            image,
            delay,
            e
        );
        tokio::time::sleep(delay).await;
    }

    unreachable!("loop always returns Ok or Err before exhausting max_attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pull_image_with_retry_pulls_a_real_image() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker unavailable, skipping: {e}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }

        let result = pull_image_with_retry(&docker, "busybox:latest", None).await;
        assert!(result.is_ok(), "expected pull to succeed: {result:?}");
    }

    #[tokio::test]
    async fn test_pull_image_with_retry_reports_the_image_name_on_failure() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => d,
            Err(e) => {
                println!("Docker unavailable, skipping: {e}");
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }

        let result = pull_image_with_retry(
            &docker,
            "temps-nonexistent-image-fixture:does-not-exist",
            None,
        )
        .await;
        let err = result.expect_err("pulling a nonexistent image must fail");
        assert!(
            err.contains("temps-nonexistent-image-fixture:does-not-exist"),
            "error should name the image that failed to pull: {err}"
        );
    }

    #[test]
    fn test_local_port_binding_binds_to_loopback_only() {
        let bindings = local_port_binding("5432/tcp", "15432");

        let port_binding = bindings
            .get("5432/tcp")
            .expect("port bindings should contain the requested container port key")
            .as_ref()
            .expect("port binding list should be present")
            .first()
            .expect("port binding list should have one entry");

        assert_eq!(
            port_binding.host_ip.as_deref(),
            Some("127.0.0.1"),
            "managed service ports must bind to loopback only, never 0.0.0.0"
        );
        assert_eq!(port_binding.host_port.as_deref(), Some("15432"));
    }

    #[test]
    fn test_local_port_binding_never_binds_to_all_interfaces() {
        for (container_port, host_port) in [
            ("5432/tcp", "5432"),
            ("6379/tcp", "6379"),
            ("9000/tcp", "9000"),
        ] {
            let bindings = local_port_binding(container_port, host_port);
            let host_ip = bindings
                .get(container_port)
                .and_then(|b| b.as_ref())
                .and_then(|v| v.first())
                .and_then(|pb| pb.host_ip.as_deref());
            assert_ne!(
                host_ip,
                Some("0.0.0.0"),
                "port {container_port} must never bind to 0.0.0.0"
            );
        }
    }
}
