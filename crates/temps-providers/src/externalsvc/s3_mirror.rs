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

/// A running `rc` helper container. Always call [`RcSession::finish`]: the
/// container carries credentials and must not outlive the operation.
pub(crate) struct RcSession {
    docker: Arc<Docker>,
    container_id: String,
    service: String,
}

impl RcSession {
    /// Pull the `rc` image and start a helper with `env` (the `RC_HOST_*`
    /// aliases and, for a temporary credential, the session-token variable).
    pub(crate) async fn start(
        docker: Arc<Docker>,
        service: &str,
        env: Vec<String>,
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
            labels: Some(std::collections::HashMap::from([(
                label_key,
                service.to_string(),
            )])),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some(temps_core::NETWORK_NAME.to_string()),
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
    pub(crate) async fn finish(self) {
        if let Err(e) = self
            .docker
            .remove_container(
                &self.container_id,
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
                    service = %self.service,
                    container = %self.container_id,
                    "Failed to remove credential-bearing rc helper container: {}",
                    e
                );
            }
        }
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
            &location_endpoint(destination),
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
            &location_endpoint(source),
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

    let session = RcSession::start(backend.docker(), &service, env).await?;
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
            let command = rc_client::mirror_command(
                &format!("{root}{bucket}/"),
                &format!("svc/{bucket}/"),
                exact,
                TokenSide::when(has_token, TokenSide::Source),
            );
            let command: Vec<&str> = command.iter().map(String::as_str).collect();
            session
                .run(
                    &format!("restoring bucket '{bucket}'"),
                    &command,
                    &sensitive,
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

/// Copy every zero-byte object of `bucket` from `source` to `target` with
/// plain `PutObject` calls (control plane, loopback endpoints). Streams the
/// listing page by page, so memory stays constant. Returns how many were put.
async fn copy_empty_objects(
    service: &str,
    source: &MigrationSource,
    target: (&ManagedS3Endpoint, &str, &str),
    bucket: &str,
) -> Result<usize, ManagedS3Error> {
    let fail = |operation: &str, reason: String| ManagedS3Error::Mirror {
        service: service.to_string(),
        operation: operation.to_string(),
        reason,
    };
    let from = sdk_client(
        &source.endpoint,
        &source.admin_access_key,
        &source.admin_secret_key,
    );
    let to = sdk_client(target.0, target.1, target.2);

    let mut copied = 0usize;
    let mut pages = from
        .list_objects_v2()
        .bucket(bucket)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|e| {
            fail(
                &format!(
                    "listing bucket '{bucket}' of {} service '{}'",
                    source.backend, source.service
                ),
                e.to_string(),
            )
        })?;
        for object in page.contents() {
            let (Some(key), Some(0)) = (object.key(), object.size()) else {
                continue;
            };
            to.put_object()
                .bucket(bucket)
                .key(key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
                .send()
                .await
                .map_err(|e| {
                    fail(
                        &format!("copying empty object '{key}' into bucket '{bucket}'"),
                        e.to_string(),
                    )
                })?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Copy every bucket of `source` into `backend` using plain S3 reads and
/// writes, so the move works between any two backends. Existing objects with
/// the same key are overwritten; objects only present in the target are kept.
pub(crate) async fn migrate_buckets<B: ManagedS3Backend + ?Sized>(
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

    let session = RcSession::start(backend.docker(), &service, env).await?;
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
            let command = rc_client::mirror_command(
                &format!("src/{bucket}/"),
                &format!("svc/{bucket}/"),
                false,
                TokenSide::Neither,
            );
            let command: Vec<&str> = command.iter().map(String::as_str).collect();
            let operation = format!(
                "migrating bucket '{bucket}' from {} service '{}'",
                source.backend, source.service
            );
            if let Err(first_error) = session.run(&operation, &command, &sensitive).await {
                // `rc` uploads a zero-byte object as a multipart upload with no
                // parts, which Garage refuses. Copy those with plain PUTs and
                // mirror again; any other failure is returned untouched.
                let copied = copy_empty_objects(
                    &service,
                    source,
                    (&endpoint, &admin_access_key, &admin_secret_key),
                    bucket,
                )
                .await;
                match copied {
                    Ok(0) => return Err(first_error),
                    Ok(count) => {
                        info!(
                            service = %service,
                            bucket = %bucket,
                            count,
                            "Copied zero-byte objects directly after the mirror refused them"
                        );
                        session.run(&operation, &command, &sensitive).await?;
                    }
                    Err(reconcile_error) => {
                        warn!(
                            service = %service,
                            bucket = %bucket,
                            "Could not copy zero-byte objects after a failed mirror: {}",
                            reconcile_error
                        );
                        return Err(first_error);
                    }
                }
            }
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
}
