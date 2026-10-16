// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backend-independent S3 mirroring for managed S3 services.
//!
//! Backup, restore and backend-to-backend migration are all "copy buckets
//! between two S3 endpoints", which is exactly what the RustFS client (`rc`)
//! does against any S3-compatible server. This module runs `rc` in a
//! short-lived helper container attached to the Temps network, so a managed
//! service is addressed by its container name (the same way apps reach it) and
//! the helper works the same on Linux and on Docker running in a VM.
//!
//! Credentials reach the helper only through `RC_HOST_<alias>` environment
//! variables (never `rc alias set`, which would put keys in argv); every piece
//! of client output is redacted before it can reach an error.

use bollard::Docker;
use std::sync::Arc;
use tracing::{info, warn};

use super::managed_s3::{
    ManagedS3Backend, ManagedS3Endpoint, ManagedS3Error, ManagedS3MirrorReport, MigrationSource,
    S3Location,
};
use super::rc_client::{self, TokenSide};
use super::{SensitiveValues, ServiceConfig};

/// A running `rc` helper container. Call [`RcSession::finish`] when the
/// operation ends: the container carries credentials and must not outlive it.
/// A session dropped without `finish` (the caller's future was cancelled
/// mid-copy, e.g. a client disconnect) removes its container in the
/// background instead of leaving it running.
pub(crate) struct RcSession {
    docker: Arc<Docker>,
    container_id: String,
    service: String,
    released: bool,
}

/// Force-remove a helper container; a container that is already gone is fine.
async fn remove_helper_container(docker: &Docker, container_id: &str, service: &str) {
    if let Err(e) = docker
        .remove_container(
            container_id,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        if !matches!(
            e,
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }
        ) {
            warn!(
                service = %service,
                container = %container_id,
                "Failed to remove credential-bearing rc helper container: {}",
                e
            );
        }
    }
}

impl Drop for RcSession {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let docker = self.docker.clone();
        let container_id = std::mem::take(&mut self.container_id);
        let service = std::mem::take(&mut self.service);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                warn!(
                    service = %service,
                    container = %container_id,
                    "rc helper session dropped without finish(); removing its container in the background"
                );
                runtime.spawn(async move {
                    remove_helper_container(&docker, &container_id, &service).await;
                });
            }
            Err(_) => {
                warn!(
                    service = %service,
                    container = %container_id,
                    "rc helper session dropped outside a runtime; its container must be removed manually"
                );
            }
        }
    }
}

impl RcSession {
    /// Pull the `rc` image and start a helper with `env` (the `RC_HOST_*`
    /// aliases and, for a temporary credential, the session-token variable).
    pub(crate) async fn start(
        docker: Arc<Docker>,
        service: &str,
        env: Vec<String>,
    ) -> Result<Self, ManagedS3Error> {
        Self::start_with_labels(docker, service, env, std::collections::HashMap::new()).await
    }

