// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! On-demand Docker disk usage (`docker system df`) for the control plane.
//!
//! Backs the "Docker disk usage" donut on the server monitoring page. This
//! is deliberately **not** a sampled metric: `GET /system/df` walks every
//! image layer, container rootfs and volume on the host and can take many
//! seconds on a busy machine, so it is fetched only when the operator opens
//! the page or presses refresh, never on the 30-second node-metrics cadence.
//!
//! ## Why this speaks raw HTTP instead of `bollard::Docker::df`
//!
//! The daemon's `/system/df` payload changed shape in Docker API 1.52:
//! engines before that return flat lists (`Images`, `Containers`, `Volumes`,
//! `BuildCache` + `LayersSize`), engines from 1.52 add per-category
//! summaries (`ImageUsage`, `ContainerUsage`, …). The `bollard` release
//! pinned by this workspace models a third, pre-release naming
//! (`ImagesDiskUsage`, …) that matches neither, so its typed `df()` yields
//! `None` for every field on every real daemon. Reading the JSON ourselves
//! and accepting all three spellings is what makes this work on the
//! Docker 24–29 range operators actually run.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;
use utoipa::ToSchema;

/// Upper bound on a single `system df` call. Generous on purpose: a host
/// with hundreds of volumes legitimately takes a while, and the page shows
/// a spinner with a cancel-free retry rather than pretending it is instant.
pub const DOCKER_DF_TIMEOUT: Duration = Duration::from_secs(90);

/// The local daemon socket: the same one every other Docker call in Temps
/// reaches through `bollard::Docker::connect_with_local_defaults`, so this
/// service always measures the daemon that runs the workloads. Deliberately
/// not a runtime setting and not read from the environment (CLAUDE.md:
/// runtime configuration lives in the database; the host's Docker socket is
/// a fixed property of the machine).
const DEFAULT_UNIX_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug, Error)]
pub enum DockerDiskUsageError {
    #[error("Docker disk usage is only available for the control plane (node 0); node {node_id} is a worker and is not supported yet")]
    UnsupportedNode { node_id: i32 },

    #[error("Cannot reach the Docker daemon at {host}: {reason}")]
    Unavailable { host: String, reason: String },

    #[error("Docker daemon at {host} did not answer GET /system/df within {timeout_secs}s")]
    Timeout { host: String, timeout_secs: u64 },

    #[error("Docker daemon at {host} answered GET /system/df with HTTP {status}: {body}")]
    UnexpectedStatus {
        host: String,
        status: u16,
        body: String,
    },

    #[error("Could not parse the Docker daemon's /system/df response from {host}: {reason}")]
    Parse { host: String, reason: String },
}

/// Where the Docker daemon lives. Production always uses the local socket
/// ([`DockerHost::local`]); the HTTP form exists so the parser can be tested
/// against a stub daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerHost {
    /// `unix:///path/to/docker.sock` (or the default socket).
    Unix(PathBuf),
    /// `tcp://host:port` / `http://host:port` — an HTTP base URL without
    /// trailing slash.
    Http(String),
}

impl DockerHost {
    /// The daemon on this machine, the one the workloads run on.
    pub fn local() -> Self {
        Self::Unix(PathBuf::from(DEFAULT_UNIX_SOCKET))
    }

    /// Parse a Docker host URL (`unix://…`, `tcp://…`, `http(s)://…`, or a
    /// bare socket path). Unknown schemes fall back to the local socket so a
    /// typo degrades to "cannot reach daemon" instead of a panic.
    pub fn parse(value: &str) -> Self {
        let value = value.trim();
        if let Some(path) = value.strip_prefix("unix://") {
            return Self::Unix(PathBuf::from(if path.is_empty() {
                DEFAULT_UNIX_SOCKET
            } else {
                path
            }));
        }
        if let Some(rest) = value.strip_prefix("tcp://") {
            return Self::Http(format!("http://{}", rest.trim_end_matches('/')));
        }
        if value.starts_with("http://") || value.starts_with("https://") {
            return Self::Http(value.trim_end_matches('/').to_string());
        }
        // Bare paths (`/var/run/docker.sock`) are accepted by the Docker CLI.
        if value.starts_with('/') {
            return Self::Unix(PathBuf::from(value));
        }
        Self::Unix(PathBuf::from(DEFAULT_UNIX_SOCKET))
    }

