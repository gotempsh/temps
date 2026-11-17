// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded extraction of static files streamed out of a Docker image.

use super::DockerRuntime;
use crate::static_ingestion::{MAX_STATIC_ENTRIES, MAX_STATIC_ENTRY_BYTES, MAX_STATIC_TOTAL_BYTES};
use crate::BuilderError;
use futures::{Stream, StreamExt};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path};
use tempfile::TempDir;
use temps_core::static_files::MAX_STATIC_PATH_COMPONENTS;
use tokio::io::AsyncWriteExt;

pub(super) const MAX_STATIC_ARCHIVE_STREAM_BYTES: u64 =
    MAX_STATIC_TOTAL_BYTES + (MAX_STATIC_ENTRIES as u64 * 1024) + (1024 * 1024);

/// The name Docker's container-archive API gives the root entry of a
/// download of `source_path`: its final path component.
pub(super) fn static_archive_root(
    source_path: &str,
    image_name: &str,
) -> Result<std::ffi::OsString, BuilderError> {
    Path::new(source_path)
        .file_name()
        .filter(|component| !component.is_empty())
        .map(|component| component.to_os_string())
        .ok_or_else(|| {
            BuilderError::InvalidContext(format!(
                "Docker extraction source path '{source_path}' for image '{image_name}' must name a file or directory"
            ))
        })
}

/// Unpack a container-archive tar of `source_path` from `image_name` into
/// `destination`, enforcing the static byte, entry, depth and type limits.
///
/// Shared by local extraction and by images built on a worker node, whose
/// archive arrives over the agent channel. Either way the bytes are validated
/// on the host that will serve them, never trusted for where they came from.
pub(crate) async fn unpack_static_archive_stream<S, E>(
    stream: S,
    image_name: &str,
    source_path: &str,
    destination: &Path,
) -> Result<(), BuilderError>
where
    S: Stream<Item = Result<bytes::Bytes, E>>,
    E: std::fmt::Display,
{
    let archive_root = static_archive_root(source_path, image_name)?;

    // The archive and extraction directory are removed when this scope exits.
    let temp_dir = TempDir::new().map_err(|error| {
        BuilderError::IoError(std::io::Error::new(
            error.kind(),
            format!(
                "Failed to create temporary Docker extraction directory for image '{image_name}' path '{source_path}': {error}"
            ),
        ))
    })?;
    let archive_path = temp_dir.path().join("static-output.tar");
    let extraction_path = temp_dir.path().join("extracted");

    DockerRuntime::write_bounded_byte_stream(
        stream,
        &archive_path,
        MAX_STATIC_ARCHIVE_STREAM_BYTES,
        image_name,
        source_path,
    )
    .await?;

    let blocking_archive_path = archive_path.clone();
    let blocking_extraction_path = extraction_path.clone();
    let blocking_image_name = image_name.to_string();
    let blocking_source_path = source_path.to_string();
    tokio::task::spawn_blocking(move || {
        extract_bounded_static_archive(
            &blocking_archive_path,
            &blocking_extraction_path,
            &blocking_image_name,
            &blocking_source_path,
        )
    })
    .await
    .map_err(|error| {
        BuilderError::Other(format!(
            "Docker archive extraction task failed for image '{image_name}' path '{source_path}': {error}"
        ))
    })??;

    let extracted_dir = extraction_path.join(&archive_root);
    let canonical_extraction_path = tokio::fs::canonicalize(&extraction_path)
        .await
        .map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to resolve Docker extraction root {} for image '{image_name}' path '{source_path}': {error}",
                    extraction_path.display()
                ),
            ))
        })?;
    let canonical_extracted_dir = tokio::fs::canonicalize(&extracted_dir)
        .await
        .map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Docker archive for image '{image_name}' path '{source_path}' did not contain expected directory {}: {error}",
                    extracted_dir.display()
                ),
            ))
        })?;
    if !canonical_extracted_dir.starts_with(&canonical_extraction_path)
        || !canonical_extracted_dir.is_dir()
    {
        return Err(BuilderError::InvalidContext(format!(
            "Docker archive for image '{image_name}' path '{source_path}' did not resolve to a confined directory"
        )));
    }

    tokio::fs::rename(&canonical_extracted_dir, destination)
        .await
        .map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to move extracted Docker files for image '{image_name}' path '{source_path}' from {} to {}: {error}",
                    canonical_extracted_dir.display(),
                    destination.display()
                ),
            ))
        })
}

