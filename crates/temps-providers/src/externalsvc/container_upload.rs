// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Put files into a container through the Docker archive API.
//!
//! Restore helpers need input files (a snapshot, an archive, a credentials
//! file). Bind-mounting a host temp directory only works when the Docker
//! daemon sees the same filesystem, which is not the case for Docker running
//! in a VM (Colima, Docker Desktop) or on a remote host: the container then
//! sees an empty directory. Uploading into a created-but-not-started
//! container works wherever the daemon is, and streams, so memory does not
//! depend on file size.

use anyhow::Result;
use bollard::Docker;

/// Read size when streaming a host file into a container upload.
const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;

/// The tar stream that uploads one regular file named `name` of `size`
/// bytes with permission bits `mode`: header, then `body`, then padding and
/// the end-of-archive marker. Built lazily, so memory does not depend on
/// `size`.
pub(crate) fn single_file_tar_stream<S>(
    name: &str,
    size: u64,
    mode: u32,
    body: S,
) -> Result<impl futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static>
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    use futures::StreamExt;

    let mut header = tar::Header::new_gnu();
    header
        .set_path(name)
        .map_err(|e| anyhow::anyhow!("Invalid upload file name '{}': {}", name, e))?;
    header.set_size(size);
    header.set_mode(mode);
    header.set_mtime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    );
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    let header_bytes = bytes::Bytes::copy_from_slice(header.as_bytes());
    // Pad the file to a 512-byte block, then two zero blocks end the archive.
    let padding = ((512 - (size % 512)) % 512) as usize;
    let trailer = bytes::Bytes::from(vec![0_u8; padding + 1024]);

    Ok(futures::stream::once(async move { Ok(header_bytes) })
        .chain(body)
        .chain(futures::stream::once(async move { Ok(trailer) })))
}

async fn upload_tar_stream<S>(
    docker: &Docker,
    container_id: &str,
    dest_dir: &str,
    dest_name: &str,
    size: u64,
    timeout: std::time::Duration,
    tar_stream: S,
) -> Result<()>
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    tokio::time::timeout(
        timeout,
        docker.upload_to_container(
            container_id,
            Some(bollard::query_parameters::UploadToContainerOptions {
                path: dest_dir.to_string(),
                ..Default::default()
            }),
            bollard::body_try_stream(tar_stream),
        ),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Timed out after {:?} uploading {} ({} bytes) into container {}",
            timeout,
            dest_name,
            size,
            container_id
        )
    })?
    .map_err(|e| {
        anyhow::anyhow!(
            "Failed to upload {} bytes into {}:{}/{}: {}",
            size,
            container_id,
            dest_dir,
            dest_name,
            e
        )
    })
}

