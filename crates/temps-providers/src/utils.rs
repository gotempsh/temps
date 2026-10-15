// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use bollard::auth::DockerCredentials;
use bollard::query_parameters::CreateImageOptions;
use bollard::Docker;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use temps_core::retry::RetryConfig;
use thiserror::Error;
use tokio::time::Instant;
use tracing::{error, info, warn};

pub(crate) async fn ensure_network_exists(
    docker: &Docker,
) -> Result<(), Box<dyn std::error::Error>> {
    let network_name = temps_core::NETWORK_NAME.as_str();
    match temps_core::docker_network::ensure_bridge_network(docker, network_name).await {
        Ok(temps_core::docker_network::NetworkEnsured::Existing) => {}
        Ok(outcome) => info!("Network {} ready ({:?})", network_name, outcome),
        Err(e) => {
            error!("Failed to create network {}: {}", network_name, e);
            return Err(Box::new(e));
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

/// Longest a pull may go without the daemon reporting any progress. Docker
/// emits an event for every layer state change and for download/extract
/// progress, so a gap this long means the transfer is wedged (a registry that
/// accepted the connection and went quiet, a dead mirror), not slow.
const PULL_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Ceiling on a single pull attempt, however much progress it reports. Far
/// above a cold pull of the largest images Temps manages on a slow link; it
/// exists so that a trickle of progress events cannot hold a caller forever.
const PULL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(900);

/// Why one pull attempt did not finish.
#[derive(Error, Debug)]
enum PullAttemptError {
    /// The daemon (or the registry behind it) reported an error.
    #[error("{0}")]
    Docker(bollard::errors::Error),
    /// The pull stream produced nothing for `idle_secs`.
    #[error("no progress from the Docker daemon for {idle_secs}s")]
    Stalled { idle_secs: u64 },
    /// The pull kept reporting progress but did not finish within `total_secs`.
    #[error("pull did not finish within {total_secs}s")]
    TimedOut { total_secs: u64 },
}

impl PullAttemptError {
    /// Only a definitive registry answer is terminal. A stall or timeout says
    /// nothing about the image, so it is retried like a dropped connection.
    fn is_terminal(&self) -> bool {
        match self {
            PullAttemptError::Docker(e) => is_terminal_pull_error(e),
            PullAttemptError::Stalled { .. } | PullAttemptError::TimedOut { .. } => false,
        }
    }
}

/// Drain a pull stream to its end, giving up when it goes quiet for `idle` or
/// runs for longer than `total`.
async fn drain_pull_stream<S, T>(
    mut stream: S,
    idle: Duration,
    total: Duration,
) -> Result<(), PullAttemptError>
where
    S: Stream<Item = Result<T, bollard::errors::Error>> + Unpin,
{
    let deadline = Instant::now() + total;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(PullAttemptError::TimedOut {
                total_secs: total.as_secs(),
            });
        }
        let wait = idle.min(remaining);
        match tokio::time::timeout(wait, stream.next()).await {
            Ok(None) => return Ok(()),
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => return Err(PullAttemptError::Docker(e)),
            // The overall ceiling and the idle limit use one clock; report
            // whichever of the two actually ran out.
            Err(_) if remaining <= idle => {
                return Err(PullAttemptError::TimedOut {
                    total_secs: total.as_secs(),
                })
            }
            Err(_) => {
                return Err(PullAttemptError::Stalled {
                    idle_secs: idle.as_secs(),
                })
            }
        }
    }
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
///
/// Registry unavailable, image already on the host: when every pull attempt
/// fails but the exact reference is already present locally, this logs a
/// warning and returns `Ok(())` so the caller runs the local copy. Every
/// caller pulls only to make sure the image is there before creating or
/// recreating a container from it, so a local copy satisfies all of them.
/// Without this, an upstream withdrawal (MinIO pulled every
/// `quay.io/minio/*` and Docker Hub `minio/*` image in 2026) or a registry
/// outage stops services from even restarting on hosts that already hold the
/// image. The cost is that a moved tag is not refreshed while the registry
/// is failing, which the warning makes visible. A digest-pinned reference is
/// only found locally when the digest matches, so pinning still holds.
///
/// A pull never waits forever: an attempt that reports no progress for
/// `PULL_IDLE_TIMEOUT`, or runs past `PULL_ATTEMPT_TIMEOUT`, is abandoned and
/// handled like any other transient failure (retried, then the local-copy
/// fallback above).
pub(crate) async fn pull_image_with_retry(
    docker: &Docker,
    image: &str,
    registry_credentials: Option<DockerCredentials>,
) -> Result<(), String> {
    let retry = RetryConfig::new(3)
        .with_base_delay(Duration::from_secs(2))
        .with_max_delay(Duration::from_secs(15));

    for attempt in 0..retry.max_attempts {
        let stream = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            registry_credentials.clone(),
        );

        let e = match drain_pull_stream(Box::pin(stream), PULL_IDLE_TIMEOUT, PULL_ATTEMPT_TIMEOUT)
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };

        let is_last_attempt = attempt + 1 >= retry.max_attempts;
        if e.is_terminal() || is_last_attempt {
            if docker.inspect_image(image).await.is_ok() {
                warn!(
                    "Failed to pull image '{}' ({}); using the copy already present on this host",
                    image, e
                );
                return Ok(());
            }
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

    /// A registry failure must not stop a service whose image is already on
    /// the host. The reference below exists only locally (tagged from
    /// busybox), so every pull fails with "not found" from Docker Hub.
    #[tokio::test]
    async fn test_pull_image_with_retry_falls_back_to_a_local_image() {
        use bollard::query_parameters::{RemoveImageOptions, TagImageOptionsBuilder};

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
        if pull_image_with_retry(&docker, "busybox:latest", None)
            .await
            .is_err()
        {
            println!("busybox:latest unavailable, skipping");
            return;
        }

        let repo = "temps-local-only-image-fixture";
        let tag = format!("t{}", rand::random::<u32>());
        let local_only = format!("{repo}:{tag}");
        docker
            .tag_image(
                "busybox:latest",
                Some(TagImageOptionsBuilder::new().repo(repo).tag(&tag).build()),
            )
            .await
            .expect("tagging busybox locally should succeed");

        let result = pull_image_with_retry(&docker, &local_only, None).await;

        let _ = docker
            .remove_image(&local_only, None::<RemoveImageOptions>, None)
            .await;
        assert!(
            result.is_ok(),
            "a failed pull of an image present locally must fall back to it: {result:?}"
        );
    }

    fn docker_404() -> bollard::errors::Error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "manifest unknown".to_string(),
        }
    }

    #[tokio::test]
    async fn drain_pull_stream_succeeds_when_the_stream_ends() {
        let stream = futures::stream::iter(vec![Ok(()), Ok(()), Ok(())]);
        let result =
            drain_pull_stream(stream, Duration::from_secs(5), Duration::from_secs(5)).await;
        assert!(result.is_ok(), "a finished pull must succeed: {result:?}");
    }

    #[tokio::test]
    async fn drain_pull_stream_reports_a_stalled_pull_instead_of_waiting_forever() {
        // One progress event, then the daemon goes silent without closing.
        let stream = futures::stream::iter(vec![Ok(())]).chain(futures::stream::pending());
        let started = std::time::Instant::now();

        let result = drain_pull_stream(
            Box::pin(stream),
            Duration::from_millis(100),
            Duration::from_secs(30),
        )
        .await;

        assert!(
            matches!(result, Err(PullAttemptError::Stalled { .. })),
            "a silent stream must be reported as stalled: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the stall must be detected at the idle limit, not the overall one"
        );
    }

    #[tokio::test]
    async fn drain_pull_stream_caps_a_pull_that_keeps_reporting_progress() {
        // Events arrive faster than the idle limit, so only the overall
        // ceiling can stop this pull.
        let stream = futures::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((Ok::<(), bollard::errors::Error>(()), ()))
        });

        let result = drain_pull_stream(
            Box::pin(stream),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .await;

        assert!(
            matches!(result, Err(PullAttemptError::TimedOut { .. })),
            "an endless trickle of progress must hit the overall ceiling: {result:?}"
        );
    }

    #[tokio::test]
    async fn drain_pull_stream_surfaces_the_daemons_error() {
        let stream = futures::stream::iter(vec![Ok(()), Err(docker_404())]);
        let result =
            drain_pull_stream(stream, Duration::from_secs(5), Duration::from_secs(5)).await;
        let error = result.expect_err("a pull error must be returned");
        assert!(matches!(error, PullAttemptError::Docker(_)), "{error:?}");
        assert!(error.is_terminal(), "a 404 from the registry is terminal");
    }

    #[test]
    fn a_stalled_or_timed_out_pull_is_retried_not_terminal() {
        assert!(!PullAttemptError::Stalled { idle_secs: 120 }.is_terminal());
        assert!(!PullAttemptError::TimedOut { total_secs: 900 }.is_terminal());
        assert!(
            !PullAttemptError::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 503,
                message: "registry unavailable".to_string(),
            })
            .is_terminal(),
            "a 5xx from the registry is transient"
        );
    }

    #[test]
    fn pull_attempt_errors_say_what_happened() {
        assert_eq!(
            PullAttemptError::Stalled { idle_secs: 120 }.to_string(),
            "no progress from the Docker daemon for 120s"
        );
        assert_eq!(
            PullAttemptError::TimedOut { total_secs: 900 }.to_string(),
            "pull did not finish within 900s"
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