    /// Like [`Self::start`], with `extra_labels` added to the helper. A
    /// restore helper carries the restore labels of the container it writes
    /// into, so the restore fence finds one that outlived a crash.
    pub(crate) async fn start_with_labels(
        docker: Arc<Docker>,
        service: &str,
        env: Vec<String>,
        extra_labels: std::collections::HashMap<String, String>,
    ) -> Result<Self, ManagedS3Error> {
        let mirror_error = |operation: &str, reason: String| ManagedS3Error::Mirror {
            service: service.to_string(),
            operation: operation.to_string(),
            reason,
        };

        crate::utils::pull_image_with_retry(&docker, rc_client::RC_IMAGE, None)
            .await
            .map_err(|reason| {
                mirror_error(
                    "pulling the rc helper image",
                    format!("{}: {reason}", rc_client::RC_IMAGE),
                )
            })?;
        crate::utils::ensure_network_exists(&docker)
            .await
            .map_err(|e| mirror_error("ensuring the Temps network exists", e.to_string()))?;

        let name = format!("rc-s3-{}", uuid::Uuid::new_v4());
        let label_key = format!("{}managed_s3_helper", temps_core::DOCKER_LABEL_PREFIX);
        let mut labels = extra_labels;
        labels.insert(label_key, service.to_string());
        let config = bollard::models::ContainerCreateBody {
            image: Some(rc_client::RC_IMAGE.to_string()),
            env: Some(env),
            // A `sh` entrypoint plus a tty keeps the container alive; the
            // operations below run through `docker exec`.
            entrypoint: Some(vec!["sh".to_string()]),
            tty: Some(true),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            labels: Some(labels),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some(temps_core::NETWORK_NAME.to_string()),
                // Linux Docker has no `host.docker.internal` unless asked:
                // a backup location on the host's loopback is reached
                // through it (see `host_gateway_endpoint`).
                extra_hosts: Some(vec!["host.docker.internal:host-gateway".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let container = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                config,
            )
            .await
            .map_err(|e| {
                mirror_error("creating the rc helper container", format!("{name}: {e}"))
            })?;
        let session = Self {
            docker: docker.clone(),
            container_id: container.id.clone(),
            service: service.to_string(),
            released: false,
        };
        if let Err(e) = docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
        {
            session.finish().await;
            return Err(mirror_error(
                "starting the rc helper container",
                format!("{name}: {e}"),
            ));
        }
        Ok(session)
    }

    pub(crate) fn docker(&self) -> &Docker {
        &self.docker
    }

    pub(crate) fn container_id(&self) -> &str {
        &self.container_id
    }

    /// Run `cmd` and return its output; a non-zero exit is an error whose
    /// text has been through `sensitive`.
    pub(crate) async fn run(
        &self,
        operation: &str,
        cmd: &[&str],
        sensitive: &SensitiveValues<'_>,
    ) -> Result<rc_client::ExecOutput, ManagedS3Error> {
        let output = rc_client::exec_capture(&self.docker, &self.container_id, cmd)
            .await
            .map_err(|e| ManagedS3Error::Mirror {
                service: self.service.clone(),
                operation: operation.to_string(),
                reason: sensitive.redact(&e.to_string()),
            })?;
        if output.exit_code != 0 {
            return Err(ManagedS3Error::Mirror {
                service: self.service.clone(),
                operation: operation.to_string(),
                reason: format!(
                    "exit code {}: {}",
                    output.exit_code,
                    sensitive.redact(
                        format!("{}\n{}", output.stderr.trim(), output.stdout.trim()).trim()
                    )
                ),
            });
        }
        Ok(output)
    }

    /// Like [`Self::run`], but a non-zero exit whose output contains one of
    /// `tolerated` (lower-case) counts as success. Used for removals, where
    /// "already gone" is the state the caller wanted.
    pub(crate) async fn run_tolerant(
        &self,
        operation: &str,
        cmd: &[&str],
        sensitive: &SensitiveValues<'_>,
        tolerated: &[&str],
    ) -> Result<(), ManagedS3Error> {
        let output = rc_client::exec_capture(&self.docker, &self.container_id, cmd)
            .await
            .map_err(|e| ManagedS3Error::Mirror {
                service: self.service.clone(),
                operation: operation.to_string(),
                reason: sensitive.redact(&e.to_string()),
            })?;
        if output.exit_code == 0 {
            return Ok(());
        }
        let text = format!("{}\n{}", output.stderr.trim(), output.stdout.trim()).to_lowercase();
        if tolerated.iter().any(|marker| text.contains(marker)) {
            return Ok(());
        }
        Err(ManagedS3Error::Mirror {
            service: self.service.clone(),
            operation: operation.to_string(),
            reason: format!(
                "exit code {}: {}",
                output.exit_code,
                sensitive.redact(text.trim())
            ),
        })
    }

    /// Remove the helper. Failure is logged, not returned: the operation's own
    /// result is what the caller needs, and a leftover helper is labelled so
    /// it can be found.
    pub(crate) async fn finish(mut self) {
        self.released = true;
        remove_helper_container(&self.docker, &self.container_id, &self.service).await;
    }
}

fn mirror_failure(service: &str, operation: &str, error: impl std::fmt::Display) -> ManagedS3Error {
    ManagedS3Error::Mirror {
        service: service.to_string(),
        operation: operation.to_string(),
        reason: error.to_string(),
    }
}

/// `RC_HOST_*` aliases for a managed endpoint using its Docker-network URL.
fn service_alias(alias: &str, endpoint: &ManagedS3Endpoint, access: &str, secret: &str) -> String {
    rc_client::rc_host_env(alias, &endpoint.internal_url(), access, secret)
}

fn location_endpoint(location: &S3Location) -> String {
    location
        .endpoint
        .clone()
        .unwrap_or_else(|| "https://s3.amazonaws.com".to_string())
}

/// Port of a loopback endpoint (`localhost` / `127.0.0.1`), which names the
/// Temps host. Inside the `rc` helper container the same URL would point at
/// the helper itself, so it has to be translated first.
fn loopback_endpoint_port(endpoint: &str) -> Option<u16> {
    let rest = endpoint.split("://").nth(1).unwrap_or(endpoint);
    let (host, tail) = rest.split_once(':')?;
    if host != "localhost" && host != "127.0.0.1" {
        return None;
    }
    tail.split('/').next()?.parse().ok()
}

/// `endpoint` with its loopback host replaced by the host gateway name
/// Docker resolves for containers.
fn host_gateway_endpoint(endpoint: &str) -> String {
    endpoint
        .replace("://localhost", "://host.docker.internal")
        .replace("://127.0.0.1", "://host.docker.internal")
}

/// The location's endpoint as the helper container can reach it. A loopback
/// endpoint that is really another container's published port (a RustFS or
/// Garage service used as a backup destination) becomes that container's
/// Docker-network address; any other loopback endpoint goes through the host
/// gateway; everything else is used unchanged.
async fn helper_reachable_endpoint(
    docker: &Docker,
    service: &str,
    location: &S3Location,
) -> Result<String, ManagedS3Error> {
    let endpoint = location_endpoint(location);
    let Some(port) = loopback_endpoint_port(&endpoint) else {
        return Ok(endpoint);
    };
    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: false,
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    for container in &containers {
        let on_temps_network = container
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .is_some_and(|networks| networks.contains_key(temps_core::NETWORK_NAME.as_str()));
        let internal_port = container
            .ports
            .as_ref()
            .and_then(|ports| ports.iter().find(|p| p.public_port == Some(port)))
            .map(|p| p.private_port);
        let name = container
            .names
            .as_ref()
            .and_then(|names| names.iter().find(|n| n.matches('/').count() == 1))
            .map(|n| n.trim_start_matches('/'));
        if let (true, Some(internal_port), Some(name)) = (on_temps_network, internal_port, name) {
            let scheme = if endpoint.starts_with("https") {
                "https"
            } else {
                "http"
            };
            return Ok(format!("{scheme}://{name}:{internal_port}"));
        }
    }
    ensure_host_gateway_reaches(docker, service, port).await?;
    Ok(host_gateway_endpoint(&endpoint))
}

/// On Linux the host gateway is the Docker network's bridge address, which a
/// listener bound only to the host's loopback interface never answers. Check
/// that the port accepts connections there before starting a helper that
/// could only time out, and say what to change instead. Other platforms
/// forward `host.docker.internal` to the host's loopback, so nothing is
/// checked there.
async fn ensure_host_gateway_reaches(
    docker: &Docker,
    service: &str,
    port: u16,
) -> Result<(), ManagedS3Error> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let gateway = docker
        .inspect_network(
            temps_core::NETWORK_NAME.as_str(),
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await
        .ok()
        .and_then(|network| network.ipam)
        .and_then(|ipam| ipam.config)
        .and_then(|configs| configs.into_iter().find_map(|config| config.gateway));
    let Some(gateway) = gateway else {
        return Ok(());
    };
    if tcp_port_accepts(&format!("{gateway}:{port}")).await {
        return Ok(());
    }
    Err(mirror_failure(
        service,
        "reaching the backup location",
        format!(
            "the endpoint on port {port} is reachable only from the Temps host's loopback \
             interface, which Docker containers cannot use; make the storage server listen on \
             an address the Docker network can reach (for example 0.0.0.0:{port}), or use its \
             Docker network address or a public hostname as the endpoint"
        ),
    ))
}

/// Whether a TCP connection to `address` succeeds within a few seconds.
async fn tcp_port_accepts(address: &str) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::TcpStream::connect(address),
        )
        .await,
        Ok(Ok(_))
    )
}

