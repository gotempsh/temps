//! Container readiness probing.
//!
//! "Running" (Docker's container state) is not the same as "able to serve a
//! request": a container can be `Running` for some time before the process
//! inside binds its listening port and starts answering HTTP. The scale-to-zero
//! **wake** path (`temps-proxy`) holds the first request that wakes a sleeping
//! environment until the container can actually serve it — otherwise that
//! request races startup and gets a spurious upstream-connect 503.
//!
//! ## Why HTTP, not a bare TCP connect
//!
//! A TCP-connect probe is **not** sufficient. Docker's userland proxy
//! (`docker-proxy`, the default on Docker Desktop / macOS and on Linux unless
//! disabled) starts accepting connections on the mapped host port the instant
//! the container starts — *before* the app inside has bound its port. So a TCP
//! handshake to `127.0.0.1:{host_port}` succeeds immediately and reports a
//! not-yet-ready container as "ready", defeating the whole point of the probe.
//!
//! Verified empirically: a container that sleeps 20s before binding still
//! accepts a TCP connect on its mapped host port at t=0, but an HTTP GET returns
//! no response until the app is actually up. So the probe issues an **HTTP GET**
//! and only treats the container as ready once it gets a real HTTP status line
//! back. This matches what `temps-deployments`' `DeployImageJob` already does
//! for fresh deploys (`reqwest` GET against the health-check path).
//!
//! ## URL resolution
//!
//! The probe URL is built with [`temps_core::DeploymentMode::build_container_url`]
//! — the same helper the deploy path uses — so it is correct in both modes:
//! * **Docker mode** (control plane runs as a container on the shared app
//!   network): `http://{container_name}:{container_port}` — straight to the app
//!   over the Docker network, bypassing `docker-proxy` entirely.
//! * **Baremetal mode** (control plane on the host): `http://127.0.0.1:{host_port}`
//!   — through `docker-proxy`, where requiring a real HTTP response is exactly
//!   what avoids the false-positive above.
//!
//! ## Scope
//!
//! For the local single-node case this is correct. Containers on *remote* worker
//! nodes are not reachable this way (and a local deployer can't even
//! `get_container_info` a remote container); multi-node wake readiness is
//! tracked separately. A container that publishes **no** host port has nothing
//! to probe over HTTP, so the `Running` state is taken as ready.

use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{ContainerDeployer, ContainerStatus, DeployerError};

/// Path the readiness probe requests. We don't need the app to have a real
/// health endpoint — *any* HTTP response proves the server is up — so we hit the
/// root. A 404/405 here still counts as ready (see [`is_ready_status`]).
const PROBE_PATH: &str = "/";

/// Why a readiness probe stopped before reporting the container ready.
#[derive(Error, Debug)]
pub enum ReadinessError {
    /// The container did not start answering HTTP within the budget.
    #[error(
        "Container {container_id} did not become ready within {timeout_secs}s \
         (last status: {last_status:?})"
    )]
    Timeout {
        container_id: String,
        timeout_secs: u64,
        /// The container's Docker status at the final poll, for diagnosis
        /// (e.g. `Exited` means it crashed on boot rather than booting slowly).
        last_status: Option<ContainerStatus>,
    },

    /// The container reached a terminal state (exited/dead) — it will never
    /// answer, so there is no point waiting out the timeout.
    #[error("Container {container_id} is in terminal state {status:?} and will not become ready")]
    Terminal {
        container_id: String,
        status: ContainerStatus,
    },

    /// Inspecting the container failed (the deployer errored).
    #[error("Failed to inspect container {container_id} while waiting for readiness: {source}")]
    Inspect {
        container_id: String,
        #[source]
        source: DeployerError,
    },

    /// The HTTP client could not be constructed.
    #[error("Failed to build readiness HTTP client: {0}")]
    Client(String),
}

/// Tunables for [`wait_until_accepting_requests`]. `Default` matches the values
/// the scale-to-zero wake path historically used inline.
#[derive(Debug, Clone, Copy)]
pub struct ReadinessProbe {
    /// Total budget for the container to start answering HTTP.
    pub timeout: Duration,
    /// Delay between successive readiness checks.
    pub poll_interval: Duration,
    /// Per-attempt HTTP request timeout. Kept short so a hung request doesn't
    /// eat the whole poll interval.
    pub request_timeout: Duration,
}