    /// Human-readable target for error messages.
    pub fn display(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix://{}", p.display()),
            Self::Http(u) => u.clone(),
        }
    }
}

/// One slice of the Docker disk-usage donut.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DockerDiskUsageCategory {
    /// Number of objects in this category (all images, all containers, …).
    pub total_count: i64,
    /// Objects currently in use (images referenced by a container, running
    /// containers, mounted volumes, in-use cache records).
    pub active_count: i64,
    /// Bytes on disk attributed to this category.
    pub size_bytes: i64,
    /// Bytes `docker system prune` could free from this category. `null`
    /// when the daemon is older than API 1.52 and does not report it.
    pub reclaimable_bytes: Option<i64>,
}

/// Result of `docker system df` for the control-plane host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DockerDiskUsage {
    pub images: DockerDiskUsageCategory,
    pub containers: DockerDiskUsageCategory,
    pub volumes: DockerDiskUsageCategory,
    pub build_cache: DockerDiskUsageCategory,
    /// Sum of the four category sizes.
    pub total_bytes: i64,
    /// When this snapshot was taken (ISO 8601, UTC).
    pub collected_at: String,
    /// Docker API version the daemon answered with, when it reported one
    /// (`Api-Version` header). Useful when a category shows `null`
    /// reclaimable bytes.
    pub api_version: Option<String>,
}

// ── Wire shapes ───────────────────────────────────────────────────────────────
//
// Every field is optional so a partial or future-shaped payload still parses;
// `derive_totals` decides what it can compute from what actually arrived.