fn location_path(alias: &str, location: &S3Location) -> String {
    let prefix = location.prefix.trim_matches('/');
    if prefix.is_empty() {
        format!("{alias}/{}", location.bucket)
    } else {
        format!("{alias}/{}/{prefix}", location.bucket)
    }
}

/// Mirror every bucket of a managed service into
/// `<destination bucket>/<prefix>/<bucket>/`, the layout every restore lists.
pub(crate) async fn backup_buckets(
    docker: Arc<Docker>,
    service: &str,
    endpoint: &ManagedS3Endpoint,
    admin_access_key: &str,
    admin_secret_key: &str,
    destination: &S3Location,
) -> Result<ManagedS3MirrorReport, ManagedS3Error> {
    let has_token = destination
        .session_token
        .as_deref()
        .is_some_and(|t| !t.is_empty());
    let mut env = vec![
        service_alias("svc", endpoint, admin_access_key, admin_secret_key),
        rc_client::rc_host_env(
            "dst",
            &helper_reachable_endpoint(&docker, service, destination).await?,
            &destination.access_key,
            &destination.secret_key,
        ),
    ];
    env.extend(rc_client::remote_session_token_env(
        destination.session_token.as_deref(),
    ));
    let sensitive = SensitiveValues::new()
        .credential(admin_access_key, admin_secret_key, None)
        .credential(
            &destination.access_key,
            &destination.secret_key,
            destination.session_token.as_deref(),
        );

    let session = RcSession::start(docker, service, env).await?;
    let result = rc_client::mirror_all_buckets(
        session.docker(),
        session.container_id(),
        "svc",
        &location_path("dst", destination),
        has_token,
        &sensitive,
    )
    .await
    .map_err(|e| mirror_failure(service, "backing up buckets", e));
    session.finish().await;
    let buckets = result?;
    info!(service, buckets = ?buckets, "Backed up managed S3 buckets");
    Ok(ManagedS3MirrorReport { buckets })
}

