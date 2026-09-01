// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use temps_core::static_files::{
    normalize_static_request_path, validate_static_artifact_path, validate_static_dir,
    StaticPathPolicyError, MAX_PUBLIC_STATIC_ASSET_BYTES,
};
use thiserror::Error;
use tokio::fs::{self, File};
use tokio::io::AsyncReadExt;

/// Each filesystem read is bounded independently of the deployed file size.
pub(crate) const STATIC_STREAM_CHUNK_BYTES: usize = 64 * 1024;
pub(crate) const STATIC_NOT_FOUND_BODY: &[u8] =
    b"<html><body><h1>404 - File Not Found</h1></body></html>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaticNotFoundContract {
    pub status: u16,
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub content_length: usize,
    pub send_body: bool,
}

pub(crate) fn static_not_found_contract(method: &str) -> StaticNotFoundContract {
    StaticNotFoundContract {
        status: 404,
        content_type: "text/html",
        cache_control: "no-store",
        content_length: STATIC_NOT_FOUND_BODY.len(),
        send_body: method != "HEAD",
    }
}

pub(crate) fn bounded_cas_etag(content_hash: &str, size_bytes: i64) -> Option<String> {
    if size_bytes < 0
        || size_bytes as u64 > MAX_PUBLIC_STATIC_ASSET_BYTES
        || content_hash.len() != 64
        || !content_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(format!("\"{}\"", content_hash.to_ascii_lowercase()))
}

pub(crate) fn opened_cas_size_matches(declared_size_bytes: i64, actual_size_bytes: u64) -> bool {
    declared_size_bytes >= 0
        && declared_size_bytes as u64 == actual_size_bytes
        && actual_size_bytes <= MAX_PUBLIC_STATIC_ASSET_BYTES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaticFileServeOutcome {
    Served,
    NotFound,
}

#[derive(Debug)]
pub(crate) struct OpenedStaticFile {
    pub file: File,
    pub canonical_path: PathBuf,
    pub metadata: Metadata,
}

#[derive(Debug, Error)]
pub(crate) enum StaticFileUnavailable {
    #[error("Static request path '{path}' was rejected: {reason}")]
    RequestPath {
        path: String,
        reason: StaticPathPolicyError,
    },
    #[error("Stored static directory '{path}' was rejected: {reason}")]
    StaticDirectory {
        path: String,
        reason: StaticPathPolicyError,
    },
    #[error("Stored static directory component '{path}' is a symbolic link")]
    SymlinkedStaticDirectory { path: String },
    #[error("Resolved static path '{path}' was rejected by the publication policy: {reason}")]
    ResolvedPath {
        path: String,
        reason: StaticPathPolicyError,
    },
    #[error("Static path '{path}' was not found while attempting to {operation}: {reason}")]
    NotFound {
        path: String,
        operation: &'static str,
        reason: std::io::Error,
    },
    #[error("Static path '{path}' is unavailable while attempting to {operation}: {reason}")]
    Unusable {
        path: String,
        operation: &'static str,
        reason: std::io::Error,
    },
    #[error(
        "Resolved static path '{path}' escapes deployment root '{deployment_root}' during {operation}"
    )]
    EscapesRoot {
        path: String,
        deployment_root: String,
        operation: &'static str,
    },
    #[error("Resolved static path '{path}' is not a regular file")]
    NotAFile { path: String },
}

impl StaticFileUnavailable {
    pub(crate) fn category(&self) -> &'static str {
        match self {
            Self::RequestPath { .. } => "request_path_rejected",
            Self::StaticDirectory { .. } => "static_directory_rejected",
            Self::SymlinkedStaticDirectory { .. } => "static_directory_symlink",
            Self::ResolvedPath { .. } => "resolved_path_rejected",
            Self::NotFound { .. } => "not_found",
            Self::Unusable { .. } => "unusable",
            Self::EscapesRoot { .. } => "escapes_root",
            Self::NotAFile { .. } => "not_a_file",
        }
    }
}