impl Default for ReadinessProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
            request_timeout: Duration::from_secs(2),
        }
    }
}

impl ReadinessProbe {
    /// Build a probe with the given overall timeout, keeping the default poll
    /// and request intervals.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }
}

/// Outcome of a single readiness check (see [`check_accepting_requests`]).
#[derive(Debug, Clone)]
pub enum ReadinessCheck {
    /// The container is `Running` and answering HTTP (or has no port to probe).
    /// Safe to route/serve traffic.
    Ready,
    /// The container isn't ready yet but may still become ready — keep polling.
    /// Carries the observed status for logging.
    NotYet(ContainerStatus),
    /// The container is in a terminal state and will never become ready.
    Terminal(ContainerStatus),
}

/// Is this HTTP status proof the server is up and serving?
///
/// Matches `DeployImageJob`'s health-check semantics: *any* HTTP response means
/// the process is bound and answering. 2xx/3xx are healthy; 404/405 mean the
/// probe path doesn't exist but the server is up; only 5xx (and no response at
/// all) count as not-ready.
fn is_ready_status(status: reqwest::StatusCode) -> bool {
    status.is_success()
        || status.is_redirection()
        || status.as_u16() == 404
        || status.as_u16() == 405
}

/// Perform a **single** readiness check: inspect the container, and if it is
/// `Running`, issue one HTTP GET to its resolved URL.
///
/// This is the one-shot primitive shared by the scale-to-zero wake path (which
/// runs its own outer poll loop) and [`wait_until_accepting_requests`] (which
/// loops over this). The probe targets the **lowest** published host port
/// deterministically — Docker reports ports as an unordered map, so picking
/// `.first()` would be unstable for a container that publishes more than one.
/// A container with no published port is `Ready` once `Running` (nothing to
/// probe over HTTP).
pub async fn check_accepting_requests(
    deployer: &Arc<dyn ContainerDeployer>,
    container_id: &str,
    request_timeout: Duration,
) -> Result<ReadinessCheck, ReadinessError> {
    let client = build_client(request_timeout)?;
    check_accepting_requests_with_client(deployer, container_id, &client).await
}

/// Like [`check_accepting_requests`] but reuses a caller-provided client, so a
/// poll loop doesn't rebuild one per attempt.
async fn check_accepting_requests_with_client(
    deployer: &Arc<dyn ContainerDeployer>,
    container_id: &str,
    client: &reqwest::Client,
) -> Result<ReadinessCheck, ReadinessError> {
    let info = deployer
        .get_container_info(container_id)
        .await
        .map_err(|e| ReadinessError::Inspect {
            container_id: container_id.to_string(),
            source: e,
        })?;

    Ok(match info.status {
        ContainerStatus::Running => {
            // Pick the lowest host port deterministically (Docker's port map has
            // no defined iteration order).
            match info.ports.iter().map(|p| p.host_port).min() {
                // No published port → nothing to probe over HTTP; trust Running.
                None => ReadinessCheck::Ready,
                Some(host_port) => {
                    // Build the URL the same way the deploy/proxy paths do, so
                    // Docker mode hits the app directly over the network and
                    // baremetal mode hits the mapped host port.
                    let container_port = info
                        .ports
                        .iter()
                        .find(|p| p.host_port == host_port)
                        .map(|p| p.container_port)
                        .unwrap_or(host_port);
                    let url = temps_core::DeploymentMode::build_container_url(
                        &info.container_name,
                        container_port,
                        host_port,
                        Some(PROBE_PATH),
                    );
                    if probe_http(client, &url).await {
                        ReadinessCheck::Ready
                    } else {
                        ReadinessCheck::NotYet(ContainerStatus::Running)
                    }
                }
            }
        }
        // Terminal: the container will never become ready.
        status @ (ContainerStatus::Exited | ContainerStatus::Dead) => {
            ReadinessCheck::Terminal(status)
        }
        // Not ready yet but not terminal — e.g. start_container is still
        // taking effect.
        status
        @ (ContainerStatus::Created | ContainerStatus::Paused | ContainerStatus::Stopped) => {
            ReadinessCheck::NotYet(status)
        }
    })
}