fn checked_static_archive_entry_count(
    current: u32,
    image_name: &str,
    source_path: &str,
) -> Result<u32, BuilderError> {
    let next = current.checked_add(1).ok_or_else(|| {
        BuilderError::ResourceLimitExceeded(format!(
            "Docker archive entry count overflowed for image '{image_name}' path '{source_path}'"
        ))
    })?;
    if next > MAX_STATIC_ENTRIES {
        return Err(BuilderError::ResourceLimitExceeded(format!(
            "Docker archive for image '{image_name}' path '{source_path}' exceeds the {MAX_STATIC_ENTRIES} entry limit"
        )));
    }
    Ok(next)
}

fn checked_static_archive_total(
    current: u64,
    entry_size: u64,
    image_name: &str,
    source_path: &str,
) -> Result<u64, BuilderError> {
    let next = current.checked_add(entry_size).ok_or_else(|| {
        BuilderError::ResourceLimitExceeded(format!(
            "Docker archive extracted byte count overflowed for image '{image_name}' path '{source_path}'"
        ))
    })?;
    if next > MAX_STATIC_TOTAL_BYTES {
        return Err(BuilderError::ResourceLimitExceeded(format!(
            "Docker archive for image '{image_name}' path '{source_path}' exceeds the {MAX_STATIC_TOTAL_BYTES} byte extracted-size limit"
        )));
    }
    Ok(next)
}