/// Mirror the buckets stored under `source` into `backend`, creating each
/// bucket first. See [`ManagedS3Backend::restore`] for `exact`.
pub(crate) async fn restore_buckets<B: ManagedS3Backend + ?Sized>(
    docker: Arc<Docker>,
    backend: &B,
    config: &ServiceConfig,
    source: &S3Location,
    exact: bool,
) -> Result<ManagedS3MirrorReport, ManagedS3Error> {
    let service = backend.service_name();
    let endpoint = backend.endpoint(config)?;
    let (admin_access_key, admin_secret_key) = backend.admin_credentials(config)?;
    let has_token = source
        .session_token
        .as_deref()
        .is_some_and(|t| !t.is_empty());
    let mut env = vec![
        service_alias("svc", &endpoint, &admin_access_key, &admin_secret_key),
        rc_client::rc_host_env(
            "src",
            &helper_reachable_endpoint(&docker, &service, source).await?,
            &source.access_key,
            &source.secret_key,
        ),
    ];
    env.extend(rc_client::remote_session_token_env(
        source.session_token.as_deref(),
    ));
    let sensitive = SensitiveValues::new()
        .credential(&admin_access_key, &admin_secret_key, None)
        .credential(
            &source.access_key,
            &source.secret_key,
            source.session_token.as_deref(),
        );

    let session = RcSession::start_with_labels(
        docker,
        &service,
        env,
        crate::externalsvc::restore_helper::restore_helper_labels(&backend.container_name()),
    )
    .await?;
    let result = async {
        // The trailing slash lists the folder's contents rather than every key
        // sharing the prefix.
        let root = format!("{}/", location_path("src", source));
        let buckets = rc_client::list_dir_names(
            session.docker(),
            session.container_id(),
            &root,
            has_token,
            &sensitive,
        )
        .await
        .map_err(|e| mirror_failure(&service, "listing the backup", e))?;

        for bucket in &buckets {
            backend.create_bucket(config, bucket).await?;
            let mirror = rc_client::mirror_command(
                &format!("{root}{bucket}/"),
                &format!("svc/{bucket}/"),
                exact,
                TokenSide::when(has_token, TokenSide::Source),
            );
            // Every restore form (token-staged, exact, plain) can hit the
            // target's refusal of zero-byte uploads, so all get the fallback.
            let fallback = Some(EmptyObjectCopy {
                source_label: format!("backup location '{}'", location_path("", source)),
                from: location_sdk_client(source).await,
                from_bucket: source.bucket.clone(),
                from_prefix: format!(
                    "{}/",
                    [source.prefix.trim_matches('/'), bucket.as_str()]
                        .iter()
                        .filter(|part| !part.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                to: sdk_client(&endpoint, &admin_access_key, &admin_secret_key),
                to_bucket: bucket.clone(),
            });
            run_mirror_with_empty_object_fallback(
                &session,
                &service,
                &format!("restoring bucket '{bucket}'"),
                &sensitive,
                &mirror,
                fallback,
            )
            .await?;
        }
        Ok::<_, ManagedS3Error>(buckets)
    }
    .await;
    session.finish().await;
    let buckets = result?;
    info!(service = %service, buckets = ?buckets, exact, "Restored managed S3 buckets");
    Ok(ManagedS3MirrorReport { buckets })
}

fn sdk_client(
    endpoint: &ManagedS3Endpoint,
    access_key: &str,
    secret_key: &str,
) -> aws_sdk_s3::Client {
    let credentials =
        aws_sdk_s3::config::Credentials::new(access_key, secret_key, None, None, "temps-migrate");
    aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .region(aws_sdk_s3::config::Region::new(endpoint.region.clone()))
            .endpoint_url(format!("http://{}:{}", endpoint.host, endpoint.host_port))
            .credentials_provider(credentials)
            .force_path_style(true)
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .build(),
    )
}

/// Most zero-byte objects copied one by one per bucket when a mirror refuses
/// them (their keys are passed to the retry as `--exclude` arguments).
const MAX_EMPTY_OBJECTS: usize = 1000;

/// Glob that matches exactly `key`: every glob metacharacter becomes a
/// one-character class.
fn exact_glob(key: &str) -> String {
    let mut glob = String::with_capacity(key.len());
    for c in key.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\' | '!') {
            glob.push('[');
            glob.push(c);
            glob.push(']');
        } else {
            glob.push(c);
        }
    }
    glob
}