/// Wait until `container_id` is answering HTTP, polling
/// [`check_accepting_requests`] until ready or the probe budget is exhausted.
///
/// Returns `Ok(())` as soon as the container returns a usable HTTP response. A
/// container that publishes no host port is considered ready once it is
/// `Running` (there is nothing to probe). A container that reaches a terminal
/// state (`Exited`/`Dead`) fails fast with [`ReadinessError::Terminal`] rather
/// than waiting out the full timeout — a crash-on-boot shouldn't cost the caller
/// the whole budget.
pub async fn wait_until_accepting_requests(
    deployer: &Arc<dyn ContainerDeployer>,
    container_id: &str,
    probe: ReadinessProbe,
) -> Result<(), ReadinessError> {
    let client = build_client(probe.request_timeout)?;
    let start = Instant::now();

    loop {
        // The status observed this iteration — surfaced in the timeout error so
        // a stuck wake reports *why* (e.g. still `Created`, or `Running` but
        // not yet answering HTTP).
        let last_status =
            match check_accepting_requests_with_client(deployer, container_id, &client).await? {
                ReadinessCheck::Ready => return Ok(()),
                ReadinessCheck::Terminal(status) => {
                    return Err(ReadinessError::Terminal {
                        container_id: container_id.to_string(),
                        status,
                    });
                }
                ReadinessCheck::NotYet(status) => status,
            };

        if start.elapsed() >= probe.timeout {
            return Err(ReadinessError::Timeout {
                container_id: container_id.to_string(),
                timeout_secs: probe.timeout.as_secs(),
                last_status: Some(last_status),
            });
        }

        tokio::time::sleep(probe.poll_interval).await;
    }
}

/// Build the readiness HTTP client. Redirects are NOT followed — a 3xx already
/// proves the server is up, and following it could hit an unrelated host.
fn build_client(request_timeout: Duration) -> Result<reqwest::Client, ReadinessError> {
    reqwest::Client::builder()
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ReadinessError::Client(e.to_string()))
}

