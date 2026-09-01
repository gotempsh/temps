// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared helper for ensuring a Docker image is locally cached before
//! `create_container`.
//!
//! Without this, fresh prod hosts 404 on `create_container` with
//! "No such image: …" because the engine assumed Docker would auto-pull
//! (it doesn't — bare `create_container` against a missing image fails).

use bollard::Docker;
use futures::stream::StreamExt as FuturesStreamExt;
use tracing::{debug, info};

use temps_backup_core::engine_v2::BackupError;

/// Ensure `image_tag` is pulled and available locally. No-op if Docker
/// already has the image cached; otherwise streams a pull and returns
/// after the daemon reports completion.
///
/// Pull failures are mapped to `BackupError::Failed` so the engine's caller
/// surfaces a useful message ("failed to pull '…': …") instead of a generic
/// 404 on the subsequent `create_container` call.
///
/// `engine` is the engine key (e.g. `"control_plane"`, `"postgres_pgdump"`);
/// errors carry it in logs so failures land on the right engine in the UI.
pub async fn ensure_image_pulled_v2(image_tag: &str, engine: &str) -> Result<(), BackupError> {
    let docker = connect_to_docker("ensure_image_pulled_v2", engine)?;

    if docker.inspect_image(image_tag).await.is_ok() {
        return Ok(());
    }

    info!(
        image_tag,
        engine, "ensure_image_pulled_v2: image not cached, pulling"
    );

    pull_image(&docker, image_tag, engine).await
}

/// Refresh an image from its registry even if the same tag exists locally.
///
/// This is reserved for short-lived helpers that receive credentials. A local
/// actor must not be able to pre-populate a trusted tag and have that image run
/// with backup credentials.
pub async fn force_pull_image_v2(image_tag: &str, engine: &str) -> Result<(), BackupError> {
    let docker = connect_to_docker("force_pull_image_v2", engine)?;

    info!(
        image_tag,
        engine, "force_pull_image_v2: refreshing image from registry"
    );
    pull_image(&docker, image_tag, engine).await
}

fn connect_to_docker(operation: &str, engine: &str) -> Result<Docker, BackupError> {
    Docker::connect_with_local_defaults().map_err(|error| BackupError::Failed {
        reason: format!("{operation} ({engine}): failed to connect to Docker: {error}"),
    })
}

fn split_image_tag(image_tag: &str) -> (&str, Option<&str>) {
    if image_tag.contains("@sha256:") {
        return (image_tag, None);
    }

    match image_tag.rsplit_once(':') {
        Some((image, tag)) if !tag.contains('/') => (image, Some(tag)),
        _ => (image_tag, Some("latest")),
    }
}

async fn pull_image(docker: &Docker, image_tag: &str, engine: &str) -> Result<(), BackupError> {
    use bollard::query_parameters::CreateImageOptionsBuilder;
    let (image, tag) = split_image_tag(image_tag);
    let mut options = CreateImageOptionsBuilder::new().from_image(image);
    if let Some(tag) = tag {
        options = options.tag(tag);
    }
    let options = options.build();

    let mut stream = docker.create_image(Some(options), None, None);
    while let Some(result) = FuturesStreamExt::next(&mut stream).await {
        match result {
            Ok(info) => {
                if let Some(status) = info.status {
                    debug!(image_tag, engine, "Docker pull: {}", status);
                }
            }
            Err(e) => {
                return Err(BackupError::Failed {
                    reason: format!("failed to pull '{}': {}", image_tag, e),
                });
            }
        }
    }

    info!(image_tag, engine, "Docker pull complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::split_image_tag;

    #[test]
    fn split_image_tag_handles_explicit_and_implicit_tags() {
        assert_eq!(
            split_image_tag("minio/mc:RELEASE.2025-08-13T08-35-41Z"),
            ("minio/mc", Some("RELEASE.2025-08-13T08-35-41Z"))
        );
        assert_eq!(split_image_tag("minio/mc"), ("minio/mc", Some("latest")));
    }

    #[test]
    fn split_image_tag_does_not_treat_registry_port_as_tag() {
        assert_eq!(
            split_image_tag("registry.example.test:5000/minio/mc"),
            ("registry.example.test:5000/minio/mc", Some("latest"))
        );
    }

    #[test]
    fn split_image_tag_preserves_digest_reference() {
        let image = "minio/mc@sha256:0123456789abcdef";
        assert_eq!(split_image_tag(image), (image, None));
    }
}