/// `command` (an `rc mirror` invocation) with each of `keys` excluded from
/// the transfer. The direct form ends with source and target, so the flags go
/// in front of them; the staged form (`sh -c <script> sh <src> <dst> <mode>
/// <side>`) takes the flags as trailing arguments, which the script passes to
/// the upload step.
fn with_excluded_keys(command: &[String], keys: &[String]) -> Vec<String> {
    let exclusions: Vec<String> = keys
        .iter()
        .flat_map(|key| ["--exclude".to_string(), exact_glob(key)])
        .collect();
    if command.first().map(String::as_str) == Some("sh") {
        let mut result = command.to_vec();
        result.extend(exclusions);
        return result;
    }
    let split = command.len().saturating_sub(2);
    let mut result: Vec<String> = command[..split].to_vec();
    result.extend(exclusions);
    result.extend_from_slice(&command[split..]);
    result
}

/// The object key relative to the mirrored tree, exactly as stored: only the
/// listing prefix is removed, so a key that itself starts with `/` keeps it.
/// `None` for the prefix's own placeholder and for directory markers.
fn key_below_prefix<'a>(full_key: &'a str, prefix: &str) -> Option<&'a str> {
    let key = full_key.strip_prefix(prefix)?;
    if key.is_empty() || key.ends_with('/') {
        None
    } else {
        Some(key)
    }
}

/// Where zero-byte objects of one bucket are read from and written to when
/// `rc` refuses them: a prefix of a source bucket, and the target bucket.
struct EmptyObjectCopy {
    /// Names the source in errors, e.g. `rustfs service 'media'`.
    source_label: String,
    from: aws_sdk_s3::Client,
    from_bucket: String,
    /// Key prefix of the mirrored tree inside `from_bucket` (empty for none).
    from_prefix: String,
    to: aws_sdk_s3::Client,
    to_bucket: String,
}

/// Copy every zero-byte object under the source prefix with plain `PutObject`
/// calls (control plane, loopback endpoints). Streams the listing page by
/// page. Returns the keys, relative to the prefix, that were put, so the
/// caller can keep the mirror from trying them again; the list is capped at
/// [`MAX_EMPTY_OBJECTS`] because it ends up on a command line.
async fn copy_empty_objects(
    service: &str,
    copy: &EmptyObjectCopy,
) -> Result<Vec<String>, ManagedS3Error> {
    let fail = |operation: &str, reason: String| ManagedS3Error::Mirror {
        service: service.to_string(),
        operation: operation.to_string(),
        reason,
    };
    let bucket = &copy.to_bucket;

    let mut copied: Vec<String> = Vec::new();
    let mut listing = copy.from.list_objects_v2().bucket(&copy.from_bucket);
    if !copy.from_prefix.is_empty() {
        listing = listing.prefix(&copy.from_prefix);
    }
    let mut pages = listing.into_paginator().send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|e| {
            fail(
                &format!(
                    "listing bucket '{}' of {}",
                    copy.from_bucket, copy.source_label
                ),
                e.to_string(),
            )
        })?;
        for object in page.contents() {
            let (Some(full_key), Some(0)) = (object.key(), object.size()) else {
                continue;
            };
            let Some(key) = key_below_prefix(full_key, &copy.from_prefix) else {
                continue;
            };
            if copied.len() >= MAX_EMPTY_OBJECTS {
                return Err(fail(
                    &format!("copying empty objects of bucket '{bucket}'"),
                    format!(
                        "more than {MAX_EMPTY_OBJECTS} zero-byte objects; the target backend \
                         cannot take them through a mirror and copying them one by one is capped"
                    ),
                ));
            }
            // The mirror would have carried the object's headers and user
            // metadata, so the direct copy reads them from the source too.
            let head = copy
                .from
                .head_object()
                .bucket(&copy.from_bucket)
                .key(full_key)
                .send()
                .await
                .map_err(|e| {
                    fail(
                        &format!(
                            "reading metadata of empty object '{full_key}' in bucket '{}' of {}",
                            copy.from_bucket, copy.source_label
                        ),
                        e.to_string(),
                    )
                })?;
            copy.to
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
                .set_content_type(head.content_type().map(str::to_string))
                .set_cache_control(head.cache_control().map(str::to_string))
                .set_content_disposition(head.content_disposition().map(str::to_string))
                .set_content_encoding(head.content_encoding().map(str::to_string))
                .set_content_language(head.content_language().map(str::to_string))
                // The SDK deprecates the parsed value in favour of the raw
                // header string, but `put_object` only accepts a parsed date.
                .set_expires({
                    #[allow(deprecated)]
                    let expires = head.expires().cloned();
                    expires
                })
                .set_metadata(head.metadata().cloned())
                .send()
                .await
                .map_err(|e| {
                    fail(
                        &format!("copying empty object '{key}' into bucket '{bucket}'"),
                        e.to_string(),
                    )
                })?;
            copied.push(key.to_string());
        }
    }
    Ok(copied)
}