/// Upload one host file into a container at `dest_dir/dest_name`. Works on a
/// created-but-not-started container, streams the file (constant memory),
/// and needs no filesystem shared with the Docker daemon. `dest_dir` must
/// already exist in the image.
pub(crate) async fn upload_file_to_container(
    docker: &Docker,
    container_id: &str,
    host_path: &std::path::Path,
    dest_dir: &str,
    dest_name: &str,
    mode: u32,
    timeout: std::time::Duration,
) -> Result<()> {
    let file = tokio::fs::File::open(host_path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to open {} for upload: {}", host_path.display(), e))?;
    let size = file
        .metadata()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to stat {}: {}", host_path.display(), e))?
        .len();
    let body = futures::stream::try_unfold((file, size), |(mut file, remaining)| async move {
        use tokio::io::AsyncReadExt;
        if remaining == 0 {
            return Ok(None);
        }
        let want = remaining.min(UPLOAD_CHUNK_BYTES as u64) as usize;
        let mut buf = vec![0_u8; want];
        file.read_exact(&mut buf).await?;
        Ok(Some((
            bytes::Bytes::from(buf),
            (file, remaining - want as u64),
        )))
    });
    let tar_stream = single_file_tar_stream(dest_name, size, mode, body)?;
    upload_tar_stream(
        docker,
        container_id,
        dest_dir,
        dest_name,
        size,
        timeout,
        tar_stream,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{} (source {})", e, host_path.display()))
}

/// Upload a small in-memory file (e.g. a credentials file) into a container
/// without ever writing it to the host's disk.
pub(crate) async fn upload_bytes_to_container(
    docker: &Docker,
    container_id: &str,
    contents: Vec<u8>,
    dest_dir: &str,
    dest_name: &str,
    mode: u32,
    timeout: std::time::Duration,
) -> Result<()> {
    let size = contents.len() as u64;
    let body = futures::stream::once(async move { Ok(bytes::Bytes::from(contents)) });
    let tar_stream = single_file_tar_stream(dest_name, size, mode, body)?;
    upload_tar_stream(
        docker,
        container_id,
        dest_dir,
        dest_name,
        size,
        timeout,
        tar_stream,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_file_tar_stream_is_a_valid_archive_of_the_body() {
        use futures::StreamExt;
        use std::io::Read;

        for size in [0_usize, 1, 511, 512, 513, 70_000] {
            let payload: Vec<u8> = (0..size).map(|i| (i % 253) as u8).collect();
            let chunks: Vec<std::io::Result<bytes::Bytes>> = payload
                .chunks(4_096)
                .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
                .collect();
            let stream = single_file_tar_stream(
                "temps-restore.rdb",
                size as u64,
                0o600,
                futures::stream::iter(chunks),
            )
            .expect("build tar stream");
            let mut archive_bytes = Vec::new();
            let mut stream = Box::pin(stream);
            while let Some(chunk) = stream.next().await {
                archive_bytes.extend_from_slice(&chunk.expect("chunk"));
            }
            assert_eq!(
                archive_bytes.len() % 512,
                0,
                "size {size}: not block aligned"
            );

            let mut archive = tar::Archive::new(archive_bytes.as_slice());
            let mut entries = archive.entries().expect("entries");
            let mut entry = entries.next().expect("one entry").expect("entry");
            assert_eq!(
                entry.path().expect("path").to_str(),
                Some("temps-restore.rdb")
            );
            assert_eq!(entry.header().mode().expect("mode"), 0o600);
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).expect("read entry");
            assert_eq!(contents, payload, "size {size}: contents differ");
            drop(entry);
            assert!(entries.next().is_none(), "size {size}: extra entries");
        }
    }

    /// Both upload paths land the file, with its mode, in a container that
    /// was created but never started — and the source never needs to be
    /// visible to the Docker daemon.
    #[tokio::test]
    async fn uploads_reach_a_created_container_without_a_shared_path() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let image = "busybox:latest";
        if crate::utils::pull_image_with_retry(&docker, image, None)
            .await
            .is_err()
        {
            println!("Could not pull {image}, skipping");
            return;
        }
        let name = format!(
            "temps-upload-test-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let Ok(created) = docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec!["true".into()]),
                    ..Default::default()
                },
            )
            .await
        else {
            println!("Could not create upload test container, skipping");
            return;
        };

        let host_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => {
                println!("No host temp dir ({e}), skipping");
                return;
            }
        };
        let host_file = host_dir.path().join("archive.gz");
        let payload: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
        let wrote = std::fs::write(&host_file, &payload);
        let timeout = std::time::Duration::from_secs(60);

        let file_upload = match wrote {
            Ok(()) => {
                upload_file_to_container(
                    &docker,
                    &created.id,
                    &host_file,
                    "/tmp",
                    "archive.gz",
                    0o644,
                    timeout,
                )
                .await
            }
            Err(e) => Err(anyhow::anyhow!("write host file: {e}")),
        };
        let bytes_upload = upload_bytes_to_container(
            &docker,
            &created.id,
            b"password: 'x'\n".to_vec(),
            "/tmp",
            "restore.yaml",
            0o600,
            timeout,
        )
        .await;

        let mut found = std::collections::HashMap::new();
        if file_upload.is_ok() && bytes_upload.is_ok() {
            use futures::StreamExt;
            for path in ["/tmp/archive.gz", "/tmp/restore.yaml"] {
                let mut stream = docker.download_from_container(
                    &created.id,
                    Some(
                        bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
                            .path(path)
                            .build(),
                    ),
                );
                let mut tar_bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(chunk) => tar_bytes.extend_from_slice(&chunk),
                        Err(_) => break,
                    }
                }
                let mut archive = tar::Archive::new(tar_bytes.as_slice());
                if let Ok(mut entries) = archive.entries() {
                    if let Some(Ok(mut entry)) = entries.next() {
                        use std::io::Read;
                        let mode = entry.header().mode().unwrap_or(0);
                        let mut contents = Vec::new();
                        let _ = entry.read_to_end(&mut contents);
                        found.insert(path, (mode, contents));
                    }
                }
            }
        }
        let _ = docker
            .remove_container(
                &created.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        file_upload.expect("file upload");
        bytes_upload.expect("bytes upload");
        let (_, archive) = found.get("/tmp/archive.gz").expect("archive uploaded");
        assert_eq!(archive, &payload);
        let (mode, config) = found.get("/tmp/restore.yaml").expect("config uploaded");
        assert_eq!(config.as_slice(), b"password: 'x'\n");
        assert_eq!(mode & 0o777, 0o600);
    }
}