/// Issue one HTTP GET. Any HTTP status line back means the app is bound and
/// serving (see [`is_ready_status`] for which statuses count as ready). A
/// connection error / timeout means it hasn't bound yet — reported as not-ready
/// so the caller keeps polling. A 5xx means the app is up but erroring; we treat
/// that as not-ready-yet too (it may still be warming up), and the caller's
/// overall timeout bounds the wait.
async fn probe_http(client: &reqwest::Client, url: &str) -> bool {
    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if is_ready_status(status) {
                true
            } else {
                tracing::debug!(
                    url = %url,
                    status = %status,
                    "Readiness probe got a non-ready HTTP status; container not ready yet"
                );
                false
            }
        }
        Err(e) => {
            tracing::debug!(
                url = %url,
                error = %e,
                "Readiness probe request failed; container not ready yet"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContainerInfo, ContainerStats, DeployRequest, DeployResult, PortMapping, Protocol,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Deployer that returns a queue of canned `ContainerInfo`s, one per
    /// `get_container_info` call (repeating the last once drained). Lets a test
    /// model a container that is `Created` for a poll or two and then `Running`.
    struct ScriptedDeployer {
        infos: Mutex<Vec<ContainerInfo>>,
    }

    impl ScriptedDeployer {
        /// Build a boxed deployer from a script of canned infos. Named `arc`
        /// (not `new`) because it returns a trait object, not `Self`.
        fn arc(infos: Vec<ContainerInfo>) -> Arc<dyn ContainerDeployer> {
            Arc::new(Self {
                infos: Mutex::new(infos),
            })
        }
    }

    /// Build a `ContainerInfo` pointed at a test listener.
    ///
    /// `build_container_url` resolves to `http://{host}:{port}/` where `(host,
    /// port)` is `(container_name, container_port)` in Docker mode and
    /// `("127.0.0.1", host_port)` in baremetal mode. To make these tests
    /// independent of the ambient `DEPLOYMENT_MODE` env var (another test in the
    /// same binary could flip it), we set `container_name = "127.0.0.1"` AND
    /// `container_port == host_port == listener_port`. Both modes then resolve
    /// to `http://127.0.0.1:{listener_port}/`.
    fn info(status: ContainerStatus, ports: Vec<u16>) -> ContainerInfo {
        ContainerInfo {
            container_id: "c1".to_string(),
            container_name: "127.0.0.1".to_string(),
            image_name: "app:latest".to_string(),
            status,
            created_at: chrono::Utc::now(),
            ports: ports
                .into_iter()
                .map(|host_port| PortMapping {
                    host_port,
                    // container_port == host_port so the URL targets the
                    // listener in either deployment mode.
                    container_port: host_port,
                    protocol: Protocol::Tcp,
                })
                .collect(),
            environment_vars: HashMap::new(),
            restart_count: None,
            labels: HashMap::new(),
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: None,
            cpu_limit_cores: None,
        }
    }

    #[async_trait]
    impl ContainerDeployer for ScriptedDeployer {
        async fn deploy_container(
            &self,
            _request: DeployRequest,
        ) -> Result<DeployResult, DeployerError> {
            unimplemented!()
        }
        async fn start_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn stop_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn pause_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn resume_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn remove_container(&self, _id: &str) -> Result<(), DeployerError> {
            unimplemented!()
        }
        async fn get_container_info(&self, _id: &str) -> Result<ContainerInfo, DeployerError> {
            let mut infos = self.infos.lock().unwrap();
            if infos.len() > 1 {
                Ok(infos.remove(0))
            } else {
                Ok(infos
                    .first()
                    .cloned()
                    .unwrap_or_else(|| info(ContainerStatus::Created, vec![])))
            }
        }
        async fn get_container_stats(&self, _id: &str) -> Result<ContainerStats, DeployerError> {
            unimplemented!()
        }
        async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
            unimplemented!()
        }
        async fn get_container_logs(&self, _id: &str) -> Result<String, DeployerError> {
            unimplemented!()
        }
        async fn stream_container_logs(
            &self,
            _id: &str,
        ) -> Result<Box<dyn futures::Stream<Item = String> + Unpin + Send>, DeployerError> {
            unimplemented!()
        }
    }

    fn fast_probe() -> ReadinessProbe {
        ReadinessProbe {
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            request_timeout: Duration::from_millis(300),
        }
    }

    /// Spawn a minimal HTTP/1.1 server on a loopback port that answers every
    /// request with the given status line, then returns the bound port. Used to
    /// prove the probe distinguishes "answers HTTP" from "TCP open but silent".
    async fn spawn_http_server(status_line: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let body = "ok";
                    let resp = format!(
                        "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
    }

    /// Spawn a listener that accepts TCP connections but NEVER sends an HTTP
    /// response (it just holds the socket open). This is the docker-proxy
    /// false-positive: TCP open, app not answering. Returns the bound port.
    async fn spawn_silent_tcp() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                // Hold the connection open without ever writing a response.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(sock);
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn running_no_ports_is_ready() {
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![])]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn running_http_200_is_ready() {
        let port = spawn_http_server("HTTP/1.1 200 OK").await;
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![port])]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn running_http_404_is_ready() {
        // 404 means the path doesn't exist but the server IS up → ready.
        let port = spawn_http_server("HTTP/1.1 404 Not Found").await;
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![port])]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn tcp_open_but_no_http_response_times_out() {
        // THE docker-proxy false-positive: TCP accepts but the app never answers
        // HTTP. A TCP-only probe would wrongly report ready; the HTTP probe must
        // time out instead.
        let port = spawn_silent_tcp().await;
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![port])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(matches!(err, ReadinessError::Timeout { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn closed_port_times_out() {
        // Bind then drop so the port is closed → connect refused forever.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![port])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(matches!(err, ReadinessError::Timeout { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn http_500_is_not_ready() {
        // Server up but erroring → not ready; bounded by the overall timeout.
        let port = spawn_http_server("HTTP/1.1 500 Internal Server Error").await;
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Running, vec![port])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(matches!(err, ReadinessError::Timeout { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn exited_fails_fast() {
        let deployer = ScriptedDeployer::arc(vec![info(ContainerStatus::Exited, vec![])]);
        let err = wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                ReadinessError::Terminal {
                    status: ContainerStatus::Exited,
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn becomes_ready_after_a_few_polls() {
        // Created twice, then Running with an HTTP server.
        let port = spawn_http_server("HTTP/1.1 200 OK").await;
        let deployer = ScriptedDeployer::arc(vec![
            info(ContainerStatus::Created, vec![]),
            info(ContainerStatus::Created, vec![]),
            info(ContainerStatus::Running, vec![port]),
        ]);
        assert!(wait_until_accepting_requests(&deployer, "c1", fast_probe())
            .await
            .is_ok());
    }
}