/// SDK client for a backup location, for the control plane. A location with
/// no endpoint is AWS S3 proper: the bucket's region is looked up first (the
/// global endpoint answers `GetBucketLocation` for any region), falling back
/// to `us-east-1`.
async fn location_sdk_client(location: &S3Location) -> aws_sdk_s3::Client {
    let credentials = aws_sdk_s3::config::Credentials::new(
        &location.access_key,
        &location.secret_key,
        location.session_token.clone(),
        None,
        "temps-restore",
    );
    let build = |region: String, endpoint: Option<String>, path_style: bool| {
        let mut config = aws_sdk_s3::Config::builder()
            .region(aws_sdk_s3::config::Region::new(region))
            .credentials_provider(credentials.clone())
            .force_path_style(path_style)
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest());
        if let Some(endpoint) = endpoint {
            config = config.endpoint_url(endpoint);
        }
        aws_sdk_s3::Client::from_conf(config.build())
    };
    match location.endpoint.as_deref() {
        Some(endpoint) => {
            let url = if endpoint.starts_with("http") {
                endpoint.to_string()
            } else {
                format!("http://{endpoint}")
            };
            build("us-east-1".to_string(), Some(url), true)
        }
        None => {
            let global = build("us-east-1".to_string(), None, false);
            let region = match global
                .get_bucket_location()
                .bucket(&location.bucket)
                .send()
                .await
            {
                Ok(output) => match output.location_constraint().map(|c| c.as_str()) {
                    None | Some("") => "us-east-1".to_string(),
                    Some("EU") => "eu-west-1".to_string(),
                    Some(region) => region.to_string(),
                },
                Err(e) => {
                    // A key without `s3:GetBucketLocation` is common; the
                    // global endpoint still names the bucket's region in the
                    // `x-amz-bucket-region` header of any answer about it.
                    let header_region =
                        match global.head_bucket().bucket(&location.bucket).send().await {
                            Ok(_) => None,
                            Err(head_error) => head_error
                                .raw_response()
                                .and_then(|response| response.headers().get("x-amz-bucket-region"))
                                .map(str::to_string),
                        };
                    match header_region {
                        Some(region) if !region.is_empty() => region,
                        _ => {
                            warn!(
                                bucket = %location.bucket,
                                "Could not look up the region of the backup bucket, assuming us-east-1: {}",
                                e
                            );
                            "us-east-1".to_string()
                        }
                    }
                }
            };
            build(region, None, false)
        }
    }
}