fn validate_static_archive_entry_path(
    path: &Path,
    image_name: &str,
    source_path: &str,
) -> Result<(), BuilderError> {
    if path.as_os_str().is_empty() {
        return Err(BuilderError::InvalidContext(format!(
            "Docker archive for image '{image_name}' path '{source_path}' contains an empty entry path"
        )));
    }

    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(BuilderError::InvalidContext(format!(
                    "Docker archive for image '{image_name}' path '{source_path}' contains unsafe entry '{}': path traversal and absolute paths are not allowed",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn validate_static_archive_relative_depth(
    path: &Path,
    image_name: &str,
    source_path: &str,
) -> Result<(), BuilderError> {
    let depth = path.components().count();
    if depth > MAX_STATIC_PATH_COMPONENTS {
        return Err(BuilderError::ResourceLimitExceeded(format!(
            "Docker archive entry '{}' for image '{image_name}' path '{source_path}' has {depth} relative components, exceeding the {MAX_STATIC_PATH_COMPONENTS} component depth limit",
            path.display()
        )));
    }
    Ok(())
}

fn process_bounded_static_archive(
    archive_path: &Path,
    destination: Option<&Path>,
    image_name: &str,
    source_path: &str,
) -> Result<(), BuilderError> {
    if let Some(destination) = destination {
        std::fs::create_dir_all(destination).map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to create Docker archive extraction directory {} for image '{image_name}' path '{source_path}': {error}",
                    destination.display()
                ),
            ))
        })?;
    }
    let file = std::fs::File::open(archive_path).map_err(|error| {
        BuilderError::IoError(std::io::Error::new(
            error.kind(),
            format!(
                "Failed to open downloaded Docker archive {} for image '{image_name}' path '{source_path}': {error}",
                archive_path.display()
            ),
        ))
    })?;
    let mut archive = tar::Archive::new(file);
    let entries = archive.entries().map_err(|error| {
        BuilderError::InvalidContext(format!(
            "Failed to read Docker archive entries for image '{image_name}' path '{source_path}': {error}"
        ))
    })?;
    let mut entry_count = 0u32;
    let mut extracted_bytes = 0u64;
    let mut seen_paths = HashSet::new();
    let archive_root = Path::new(source_path)
        .file_name()
        .filter(|component| !component.is_empty())
        .ok_or_else(|| {
            BuilderError::InvalidContext(format!(
                "Docker extraction source path '{source_path}' for image '{image_name}' must name a file or directory"
            ))
        })?;

    for entry in entries {
        entry_count = checked_static_archive_entry_count(entry_count, image_name, source_path)?;

        let mut entry = entry.map_err(|error| {
            BuilderError::InvalidContext(format!(
                "Failed to read Docker archive entry {entry_count} for image '{image_name}' path '{source_path}': {error}"
            ))
        })?;
        let path = entry
            .path()
            .map_err(|error| {
                BuilderError::InvalidContext(format!(
                    "Failed to decode Docker archive entry {entry_count} path for image '{image_name}' path '{source_path}': {error}"
                ))
            })?
            .into_owned();
        validate_static_archive_entry_path(&path, image_name, source_path)?;
        let relative_path = path.strip_prefix(archive_root).map_err(|_| {
            BuilderError::InvalidContext(format!(
                "Docker archive entry '{}' for image '{image_name}' is outside requested path root '{}'",
                path.display(),
                archive_root.to_string_lossy()
            ))
        })?;
        validate_static_archive_relative_depth(relative_path, image_name, source_path)?;
        if !seen_paths.insert(path.clone()) {
            return Err(BuilderError::InvalidContext(format!(
                "Docker archive for image '{image_name}' path '{source_path}' contains duplicate entry '{}'",
                path.display()
            )));
        }

        let destination_path = destination.map(|destination| destination.join(&path));
        if let (Some(destination), Some(destination_path)) = (destination, &destination_path) {
            if !destination_path.starts_with(destination) {
                return Err(BuilderError::InvalidContext(format!(
                    "Docker archive entry '{}' for image '{image_name}' path '{source_path}' escapes extraction directory {}",
                    path.display(),
                    destination.display()
                )));
            }
        }

        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if let Some(destination_path) = destination_path {
                    std::fs::create_dir_all(&destination_path).map_err(|error| {
                        BuilderError::IoError(std::io::Error::new(
                            error.kind(),
                            format!(
                                "Failed to create Docker archive directory {} for image '{image_name}' path '{source_path}': {error}",
                                destination_path.display()
                            ),
                        ))
                    })?;
                }
            }
            tar::EntryType::Regular => {
                let declared_size = entry.header().size().map_err(|error| {
                    BuilderError::InvalidContext(format!(
                        "Failed to read declared size for Docker archive entry '{}' in image '{image_name}' path '{source_path}': {error}",
                        path.display()
                    ))
                })?;
                if declared_size > MAX_STATIC_ENTRY_BYTES {
                    return Err(BuilderError::ResourceLimitExceeded(format!(
                        "Docker archive entry '{}' for image '{image_name}' path '{source_path}' declares {declared_size} bytes, exceeding the {MAX_STATIC_ENTRY_BYTES} byte per-entry limit",
                        path.display()
                    )));
                }
                extracted_bytes = checked_static_archive_total(
                    extracted_bytes,
                    declared_size,
                    image_name,
                    source_path,
                )?;

                if let Some(destination_path) = destination_path {
                    if let Some(parent) = destination_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| {
                            BuilderError::IoError(std::io::Error::new(
                                error.kind(),
                                format!(
                                    "Failed to create parent directory {} for Docker archive entry '{}' in image '{image_name}' path '{source_path}': {error}",
                                    parent.display(),
                                    path.display()
                                ),
                            ))
                        })?;
                    }
                    let mut output = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&destination_path)
                        .map_err(|error| {
                            BuilderError::IoError(std::io::Error::new(
                                error.kind(),
                                format!(
                                    "Failed to create extracted Docker archive file {} for image '{image_name}' path '{source_path}': {error}",
                                    destination_path.display()
                                ),
                            ))
                        })?;
                    let copied = std::io::copy(&mut entry, &mut output).map_err(|error| {
                        BuilderError::IoError(std::io::Error::new(
                            error.kind(),
                            format!(
                                "Failed to extract Docker archive entry '{}' for image '{image_name}' path '{source_path}': {error}",
                                path.display()
                            ),
                        ))
                    })?;
                    output.flush().map_err(|error| {
                        BuilderError::IoError(std::io::Error::new(
                            error.kind(),
                            format!(
                                "Failed to flush Docker archive entry '{}' for image '{image_name}' path '{source_path}': {error}",
                                path.display()
                            ),
                        ))
                    })?;
                    if copied != declared_size {
                        return Err(BuilderError::InvalidContext(format!(
                            "Docker archive entry '{}' for image '{image_name}' path '{source_path}' declared {declared_size} bytes but yielded {copied} bytes",
                            path.display()
                        )));
                    }
                }
            }
            entry_type => {
                return Err(BuilderError::InvalidContext(format!(
                    "Docker archive entry '{}' for image '{image_name}' path '{source_path}' has disallowed type {entry_type:?}; only regular files and directories are accepted",
                    path.display()
                )));
            }
        }
    }

    Ok(())
}