#[derive(Debug, Default, Deserialize)]
struct RawUsageSummary {
    #[serde(rename = "TotalCount")]
    total_count: Option<i64>,
    #[serde(rename = "ActiveCount")]
    active_count: Option<i64>,
    #[serde(rename = "TotalSize")]
    total_size: Option<i64>,
    #[serde(rename = "Reclaimable")]
    reclaimable: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawImage {
    #[serde(rename = "Size")]
    size: Option<i64>,
    #[serde(rename = "Containers")]
    containers: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawContainer {
    #[serde(rename = "SizeRw")]
    size_rw: Option<i64>,
    #[serde(rename = "State")]
    state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVolumeUsage {
    #[serde(rename = "Size")]
    size: Option<i64>,
    #[serde(rename = "RefCount")]
    ref_count: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVolume {
    #[serde(rename = "UsageData")]
    usage_data: Option<RawVolumeUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBuildCache {
    #[serde(rename = "Size")]
    size: Option<i64>,
    #[serde(rename = "InUse")]
    in_use: Option<bool>,
    #[serde(rename = "Shared")]
    shared: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSystemDf {
    // API ≥ 1.52 summaries (current daemons).
    #[serde(rename = "ImageUsage")]
    image_usage: Option<RawUsageSummary>,
    #[serde(rename = "ContainerUsage")]
    container_usage: Option<RawUsageSummary>,
    #[serde(rename = "VolumeUsage")]
    volume_usage: Option<RawUsageSummary>,
    #[serde(rename = "BuildCacheUsage")]
    build_cache_usage: Option<RawUsageSummary>,

    // Pre-release spelling shipped by some API 1.52 stubs; accepted so a
    // daemon built against that draft still works.
    #[serde(rename = "ImagesDiskUsage")]
    images_disk_usage: Option<RawUsageSummary>,
    #[serde(rename = "ContainersDiskUsage")]
    containers_disk_usage: Option<RawUsageSummary>,
    #[serde(rename = "VolumesDiskUsage")]
    volumes_disk_usage: Option<RawUsageSummary>,
    #[serde(rename = "BuildCacheDiskUsage")]
    build_cache_disk_usage: Option<RawUsageSummary>,

    // Legacy lists (API < 1.52, still emitted by newer daemons).
    #[serde(rename = "LayersSize")]
    layers_size: Option<i64>,
    #[serde(rename = "Images")]
    images: Option<Vec<RawImage>>,
    #[serde(rename = "Containers")]
    containers: Option<Vec<RawContainer>>,
    #[serde(rename = "Volumes")]
    volumes: Option<Vec<RawVolume>>,
    #[serde(rename = "BuildCache")]
    build_cache: Option<Vec<RawBuildCache>>,
}

fn category_from_summary(s: &RawUsageSummary) -> DockerDiskUsageCategory {
    DockerDiskUsageCategory {
        total_count: s.total_count.unwrap_or(0),
        active_count: s.active_count.unwrap_or(0),
        size_bytes: s.total_size.unwrap_or(0).max(0),
        reclaimable_bytes: s.reclaimable.map(|r| r.max(0)),
    }
}

/// Turn whatever the daemon sent into the four categories. Summaries win
/// when present; otherwise totals are derived from the legacy lists the
/// same way the Docker CLI computes `docker system df`:
///
/// - images: `LayersSize` (unique layer bytes — summing `Size` per image
///   would double-count shared layers)
/// - containers: Σ `SizeRw` (writable layer; the image is counted above)
/// - volumes: Σ `UsageData.Size`
/// - build cache: Σ `Size`
fn derive_totals(raw: &RawSystemDf) -> Result<[DockerDiskUsageCategory; 4], String> {
    let images = match raw.image_usage.as_ref().or(raw.images_disk_usage.as_ref()) {
        Some(s) => category_from_summary(s),
        None => {
            let list = raw
                .images
                .as_ref()
                .ok_or("payload has neither ImageUsage nor Images")?;
            DockerDiskUsageCategory {
                total_count: list.len() as i64,
                active_count: list
                    .iter()
                    .filter(|i| i.containers.unwrap_or(0) > 0)
                    .count() as i64,
                size_bytes: raw
                    .layers_size
                    .unwrap_or_else(|| list.iter().map(|i| i.size.unwrap_or(0)).sum())
                    .max(0),
                reclaimable_bytes: None,
            }
        }
    };

    let containers = match raw
        .container_usage
        .as_ref()
        .or(raw.containers_disk_usage.as_ref())
    {
        Some(s) => category_from_summary(s),
        None => {
            let list = raw
                .containers
                .as_ref()
                .ok_or("payload has neither ContainerUsage nor Containers")?;
            DockerDiskUsageCategory {
                total_count: list.len() as i64,
                active_count: list
                    .iter()
                    .filter(|c| c.state.as_deref() == Some("running"))
                    .count() as i64,
                size_bytes: list
                    .iter()
                    .map(|c| c.size_rw.unwrap_or(0))
                    .sum::<i64>()
                    .max(0),
                reclaimable_bytes: None,
            }
        }
    };

    let volumes = match raw
        .volume_usage
        .as_ref()
        .or(raw.volumes_disk_usage.as_ref())
    {
        Some(s) => category_from_summary(s),
        None => {
            let list = raw
                .volumes
                .as_ref()
                .ok_or("payload has neither VolumeUsage nor Volumes")?;
            DockerDiskUsageCategory {
                total_count: list.len() as i64,
                active_count: list
                    .iter()
                    .filter(|v| v.usage_data.as_ref().and_then(|u| u.ref_count).unwrap_or(0) > 0)
                    .count() as i64,
                size_bytes: list
                    .iter()
                    .map(|v| v.usage_data.as_ref().and_then(|u| u.size).unwrap_or(0))
                    .sum::<i64>()
                    .max(0),
                reclaimable_bytes: None,
            }
        }
    };

    let build_cache = match raw
        .build_cache_usage
        .as_ref()
        .or(raw.build_cache_disk_usage.as_ref())
    {
        Some(s) => category_from_summary(s),
        None => {
            // Build cache is absent on daemons without BuildKit; treat as an
            // empty category rather than a parse failure.
            let list = raw.build_cache.as_deref().unwrap_or(&[]);
            DockerDiskUsageCategory {
                total_count: list.len() as i64,
                active_count: list
                    .iter()
                    .filter(|b| b.in_use.unwrap_or(false) && !b.shared.unwrap_or(false))
                    .count() as i64,
                size_bytes: list.iter().map(|b| b.size.unwrap_or(0)).sum::<i64>().max(0),
                reclaimable_bytes: None,
            }
        }
    };

    Ok([images, containers, volumes, build_cache])
}

/// Parse a raw `/system/df` body into the API response.
fn parse_system_df(body: &[u8], api_version: Option<String>) -> Result<DockerDiskUsage, String> {
    let raw: RawSystemDf =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let [images, containers, volumes, build_cache] = derive_totals(&raw)?;
    let total_bytes = images
        .size_bytes
        .saturating_add(containers.size_bytes)
        .saturating_add(volumes.size_bytes)
        .saturating_add(build_cache.size_bytes);
    Ok(DockerDiskUsage {
        images,
        containers,
        volumes,
        build_cache,
        total_bytes,
        collected_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        api_version,
    })
}

/// Fetches `docker system df` for the local daemon.
pub struct DockerDiskUsageService {
    host: DockerHost,
    timeout: Duration,
}

impl DockerDiskUsageService {
    pub fn new(host: DockerHost) -> Self {
        Self {
            host,
            timeout: DOCKER_DF_TIMEOUT,
        }
    }

    /// The service for this machine's daemon.
    pub fn local() -> Self {
        Self::new(DockerHost::local())
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn host(&self) -> &DockerHost {
        &self.host
    }

    /// Run `GET /system/df` against the daemon and aggregate it.
    pub async fn fetch(&self, node_id: i32) -> Result<DockerDiskUsage, DockerDiskUsageError> {
        if node_id != crate::services::CONTROL_PLANE_NODE_ID {
            return Err(DockerDiskUsageError::UnsupportedNode { node_id });
        }
        let host = self.host.display();
        debug!(host = %host, "fetching docker system df");

        let started = std::time::Instant::now();
        let fetched = tokio::time::timeout(self.timeout, self.raw_get_system_df())
            .await
            .map_err(|_| DockerDiskUsageError::Timeout {
                host: host.clone(),
                timeout_secs: self.timeout.as_secs(),
            })??;

        let (status, api_version, body) = fetched;
        if status != 200 {
            let excerpt = String::from_utf8_lossy(&body[..body.len().min(300)]).to_string();
            return Err(DockerDiskUsageError::UnexpectedStatus {
                host,
                status,
                body: excerpt,
            });
        }

        let usage =
            parse_system_df(&body, api_version).map_err(|reason| DockerDiskUsageError::Parse {
                host: host.clone(),
                reason,
            })?;
        debug!(
            host = %host,
            elapsed_ms = started.elapsed().as_millis() as u64,
            total_bytes = usage.total_bytes,
            "docker system df collected"
        );
        Ok(usage)
    }

    /// Returns `(status, Api-Version header, body)`.
    async fn raw_get_system_df(
        &self,
    ) -> Result<(u16, Option<String>, Vec<u8>), DockerDiskUsageError> {
        match &self.host {
            DockerHost::Unix(path) => self.get_over_unix(path).await,
            DockerHost::Http(base) => self.get_over_http(base).await,
        }
    }

    async fn get_over_unix(
        &self,
        path: &std::path::Path,
    ) -> Result<(u16, Option<String>, Vec<u8>), DockerDiskUsageError> {
        use http_body_util::{BodyExt, Empty};
        use hyper_util::rt::TokioIo;

        let host = self.host.display();
        let stream = tokio::net::UnixStream::connect(path).await.map_err(|e| {
            DockerDiskUsageError::Unavailable {
                host: host.clone(),
                reason: e.to_string(),
            }
        })?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host: host.clone(),
                reason: format!("HTTP handshake failed: {e}"),
            })?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                // Expected for short-lived HTTP/1.1 over a Unix socket that
                // closes without a clean shutdown.
                debug!("docker df connection closed: {e}");
            }
        });

        // Unversioned path: the daemon answers with its newest shape, which
        // `parse_system_df` accepts in every known spelling.
        let request = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri("/system/df")
            .header(hyper::header::HOST, "docker")
            .body(Empty::<bytes::Bytes>::new())
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host: host.clone(),
                reason: format!("could not build request: {e}"),
            })?;

        let response =
            sender
                .send_request(request)
                .await
                .map_err(|e| DockerDiskUsageError::Unavailable {
                    host: host.clone(),
                    reason: format!("request failed: {e}"),
                })?;
        let status = response.status().as_u16();
        let api_version = response
            .headers()
            .get("api-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host,
                reason: format!("could not read response body: {e}"),
            })?
            .to_bytes()
            .to_vec();
        Ok((status, api_version, body))
    }

    async fn get_over_http(
        &self,
        base: &str,
    ) -> Result<(u16, Option<String>, Vec<u8>), DockerDiskUsageError> {
        let host = self.host.display();
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host: host.clone(),
                reason: format!("could not build HTTP client: {e}"),
            })?;
        let response = client
            .get(format!("{base}/system/df"))
            .send()
            .await
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host: host.clone(),
                reason: e.to_string(),
            })?;
        let status = response.status().as_u16();
        let api_version = response
            .headers()
            .get("api-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes()
            .await
            .map_err(|e| DockerDiskUsageError::Unavailable {
                host,
                reason: format!("could not read response body: {e}"),
            })?
            .to_vec();
        Ok((status, api_version, body))
    }
}