/// Run a plain `rc mirror` (`command` ends with source and target). When it
/// fails and `fallback` is given, copy the zero-byte objects directly (`rc`
/// uploads them as a multipart upload with no parts, which Garage refuses)
/// and mirror again without them. Any other failure is returned untouched.
async fn run_mirror_with_empty_object_fallback(
    session: &RcSession,
    service: &str,
    operation: &str,
    sensitive: &SensitiveValues<'_>,
    command: &[String],
    fallback: Option<EmptyObjectCopy>,
) -> Result<(), ManagedS3Error> {
    let first: Vec<&str> = command.iter().map(String::as_str).collect();
    let first_error = match session.run(operation, &first, sensitive).await {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let Some(copy) = fallback else {
        return Err(first_error);
    };
    match copy_empty_objects(service, &copy).await {
        Ok(keys) if keys.is_empty() => Err(first_error),
        Ok(keys) => {
            info!(
                service = %service,
                bucket = %copy.to_bucket,
                count = keys.len(),
                "Copied zero-byte objects directly after the mirror refused them"
            );
            // `rc` would try the empty objects again (and fail again), so the
            // second pass leaves them out.
            let retry = with_excluded_keys(command, &keys);
            let retry: Vec<&str> = retry.iter().map(String::as_str).collect();
            session.run(operation, &retry, sensitive).await?;
            Ok(())
        }
        Err(reconcile_error) => {
            warn!(
                service = %service,
                bucket = %copy.to_bucket,
                "Could not copy zero-byte objects after a failed mirror: {}",
                reconcile_error
            );
            Err(first_error)
        }
    }
}

/// Copy every bucket of `source` into `backend` using plain S3 reads and
/// writes, so the move works between any two backends. Existing objects with
/// the same key are overwritten; objects only present in the target are kept.
pub(crate) async fn migrate_buckets<B: ManagedS3Backend + ?Sized>(
    docker: Arc<Docker>,
    backend: &B,
    config: &ServiceConfig,
    source: &MigrationSource,
) -> Result<ManagedS3MirrorReport, ManagedS3Error> {
    let service = backend.service_name();
    let endpoint = backend.endpoint(config)?;
    let (admin_access_key, admin_secret_key) = backend.admin_credentials(config)?;
    let env = vec![
        service_alias("svc", &endpoint, &admin_access_key, &admin_secret_key),
        service_alias(
            "src",
            &source.endpoint,
            &source.admin_access_key,
            &source.admin_secret_key,
        ),
    ];
    let sensitive = SensitiveValues::new()
        .credential(&admin_access_key, &admin_secret_key, None)
        .credential(&source.admin_access_key, &source.admin_secret_key, None);

    let session = RcSession::start(docker, &service, env).await?;
    let result = async {
        let buckets = rc_client::list_dir_names(
            session.docker(),
            session.container_id(),
            "src/",
            false,
            &sensitive,
        )
        .await
        .map_err(|e| {
            mirror_failure(
                &service,
                &format!(
                    "listing the buckets of source {} service '{}'",
                    source.backend, source.service
                ),
                e,
            )
        })?;

        for bucket in &buckets {
            backend.create_bucket(config, bucket).await?;
            let mirror = rc_client::mirror_command(
                &format!("src/{bucket}/"),
                &format!("svc/{bucket}/"),
                false,
                TokenSide::Neither,
            );
            let operation = format!(
                "migrating bucket '{bucket}' from {} service '{}'",
                source.backend, source.service
            );
            let fallback = EmptyObjectCopy {
                source_label: format!("{} service '{}'", source.backend, source.service),
                from: sdk_client(
                    &source.endpoint,
                    &source.admin_access_key,
                    &source.admin_secret_key,
                ),
                from_bucket: bucket.clone(),
                from_prefix: String::new(),
                to: sdk_client(&endpoint, &admin_access_key, &admin_secret_key),
                to_bucket: bucket.clone(),
            };
            run_mirror_with_empty_object_fallback(
                &session,
                &service,
                &operation,
                &sensitive,
                &mirror,
                Some(fallback),
            )
            .await?;
        }
        Ok::<_, ManagedS3Error>(buckets)
    }
    .await;
    session.finish().await;
    let buckets = result?;
    info!(
        service = %service,
        source = %source.service,
        source_backend = %source.backend,
        buckets = ?buckets,
        "Migrated managed S3 buckets"
    );
    Ok(ManagedS3MirrorReport { buckets })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(prefix: &str) -> S3Location {
        S3Location {
            endpoint: None,
            access_key: "AK".to_string(),
            secret_key: "SK".to_string(),
            session_token: None,
            bucket: "backups".to_string(),
            prefix: prefix.to_string(),
        }
    }

    #[test]
    fn loopback_endpoints_are_recognised_by_host_and_port() {
        assert_eq!(loopback_endpoint_port("http://127.0.0.1:9002"), Some(9002));
        assert_eq!(loopback_endpoint_port("localhost:9000/"), Some(9000));
        assert_eq!(
            loopback_endpoint_port("http://localhost:3900/x"),
            Some(3900)
        );
        assert_eq!(loopback_endpoint_port("http://minio.internal:9000"), None);
        assert_eq!(loopback_endpoint_port("https://s3.amazonaws.com"), None);
        assert_eq!(loopback_endpoint_port("http://127.0.0.1"), None);
    }

    #[tokio::test]
    async fn tcp_port_probe_tells_a_listener_from_a_closed_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let open = listener.local_addr().expect("bound address").to_string();
        assert!(tcp_port_accepts(&open).await);
        drop(listener);
        assert!(!tcp_port_accepts(&open).await);
    }

    #[test]
    fn host_gateway_endpoint_replaces_only_loopback_hosts() {
        assert_eq!(
            host_gateway_endpoint("http://127.0.0.1:9002"),
            "http://host.docker.internal:9002"
        );
        assert_eq!(
            host_gateway_endpoint("http://store.internal:9002"),
            "http://store.internal:9002"
        );
    }

    #[test]
    fn exact_glob_escapes_metacharacters_only() {
        assert_eq!(exact_glob("dir/empty"), "dir/empty");
        assert_eq!(exact_glob("a*b?[c]{d}"), "a[*]b[?][[]c[]][{]d[}]");
    }

    #[test]
    fn exclusions_are_appended_to_a_staged_mirror() {
        let command = rc_client::mirror_command("src/b/", "dst/b/", true, TokenSide::Source);
        let retried = with_excluded_keys(&command, &["dir/empty".to_string()]);
        assert_eq!(retried[..command.len()], command[..]);
        assert_eq!(retried[command.len()..], ["--exclude", "dir/empty"]);
    }

    #[test]
    fn exclusions_apply_to_an_exact_direct_mirror() {
        let command = rc_client::mirror_command("src/b/", "dst/b/", true, TokenSide::Neither);
        let retried = with_excluded_keys(&command, &["e".to_string()]);
        assert_eq!(
            retried,
            vec![
                "rc",
                "mirror",
                "--overwrite",
                "--remove",
                "--exclude",
                "e",
                "src/b/",
                "dst/b/"
            ]
        );
    }

    #[test]
    fn a_key_keeps_everything_after_the_listing_prefix() {
        assert_eq!(key_below_prefix("p/b/empty", "p/b/"), Some("empty"));
        // A stored key that starts with a slash is not the key without it.
        assert_eq!(key_below_prefix("p/b//empty", "p/b/"), Some("/empty"));
        assert_eq!(key_below_prefix("empty", ""), Some("empty"));
        assert_eq!(key_below_prefix("p/b/", "p/b/"), None);
        assert_eq!(key_below_prefix("p/b/dir/", "p/b/"), None);
        assert_eq!(key_below_prefix("other/empty", "p/b/"), None);
    }

    #[test]
    fn excluded_keys_go_before_the_source_and_target() {
        let command = rc_client::mirror_command("src/b/", "svc/b/", false, TokenSide::Neither);
        let retried = with_excluded_keys(&command, &["x/empty".to_string(), "e".to_string()]);
        assert_eq!(
            retried,
            vec![
                "rc",
                "mirror",
                "--overwrite",
                "--exclude",
                "x/empty",
                "--exclude",
                "e",
                "src/b/",
                "svc/b/"
            ]
        );
    }

    #[test]
    fn location_paths_trim_slashes_and_handle_an_empty_prefix() {
        assert_eq!(location_path("dst", &location("")), "dst/backups");
        assert_eq!(
            location_path("dst", &location("/svc/2026/")),
            "dst/backups/svc/2026"
        );
    }

    #[test]
    fn a_location_without_an_endpoint_is_aws_s3() {
        assert_eq!(location_endpoint(&location("")), "https://s3.amazonaws.com");
        let mut custom = location("");
        custom.endpoint = Some("http://minio.internal:9000".to_string());
        assert_eq!(location_endpoint(&custom), "http://minio.internal:9000");
    }

    #[test]
    fn service_aliases_address_the_container_not_localhost() {
        let endpoint = ManagedS3Endpoint {
            internal_host: "garage-media".to_string(),
            internal_port: 3900,
            host: "localhost".to_string(),
            host_port: 39000,
            region: "us-east-1".to_string(),
        };
        let alias = service_alias("svc", &endpoint, "AK", "SK");
        assert_eq!(alias, "RC_HOST_svc=http://AK:SK@garage-media:3900");
    }

    /// A session whose owner is cancelled mid-operation (a dropped request
    /// future) must not leave its credential-bearing container running.
    #[tokio::test]
    async fn a_dropped_session_removes_its_helper_container() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let docker = Arc::new(docker);
        let session = match RcSession::start(
            docker.clone(),
            "drop-test",
            vec!["RC_HOST_x=http://AK:SK@127.0.0.1:1".to_string()],
        )
        .await
        {
            Ok(session) => session,
            Err(e) => {
                println!("Could not start the rc helper, skipping: {e}");
                return;
            }
        };
        let container_id = session.container_id().to_string();

        drop(session);

        let mut gone = false;
        for _ in 0..50 {
            if docker
                .inspect_container(
                    &container_id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .is_err()
            {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(gone, "helper container {container_id} outlived its session");
    }
}