pub(crate) fn bounded_log_value(value: &str) -> &str {
    const MAX_BYTES: usize = 256;
    if value.len() <= MAX_BYTES {
        return value;
    }
    let mut end = MAX_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn unavailable_outcome(_error: &StaticFileUnavailable) -> StaticFileServeOutcome {
    StaticFileServeOutcome::NotFound
}

/// Resolve, contain, and open a request target without ever reading its body.
pub(crate) async fn open_static_file(
    configured_static_root: &Path,
    stored_static_dir: &str,
    raw_request_path: &str,
) -> Result<OpenedStaticFile, StaticFileUnavailable> {
    let relative_static_dir = validate_static_dir(stored_static_dir).map_err(|reason| {
        StaticFileUnavailable::StaticDirectory {
            path: bounded_log_value(stored_static_dir).to_owned(),
            reason,
        }
    })?;
    let relative_request_path =
        normalize_static_request_path(raw_request_path).map_err(|reason| {
            StaticFileUnavailable::RequestPath {
                path: bounded_log_value(raw_request_path).to_owned(),
                reason,
            }
        })?;

    let canonical_configured_root =
        canonicalize(configured_static_root, "canonicalize static root").await?;
    reject_symlinked_static_directory(&canonical_configured_root, &relative_static_dir).await?;
    let deployment_path = canonical_configured_root.join(relative_static_dir);
    let canonical_deployment_root =
        canonicalize(&deployment_path, "canonicalize deployment root").await?;
    ensure_contained(
        &canonical_deployment_root,
        &canonical_configured_root,
        &canonical_configured_root,
        "validate deployment root",
    )?;

    let request_candidate = if relative_request_path.as_os_str().is_empty() {
        canonical_deployment_root.join("index.html")
    } else {
        canonical_deployment_root.join(&relative_request_path)
    };

    let candidate = match fs::canonicalize(&request_candidate).await {
        Ok(candidate) => candidate,
        Err(reason)
            if reason.kind() == std::io::ErrorKind::NotFound
                && is_spa_route(&relative_request_path) =>
        {
            canonicalize(
                &canonical_deployment_root.join("index.html"),
                "resolve SPA fallback",
            )
            .await?
        }
        Err(reason) => {
            return Err(io_error(
                &request_candidate,
                "resolve requested file",
                reason,
            ));
        }
    };

    ensure_contained(
        &candidate,
        &canonical_deployment_root,
        &canonical_deployment_root,
        "validate requested path",
    )?;
    let candidate_metadata = fs::metadata(&candidate)
        .await
        .map_err(|reason| io_error(&candidate, "inspect requested path", reason))?;
    let final_candidate = if candidate_metadata.is_dir() {
        candidate.join("index.html")
    } else {
        candidate
    };

    // This is intentionally the last pathname operation before File::open.
    // It catches directory-index symlinks as well as ordinary file symlinks.
    let canonical_path = canonicalize(&final_candidate, "resolve final file").await?;
    ensure_contained(
        &canonical_path,
        &canonical_deployment_root,
        &canonical_deployment_root,
        "validate final file",
    )?;
    let canonical_relative_path = canonical_path
        .strip_prefix(&canonical_deployment_root)
        .map_err(|_| StaticFileUnavailable::EscapesRoot {
            path: canonical_path.display().to_string(),
            deployment_root: canonical_deployment_root.display().to_string(),
            operation: "validate canonical publication path",
        })?;
    validate_static_artifact_path(canonical_relative_path).map_err(|reason| {
        StaticFileUnavailable::ResolvedPath {
            path: canonical_relative_path.display().to_string(),
            reason,
        }
    })?;
    let file = File::open(&canonical_path)
        .await
        .map_err(|reason| io_error(&canonical_path, "open final file", reason))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|reason| io_error(&canonical_path, "read opened file metadata", reason))?;
    if !metadata.is_file() {
        return Err(StaticFileUnavailable::NotAFile {
            path: canonical_path.display().to_string(),
        });
    }

    Ok(OpenedStaticFile {
        file,
        canonical_path,
        metadata,
    })
}