impl Default for DockerDiskUsageService {
    fn default() -> Self {
        Self::local()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW_SHAPE: &str = r#"{
        "LayersSize": 1000,
        "Images": [{"Size": 700, "SharedSize": 100, "Containers": 1}, {"Size": 400, "SharedSize": 100, "Containers": 0}],
        "Containers": [{"SizeRw": 50, "State": "running"}, {"SizeRw": 25, "State": "exited"}],
        "Volumes": [{"UsageData": {"Size": 300, "RefCount": 1}}],
        "BuildCache": [{"Size": 10, "InUse": true, "Shared": false}],
        "ImageUsage": {"ActiveCount": 1, "TotalCount": 2, "TotalSize": 1000, "Reclaimable": 400},
        "ContainerUsage": {"ActiveCount": 1, "TotalCount": 2, "TotalSize": 75, "Reclaimable": 25},
        "VolumeUsage": {"ActiveCount": 1, "TotalCount": 1, "TotalSize": 300, "Reclaimable": 0},
        "BuildCacheUsage": {"ActiveCount": 1, "TotalCount": 1, "TotalSize": 10, "Reclaimable": 0}
    }"#;

    const LEGACY_SHAPE: &str = r#"{
        "LayersSize": 1000,
        "Images": [{"Size": 700, "SharedSize": 100, "Containers": 1}, {"Size": 400, "SharedSize": 100, "Containers": 0}],
        "Containers": [{"SizeRw": 50, "State": "running"}, {"SizeRw": 25, "State": "exited"}],
        "Volumes": [{"UsageData": {"Size": 300, "RefCount": 1}}, {"UsageData": {"Size": 5, "RefCount": 0}}],
        "BuildCache": [{"Size": 10, "InUse": true, "Shared": false}, {"Size": 7, "InUse": false, "Shared": true}]
    }"#;

    #[test]
    fn parses_api_152_summaries_and_prefers_them_over_lists() {
        let usage = parse_system_df(NEW_SHAPE.as_bytes(), Some("1.52".into())).unwrap();
        assert_eq!(usage.images.size_bytes, 1000);
        assert_eq!(usage.images.reclaimable_bytes, Some(400));
        assert_eq!(usage.images.active_count, 1);
        assert_eq!(usage.containers.size_bytes, 75);
        assert_eq!(usage.volumes.size_bytes, 300);
        assert_eq!(usage.build_cache.size_bytes, 10);
        assert_eq!(usage.total_bytes, 1000 + 75 + 300 + 10);
        assert_eq!(usage.api_version.as_deref(), Some("1.52"));
        assert!(usage.collected_at.ends_with('Z'));
    }

    #[test]
    fn derives_totals_from_legacy_lists_like_docker_cli() {
        let usage = parse_system_df(LEGACY_SHAPE.as_bytes(), None).unwrap();
        // Images use LayersSize, not Σ Size (which would double-count shared layers).
        assert_eq!(usage.images.size_bytes, 1000);
        assert_eq!(usage.images.total_count, 2);
        assert_eq!(usage.images.active_count, 1);
        assert_eq!(usage.images.reclaimable_bytes, None);
        assert_eq!(usage.containers.size_bytes, 75);
        assert_eq!(usage.containers.active_count, 1);
        assert_eq!(usage.volumes.size_bytes, 305);
        assert_eq!(usage.volumes.active_count, 1);
        assert_eq!(usage.build_cache.size_bytes, 17);
        assert_eq!(usage.build_cache.active_count, 1);
        assert_eq!(usage.total_bytes, 1000 + 75 + 305 + 17);
    }

    #[test]
    fn accepts_prerelease_disk_usage_spelling() {
        let body = r#"{
            "ImagesDiskUsage": {"TotalCount": 3, "ActiveCount": 2, "TotalSize": 9, "Reclaimable": 1},
            "ContainersDiskUsage": {"TotalCount": 1, "ActiveCount": 1, "TotalSize": 2, "Reclaimable": 0},
            "VolumesDiskUsage": {"TotalCount": 0, "ActiveCount": 0, "TotalSize": 0, "Reclaimable": 0},
            "BuildCacheDiskUsage": {"TotalCount": 0, "ActiveCount": 0, "TotalSize": 0, "Reclaimable": 0}
        }"#;
        let usage = parse_system_df(body.as_bytes(), None).unwrap();
        assert_eq!(usage.images.size_bytes, 9);
        assert_eq!(usage.total_bytes, 11);
    }

    #[test]
    fn missing_build_cache_is_an_empty_category_not_an_error() {
        let body = r#"{"LayersSize": 1, "Images": [], "Containers": [], "Volumes": []}"#;
        let usage = parse_system_df(body.as_bytes(), None).unwrap();
        assert_eq!(usage.build_cache, DockerDiskUsageCategory::default());
        assert_eq!(usage.total_bytes, 1);
    }

    #[test]
    fn unrecognised_payload_is_a_parse_error() {
        let err = parse_system_df(br#"{"Foo": 1}"#, None).unwrap_err();
        assert!(err.contains("ImageUsage"), "{err}");
        let err = parse_system_df(b"not json", None).unwrap_err();
        assert!(err.contains("invalid JSON"), "{err}");
    }

    #[test]
    fn docker_host_parsing() {
        assert_eq!(
            DockerHost::parse("unix:///tmp/d.sock"),
            DockerHost::Unix(PathBuf::from("/tmp/d.sock"))
        );
        assert_eq!(
            DockerHost::parse("tcp://10.0.0.5:2375/"),
            DockerHost::Http("http://10.0.0.5:2375".into())
        );
        assert_eq!(
            DockerHost::parse("http://localhost:2375"),
            DockerHost::Http("http://localhost:2375".into())
        );
        assert_eq!(
            DockerHost::parse("/run/user/1000/docker.sock"),
            DockerHost::Unix(PathBuf::from("/run/user/1000/docker.sock"))
        );
        assert_eq!(
            DockerHost::parse("ssh://user@host"),
            DockerHost::Unix(PathBuf::from(DEFAULT_UNIX_SOCKET))
        );
    }

    #[tokio::test]
    async fn worker_nodes_are_rejected_before_touching_docker() {
        let svc = DockerDiskUsageService::new(DockerHost::Unix(PathBuf::from("/nonexistent.sock")));
        let err = svc.fetch(7).await.unwrap_err();
        assert!(matches!(
            err,
            DockerDiskUsageError::UnsupportedNode { node_id: 7 }
        ));
    }

    #[tokio::test]
    async fn unreachable_socket_is_unavailable_with_host_in_message() {
        let svc = DockerDiskUsageService::new(DockerHost::Unix(PathBuf::from("/nonexistent.sock")))
            .with_timeout(Duration::from_secs(2));
        let err = svc.fetch(0).await.unwrap_err();
        match err {
            DockerDiskUsageError::Unavailable { host, .. } => {
                assert_eq!(host, "unix:///nonexistent.sock")
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// Runs only when a Docker daemon is reachable; skips gracefully otherwise
    /// (CLAUDE.md: Docker tests must not be `#[ignore]`d).
    #[tokio::test]
    async fn real_daemon_reports_a_consistent_total() {
        let svc = DockerDiskUsageService::local();
        let usage = match svc.fetch(0).await {
            Ok(u) => u,
            Err(DockerDiskUsageError::Unavailable { .. }) => {
                eprintln!("skipping: no Docker daemon reachable");
                return;
            }
            Err(e) => panic!("unexpected error: {e}"),
        };
        assert_eq!(
            usage.total_bytes,
            usage.images.size_bytes
                + usage.containers.size_bytes
                + usage.volumes.size_bytes
                + usage.build_cache.size_bytes
        );
        assert!(usage.images.total_count >= usage.images.active_count);
    }
}