fn extract_bounded_static_archive(
    archive_path: &Path,
    destination: &Path,
    image_name: &str,
    source_path: &str,
) -> Result<(), BuilderError> {
    // First pass validates every header and drains every payload without
    // creating filesystem entries. This guarantees an unsafe or malformed
    // entry anywhere in the archive is rejected before extraction begins.
    process_bounded_static_archive(archive_path, None, image_name, source_path)?;
    process_bounded_static_archive(archive_path, Some(destination), image_name, source_path)
}

impl DockerRuntime {
    pub(crate) async fn write_bounded_byte_stream<S, E>(
        s: S,
        output_path: &Path,
        max_bytes: u64,
        image_name: &str,
        source_path: &str,
    ) -> Result<u64, BuilderError>
    where
        S: Stream<Item = Result<bytes::Bytes, E>>,
        E: std::fmt::Display,
    {
        let mut output = tokio::fs::File::create(output_path).await.map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to create Docker archive download {} for image '{image_name}' path '{source_path}': {error}",
                    output_path.display()
                ),
            ))
        })?;
        let mut stream = Box::pin(s);
        let mut written = 0u64;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                BuilderError::Other(format!(
                    "Failed to download Docker archive for image '{image_name}' path '{source_path}': {error}"
                ))
            })?;
            let next_size = written.checked_add(chunk.len() as u64).ok_or_else(|| {
                BuilderError::ResourceLimitExceeded(format!(
                    "Docker archive byte count overflowed for image '{image_name}' path '{source_path}'"
                ))
            })?;
            if next_size > max_bytes {
                return Err(BuilderError::ResourceLimitExceeded(format!(
                    "Docker archive stream for image '{image_name}' path '{source_path}' exceeds the {max_bytes} byte download limit"
                )));
            }
            output.write_all(&chunk).await.map_err(|error| {
                BuilderError::IoError(std::io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to write Docker archive download {} for image '{image_name}' path '{source_path}': {error}",
                        output_path.display()
                    ),
                ))
            })?;
            written = next_size;
        }

        output.flush().await.map_err(|error| {
            BuilderError::IoError(std::io::Error::new(
                error.kind(),
                format!(
                    "Failed to flush Docker archive download {} for image '{image_name}' path '{source_path}': {error}",
                    output_path.display()
                ),
            ))
        })?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::DockerRuntime;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tar_with_files(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).expect("test tar path");
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder
                .append(&header, *content)
                .expect("append test tar entry");
        }
        builder.into_inner().expect("finish test tar")
    }

    fn raw_tar_header(path: &[u8], entry_type: tar::EntryType, size: u64) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_mode(0o644);
        header.set_size(size);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path);
        header.set_cksum();
        let mut archive = header.as_bytes().to_vec();
        archive.extend_from_slice(&[0; 1024]);
        archive
    }

    #[test]
    fn bounded_static_archive_extracts_ordinary_and_well_known_files() {
        let temporary = TempDir::new().expect("temp dir");
        let archive_path = temporary.path().join("site.tar");
        let destination = temporary.path().join("extracted");
        std::fs::write(
            &archive_path,
            tar_with_files(&[
                ("dist/index.html", b"<h1>ok</h1>"),
                ("dist/assets/app.js", b"console.log('ok')"),
                (
                    "dist/.well-known/security.txt",
                    b"Contact: mailto:test@example.test",
                ),
            ]),
        )
        .expect("write archive");

        extract_bounded_static_archive(&archive_path, &destination, "temps-test", "/app/dist")
            .expect("safe archive extracts");

        assert_eq!(
            std::fs::read(destination.join("dist/index.html")).expect("read index"),
            b"<h1>ok</h1>"
        );
        assert!(destination.join("dist/.well-known/security.txt").is_file());
    }

    #[test]
    fn bounded_generic_archive_allows_private_source_maps() {
        let temporary = TempDir::new().expect("temp dir");
        let archive_path = temporary.path().join("site.tar");
        let destination = temporary.path().join("extracted");
        std::fs::write(
            &archive_path,
            tar_with_files(&[
                ("dist/app.js", b"console.log('private source map')"),
                ("dist/app.js.map", br#"{"version":3,"sources":[]}"#),
            ]),
        )
        .expect("write archive");

        extract_bounded_static_archive(&archive_path, &destination, "temps-test", "/app/dist")
            .expect("generic extraction must preserve private source maps");

        assert!(destination.join("dist/app.js.map").is_file());
    }

    #[test]
    fn test_extract_bounded_static_archive_duplicate_path_rejected_before_output_created() {
        // Arrange
        let temporary = TempDir::new().expect("temp dir");
        let archive_path = temporary.path().join("site.tar");
        let destination = temporary.path().join("extracted");
        std::fs::write(
            &archive_path,
            tar_with_files(&[
                ("dist/index.html", b"first"),
                ("dist/index.html", b"second"),
            ]),
        )
        .expect("write archive");

        // Act
        let error =
            extract_bounded_static_archive(&archive_path, &destination, "temps-test", "/app/dist")
                .expect_err("duplicate archive path must fail");

        // Assert
        assert!(matches!(error, BuilderError::InvalidContext(_)));
        assert!(error.to_string().contains("duplicate entry"));
        assert!(
            !destination.exists(),
            "preflight must reject duplicate paths before creating extraction output"
        );
    }

    #[test]
    fn bounded_static_archive_rejects_traversal_entries() {
        let temporary = TempDir::new().expect("temp dir");
        let archive_path = temporary.path().join("site.tar");
        std::fs::write(
            &archive_path,
            raw_tar_header(b"dist/../../outside", tar::EntryType::Regular, 0),
        )
        .expect("write archive");

        let error = extract_bounded_static_archive(
            &archive_path,
            &temporary.path().join("extracted"),
            "temps-test",
            "/app/dist",
        )
        .expect_err("traversal entry must fail");

        assert!(matches!(error, BuilderError::InvalidContext(_)));
        assert!(!temporary.path().join("outside").exists());
    }

    #[test]
    fn bounded_static_archive_rejects_links_and_special_entries() {
        for entry_type in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Fifo,
        ] {
            let temporary = TempDir::new().expect("temp dir");
            let archive_path = temporary.path().join("site.tar");
            std::fs::write(&archive_path, raw_tar_header(b"dist/unsafe", entry_type, 0))
                .expect("write archive");

            let error = extract_bounded_static_archive(
                &archive_path,
                &temporary.path().join("extracted"),
                "temps-test",
                "/app/dist",
            )
            .expect_err("non-regular entry must fail");

            assert!(matches!(error, BuilderError::InvalidContext(_)));
        }
    }

    #[test]
    fn bounded_static_archive_rejects_oversized_declared_entry() {
        let temporary = TempDir::new().expect("temp dir");
        let archive_path = temporary.path().join("site.tar");
        std::fs::write(
            &archive_path,
            raw_tar_header(
                b"dist/large.bin",
                tar::EntryType::Regular,
                MAX_STATIC_ENTRY_BYTES + 1,
            ),
        )
        .expect("write archive");

        let error = extract_bounded_static_archive(
            &archive_path,
            &temporary.path().join("extracted"),
            "temps-test",
            "/app/dist",
        )
        .expect_err("oversized entry must fail before reading content");

        assert!(matches!(error, BuilderError::ResourceLimitExceeded(_)));
    }

    #[test]
    fn static_archive_limits_reject_entry_count_and_total_size_without_allocating() {
        assert!(matches!(
            checked_static_archive_entry_count(MAX_STATIC_ENTRIES, "temps-test", "/app/dist"),
            Err(BuilderError::ResourceLimitExceeded(_))
        ));
        assert!(matches!(
            checked_static_archive_total(MAX_STATIC_TOTAL_BYTES, 1, "temps-test", "/app/dist"),
            Err(BuilderError::ResourceLimitExceeded(_))
        ));
    }

    #[test]
    fn static_archive_path_depth_accepts_exact_limit_and_rejects_next_component() {
        let exact = (0..MAX_STATIC_PATH_COMPONENTS).fold(PathBuf::new(), |mut path, _| {
            path.push("d");
            path
        });
        let mut over = exact.clone();
        over.push("asset.js");

        assert!(validate_static_archive_relative_depth(&exact, "temps-test", "/app/dist").is_ok());
        assert!(matches!(
            validate_static_archive_relative_depth(&over, "temps-test", "/app/dist"),
            Err(BuilderError::ResourceLimitExceeded(_))
        ));
    }

    #[tokio::test]
    async fn docker_archive_download_stream_stops_at_configured_limit() {
        let temporary = TempDir::new().expect("temp dir");
        let output_path = temporary.path().join("download.tar");
        let stream = futures::stream::iter(vec![
            Ok::<_, bollard::errors::Error>(bytes::Bytes::from_static(b"1234")),
            Ok::<_, bollard::errors::Error>(bytes::Bytes::from_static(b"5")),
        ]);

        let error = DockerRuntime::write_bounded_byte_stream(
            stream,
            &output_path,
            4,
            "temps-test",
            "/app/dist",
        )
        .await
        .expect_err("stream above limit must fail");

        assert!(matches!(error, BuilderError::ResourceLimitExceeded(_)));
        assert!(
            std::fs::metadata(output_path)
                .expect("partial file metadata")
                .len()
                <= 4,
            "download must never write the chunk that crosses the limit"
        );
    }

    #[tokio::test]
    async fn test_write_bounded_byte_stream_exact_limit_writes_complete_stream() {
        // Arrange
        let temporary = TempDir::new().expect("temp dir");
        let output_path = temporary.path().join("download.tar");
        let stream = futures::stream::iter(vec![
            Ok::<_, bollard::errors::Error>(bytes::Bytes::from_static(b"12")),
            Ok::<_, bollard::errors::Error>(bytes::Bytes::from_static(b"34")),
        ]);

        // Act
        let written = DockerRuntime::write_bounded_byte_stream(
            stream,
            &output_path,
            4,
            "temps-test",
            "/app/dist",
        )
        .await
        .expect("stream at exact limit should succeed");

        // Assert
        assert_eq!(written, 4);
        assert_eq!(
            std::fs::read(output_path).expect("download contents"),
            b"1234"
        );
    }
}