/// Reject every stored-directory symlink component before accepting its target.
///
/// The configured root itself may intentionally be a symlink, so the walk starts
/// from its canonical target. Components below it come from stored deployment
/// state and must preserve their deployment identity instead of aliasing another
/// tenant or an in-root sensitive directory.
async fn reject_symlinked_static_directory(
    canonical_configured_root: &Path,
    relative_static_dir: &Path,
) -> Result<(), StaticFileUnavailable> {
    let mut current = canonical_configured_root.to_path_buf();
    for component in relative_static_dir.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .await
            .map_err(|reason| io_error(&current, "inspect static directory component", reason))?;
        if metadata.file_type().is_symlink() {
            return Err(StaticFileUnavailable::SymlinkedStaticDirectory {
                path: current.display().to_string(),
            });
        }
    }
    Ok(())
}

/// Build a weak validator from immutable deployment identity and file metadata.
/// No content bytes are read, so conditional requests can complete before body IO.
pub(crate) fn metadata_etag(path: &Path, metadata: &Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(modified.as_secs().to_le_bytes());
    digest.update(modified.subsec_nanos().to_le_bytes());
    let encoded = hex::encode(digest.finalize());
    format!("W/\"{}\"", &encoded[..32])
}

pub(crate) fn if_none_match_matches(value: &str, etag: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate == etag)
}

/// Read one bounded response chunk. An empty result marks EOF.
pub(crate) async fn read_static_chunk<R>(file: &mut R) -> std::io::Result<Bytes>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    let mut chunk = vec![0_u8; STATIC_STREAM_CHUNK_BYTES];
    let bytes_read = file.read(&mut chunk).await?;
    chunk.truncate(bytes_read);
    Ok(Bytes::from(chunk))
}

/// Truncate a body chunk to the opened file length and return bytes consumed.
/// This prevents a file that grows after metadata inspection from extending a
/// response beyond its advertised and security-checked length.
pub(crate) fn cap_static_chunk(chunk: &mut Bytes, remaining: u64) -> u64 {
    if chunk.len() as u64 > remaining {
        chunk.truncate(remaining as usize);
    }
    chunk.len() as u64
}

fn is_spa_route(relative_request_path: &Path) -> bool {
    relative_request_path.as_os_str().is_empty() || relative_request_path.extension().is_none()
}

async fn canonicalize(
    path: &Path,
    operation: &'static str,
) -> Result<PathBuf, StaticFileUnavailable> {
    fs::canonicalize(path)
        .await
        .map_err(|reason| io_error(path, operation, reason))
}

fn io_error(path: &Path, operation: &'static str, reason: std::io::Error) -> StaticFileUnavailable {
    if reason.kind() == std::io::ErrorKind::NotFound {
        StaticFileUnavailable::NotFound {
            path: path.display().to_string(),
            operation,
            reason,
        }
    } else {
        StaticFileUnavailable::Unusable {
            path: path.display().to_string(),
            operation,
            reason,
        }
    }
}

fn ensure_contained(
    path: &Path,
    root: &Path,
    deployment_root: &Path,
    operation: &'static str,
) -> Result<(), StaticFileUnavailable> {
    if path.starts_with(root) {
        return Ok(());
    }
    Err(StaticFileUnavailable::EscapesRoot {
        path: path.display().to_string(),
        deployment_root: deployment_root.display().to_string(),
        operation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use temp_dir::TempDir;
    use tokio::io::AsyncWriteExt;

    async fn deployment() -> (TempDir, PathBuf) {
        let root = TempDir::new().expect("temporary static root");
        let deployment = root.path().join("projects/site/production/deploy-1");
        fs::create_dir_all(deployment.join("docs"))
            .await
            .expect("create static deployment");
        fs::write(deployment.join("index.html"), b"spa")
            .await
            .expect("write root index");
        fs::write(deployment.join("docs/index.html"), b"docs")
            .await
            .expect("write directory index");
        fs::write(deployment.join("app.js"), b"javascript")
            .await
            .expect("write asset");
        (root, deployment)
    }

    const STORED_DIR: &str = "projects/site/production/deploy-1";

    #[tokio::test]
    async fn opens_assets_directories_and_legitimate_spa_fallbacks() {
        let (root, _) = deployment().await;
        for (request, suffix) in [
            ("/app.js", "app.js"),
            ("/docs/", "docs/index.html"),
            ("/account/settings", "index.html"),
            ("/user.name/settings", "index.html"),
            ("/", "index.html"),
        ] {
            let opened = open_static_file(root.path(), STORED_DIR, request)
                .await
                .expect("valid static request");
            assert!(opened.canonical_path.ends_with(suffix), "{request}");
        }
    }

    #[tokio::test]
    async fn rejects_raw_single_and_double_encoded_sensitive_or_traversal_paths() {
        let (root, _) = deployment().await;
        for request in [
            "/../secret",
            "/%2e%2e/secret",
            "/%252e%252e/secret",
            "/.git/config",
            "/%2egit/config",
            "/%252egit/config",
        ] {
            let error = open_static_file(root.path(), STORED_DIR, request)
                .await
                .expect_err("unsafe path must be rejected");
            assert_eq!(
                unavailable_outcome(&error),
                StaticFileServeOutcome::NotFound,
                "{request}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn test_open_static_file_well_known_path_opens_requested_file() {
        // Arrange
        let (root, deployment) = deployment().await;
        let well_known = deployment.join(".well-known");
        fs::create_dir_all(&well_known)
            .await
            .expect("create well-known directory");
        fs::write(
            well_known.join("security.txt"),
            b"Contact: mailto:test@example.test",
        )
        .await
        .expect("write security.txt");

        // Act
        let opened = open_static_file(root.path(), STORED_DIR, "/.well-known/security.txt")
            .await
            .expect("documented well-known file should be publishable");

        // Assert
        assert!(opened.canonical_path.ends_with(".well-known/security.txt"));
    }

    #[tokio::test]
    async fn missing_invalid_sensitive_and_unusable_paths_share_the_not_found_contract() {
        let (root, _) = deployment().await;
        let failures = [
            open_static_file(root.path(), STORED_DIR, "/missing.js")
                .await
                .expect_err("asset is absent"),
            open_static_file(root.path(), "../escape", "/app.js")
                .await
                .expect_err("stored path is invalid"),
            open_static_file(root.path(), STORED_DIR, "/.env")
                .await
                .expect_err("sensitive path is invalid"),
        ];
        let expected_contract = static_not_found_contract("GET");

        for failure in failures {
            assert_eq!(
                unavailable_outcome(&failure),
                StaticFileServeOutcome::NotFound,
                "{failure}"
            );
            assert_eq!(static_not_found_contract("GET"), expected_contract);
        }

        let unusable_root = root.path().join("not-a-directory");
        fs::write(&unusable_root, b"ordinary file")
            .await
            .expect("write unusable static root fixture");
        let unusable = open_static_file(&unusable_root, STORED_DIR, "/app.js")
            .await
            .expect_err("non-directory static root is unusable");
        assert!(matches!(unusable, StaticFileUnavailable::Unusable { .. }));
        assert_eq!(
            unavailable_outcome(&unusable),
            StaticFileServeOutcome::NotFound
        );
        assert_eq!(static_not_found_contract("GET"), expected_contract);
    }

    #[tokio::test]
    async fn oversized_paths_are_not_retained_in_resolution_errors() {
        let oversized = "a".repeat(8 * 1024);
        let stored_error = open_static_file(Path::new("unused"), &oversized, "/")
            .await
            .expect_err("oversized stored directory must fail before filesystem access");
        let request_error = open_static_file(Path::new("unused"), STORED_DIR, &oversized)
            .await
            .expect_err("oversized request must fail before filesystem access");

        assert!(matches!(
            stored_error,
            StaticFileUnavailable::StaticDirectory { path, .. } if path.len() <= 256
        ));
        assert!(matches!(
            request_error,
            StaticFileUnavailable::RequestPath { path, .. } if path.len() <= 256
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn final_directory_index_symlink_cannot_escape_the_deployment() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let outside = root.path().join("private.html");
        fs::write(&outside, b"private")
            .await
            .expect("write outside file");
        let linked_dir = deployment.join("linked");
        fs::create_dir_all(&linked_dir)
            .await
            .expect("create linked directory");
        symlink(&outside, linked_dir.join("index.html")).expect("create index symlink");

        let error = open_static_file(root.path(), STORED_DIR, "/linked/")
            .await
            .expect_err("escaping index symlink must fail");
        assert!(matches!(error, StaticFileUnavailable::EscapesRoot { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_open_static_file_file_symlink_escaping_root_returns_unavailable() {
        use std::os::unix::fs::symlink;

        // Arrange
        let (root, deployment) = deployment().await;
        let outside = root.path().join("private.txt");
        fs::write(&outside, b"private")
            .await
            .expect("write outside file");
        symlink(&outside, deployment.join("linked.txt")).expect("create file symlink");

        // Act
        let error = open_static_file(root.path(), STORED_DIR, "/linked.txt")
            .await
            .expect_err("escaping file symlink must fail");

        // Assert
        assert!(matches!(error, StaticFileUnavailable::EscapesRoot { .. }));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_root_symlink_cannot_alias_in_root_sensitive_directory() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().expect("temporary static root");
        let sensitive = root.path().join(".git");
        fs::create_dir_all(&sensitive)
            .await
            .expect("create sensitive directory");
        fs::write(sensitive.join("index.html"), b"private")
            .await
            .expect("write private index");
        let deployment_parent = root.path().join("projects/site/production");
        fs::create_dir_all(&deployment_parent)
            .await
            .expect("create deployment parent");
        symlink(&sensitive, deployment_parent.join("deploy-1"))
            .expect("create sensitive deployment alias");

        let error = open_static_file(root.path(), STORED_DIR, "/")
            .await
            .expect_err("deployment root must not alias a sensitive directory");

        assert!(matches!(
            error,
            StaticFileUnavailable::SymlinkedStaticDirectory { .. }
        ));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_root_symlink_cannot_alias_another_deployment() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().expect("temporary static root");
        let other_deployment = root.path().join("projects/other/production/deploy-2");
        fs::create_dir_all(&other_deployment)
            .await
            .expect("create other deployment");
        fs::write(other_deployment.join("index.html"), b"other tenant")
            .await
            .expect("write other deployment index");
        let deployment_parent = root.path().join("projects/site/production");
        fs::create_dir_all(&deployment_parent)
            .await
            .expect("create deployment parent");
        symlink(&other_deployment, deployment_parent.join("deploy-1"))
            .expect("create cross-deployment alias");

        let error = open_static_file(root.path(), STORED_DIR, "/")
            .await
            .expect_err("deployment root must not alias another deployment");

        assert!(matches!(
            error,
            StaticFileUnavailable::SymlinkedStaticDirectory { .. }
        ));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn in_root_file_symlink_alias_cannot_publish_sensitive_target() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let sensitive = deployment.join(".env");
        fs::write(&sensitive, b"SECRET=test")
            .await
            .expect("write sensitive target");
        symlink(&sensitive, deployment.join("public.txt")).expect("create public alias");

        let error = open_static_file(root.path(), STORED_DIR, "/public.txt")
            .await
            .expect_err("canonical sensitive target must be rejected");

        assert!(matches!(error, StaticFileUnavailable::ResolvedPath { .. }));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn in_root_directory_symlink_alias_cannot_publish_sensitive_child() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let sensitive_directory = deployment.join(".git");
        fs::create_dir_all(&sensitive_directory)
            .await
            .expect("create sensitive directory");
        fs::write(
            sensitive_directory.join("config"),
            b"private repository config",
        )
        .await
        .expect("write sensitive child");
        symlink(&sensitive_directory, deployment.join("public")).expect("create directory alias");

        let error = open_static_file(root.path(), STORED_DIR, "/public/config")
            .await
            .expect_err("canonical sensitive child must be rejected");

        assert!(matches!(error, StaticFileUnavailable::ResolvedPath { .. }));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_index_symlink_alias_cannot_publish_sensitive_target() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let sensitive = deployment.join(".env");
        fs::write(&sensitive, b"SECRET=test")
            .await
            .expect("write sensitive target");
        let index = deployment.join("docs/index.html");
        fs::remove_file(&index)
            .await
            .expect("remove ordinary directory index");
        symlink(&sensitive, &index).expect("create directory index alias");

        let error = open_static_file(root.path(), STORED_DIR, "/docs/")
            .await
            .expect_err("canonical sensitive index target must be rejected");

        assert!(matches!(error, StaticFileUnavailable::ResolvedPath { .. }));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_open_static_file_deployment_root_symlink_escaping_static_root_returns_unavailable(
    ) {
        use std::os::unix::fs::symlink;

        // Arrange
        let root = TempDir::new().expect("temporary static root");
        let outside = TempDir::new().expect("outside deployment root");
        fs::write(outside.path().join("index.html"), b"private")
            .await
            .expect("write outside index");
        let deployment_parent = root.path().join("projects/site/production");
        fs::create_dir_all(&deployment_parent)
            .await
            .expect("create deployment parent");
        symlink(outside.path(), deployment_parent.join("deploy-1"))
            .expect("create deployment root symlink");

        // Act
        let error = open_static_file(root.path(), STORED_DIR, "/")
            .await
            .expect_err("deployment root symlink must remain confined");

        // Assert
        assert!(matches!(
            error,
            StaticFileUnavailable::SymlinkedStaticDirectory { .. }
        ));
        assert_eq!(
            unavailable_outcome(&error),
            StaticFileServeOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn etag_is_available_without_reading_and_is_stable_for_opened_metadata() {
        let (root, _) = deployment().await;
        let opened = open_static_file(root.path(), STORED_DIR, "/app.js")
            .await
            .expect("open asset");
        let first = metadata_etag(&opened.canonical_path, &opened.metadata);
        let second = metadata_etag(&opened.canonical_path, &opened.metadata);
        let other_deployment = metadata_etag(Path::new("/other/deploy/app.js"), &opened.metadata);
        assert_eq!(first, second);
        assert_ne!(first, other_deployment);
        assert!(first.starts_with("W/\""));
    }

    #[test]
    fn if_none_match_supports_lists_and_wildcards() {
        let etag = "W/\"abc\"";
        assert!(if_none_match_matches(etag, etag));
        assert!(if_none_match_matches("\"old\", W/\"abc\"", etag));
        assert!(if_none_match_matches("*", etag));
        assert!(!if_none_match_matches("\"old\"", etag));
    }

    #[test]
    fn cas_policy_rejects_invalid_or_oversized_metadata_before_blob_io() {
        let hash = "a".repeat(64);
        assert!(bounded_cas_etag(&hash, 0).is_some());
        assert!(bounded_cas_etag(&hash, MAX_PUBLIC_STATIC_ASSET_BYTES as i64).is_some());
        assert!(bounded_cas_etag(&hash, -1).is_none());
        assert!(bounded_cas_etag(&hash, MAX_PUBLIC_STATIC_ASSET_BYTES as i64 + 1).is_none());
        assert!(bounded_cas_etag("short", 1).is_none());
        assert!(bounded_cas_etag(&"z".repeat(64), 1).is_none());
        assert!(opened_cas_size_matches(1024, 1024));
        assert!(!opened_cas_size_matches(1024, 1025));
        assert!(!opened_cas_size_matches(-1, 0));
        assert!(!opened_cas_size_matches(
            MAX_PUBLIC_STATIC_ASSET_BYTES as i64 + 1,
            MAX_PUBLIC_STATIC_ASSET_BYTES + 1
        ));
    }

    #[test]
    fn all_static_resolution_failures_share_one_404_contract() {
        let get = static_not_found_contract("GET");
        let head = static_not_found_contract("HEAD");
        assert_eq!(get.status, 404);
        assert_eq!(get.content_type, "text/html");
        assert_eq!(get.cache_control, "no-store");
        assert_eq!(get.content_length, STATIC_NOT_FOUND_BODY.len());
        assert!(get.send_body);
        assert_eq!(head.status, get.status);
        assert_eq!(head.content_type, get.content_type);
        assert_eq!(head.cache_control, get.cache_control);
        assert_eq!(head.content_length, get.content_length);
        assert!(!head.send_body);
    }

    #[test]
    fn attacker_controlled_log_values_are_utf8_safely_bounded() {
        let ascii = "a".repeat(400);
        assert_eq!(bounded_log_value(&ascii).len(), 256);

        let unicode = "é".repeat(200);
        let bounded = bounded_log_value(&unicode);
        assert!(bounded.len() <= 256);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[tokio::test]
    async fn large_files_are_read_in_fixed_size_chunks() {
        let (root, deployment) = deployment().await;
        let large_path = deployment.join("large.bin");
        let total_size = STATIC_STREAM_CHUNK_BYTES * 3 + 17;
        let mut writer = File::create(&large_path).await.expect("create large file");
        for _ in 0..3 {
            writer
                .write_all(&vec![7_u8; STATIC_STREAM_CHUNK_BYTES])
                .await
                .expect("write full chunk");
        }
        writer.write_all(&[9_u8; 17]).await.expect("write tail");
        writer.flush().await.expect("flush large file");
        drop(writer);

        let mut opened = open_static_file(root.path(), STORED_DIR, "/large.bin")
            .await
            .expect("open large file");
        let mut streamed = 0;
        loop {
            let chunk = read_static_chunk(&mut opened.file)
                .await
                .expect("read bounded chunk");
            if chunk.is_empty() {
                break;
            }
            assert!(chunk.len() <= STATIC_STREAM_CHUNK_BYTES);
            streamed += chunk.len();
        }
        assert_eq!(streamed, total_size);
    }

    #[test]
    fn opened_length_caps_chunks_from_a_growing_file() {
        let mut chunk = Bytes::from_static(b"original-plus-growth");

        let consumed = cap_static_chunk(&mut chunk, 8);

        assert_eq!(consumed, 8);
        assert_eq!(chunk, Bytes::from_static(b"original"));
    }

    #[tokio::test]
    async fn cas_blob_get_path_uses_opened_size_and_fixed_chunks() {
        use temps_file_store::fs_store::FsFileStore;
        use temps_file_store::FileStore;

        let root = TempDir::new().expect("temporary CAS root");
        let store = FsFileStore::new(root.path().join("cas"));
        let total_size = STATIC_STREAM_CHUNK_BYTES * 3 + 17;
        let data = Bytes::from(vec![7_u8; total_size]);
        let hash = store.put_blob(data).await.expect("persist CAS fixture");

        let mut opened = store.open_blob(&hash).await.expect("open CAS fixture");
        assert!(opened_cas_size_matches(
            total_size as i64,
            opened.size_bytes
        ));

        let mut streamed = 0;
        loop {
            let chunk = read_static_chunk(opened.reader.as_mut())
                .await
                .expect("read bounded CAS chunk");
            if chunk.is_empty() {
                break;
            }
            assert!(chunk.len() <= STATIC_STREAM_CHUNK_BYTES);
            streamed += chunk.len();
        }
        assert_eq!(streamed, total_size);
    }
}
