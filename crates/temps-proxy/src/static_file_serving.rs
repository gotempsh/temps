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
    /// The deployment's own `404.html` was served with status 404.
    ServedNotFoundPage,
    /// A trailing-slash request was redirected to its slashless `.html` page.
    Redirected,
    NotFound,
}

/// Top-level page a deployment ships to answer unknown paths. Its presence
/// switches unknown extensionless paths from the SPA shell (200) to a real 404,
/// matching the implicit rule of Cloudflare Pages and Netlify.
pub(crate) const STATIC_NOT_FOUND_PAGE: &str = "404.html";

/// What a resolved candidate stands for, which decides the response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaticFileMatch {
    /// The requested file, its directory `index.html`, or its `.html` page.
    Requested,
    /// The root `index.html`, served with 200 for an unknown extensionless
    /// path of a deployment that ships no `404.html` (client-side routing).
    SpaShell,
    /// The deployment's top-level `404.html`, served with 404.
    NotFoundPage,
    /// `<path>.html` exists for a trailing-slash request (`/about/`). The
    /// client is redirected to the slashless URL (`/about`) with 308 rather
    /// than served the page, whose relative links would otherwise resolve one
    /// directory too deep. This is what Cloudflare Pages does.
    CanonicalRedirect,
}

impl StaticFileMatch {
    pub(crate) fn status(self) -> u16 {
        match self {
            Self::Requested | Self::SpaShell => 200,
            Self::CanonicalRedirect => 308,
            Self::NotFoundPage => 404,
        }
    }
}

/// `Location` for a [`StaticFileMatch::CanonicalRedirect`]: the raw request
/// path without its trailing slash, plus the original query string.
///
/// Returns `None` unless the result is a plain same-origin path. The request
/// path has already passed `normalize_static_request_path`, which rejects
/// `//host` and `\` forms, so this is a second line of defence against
/// building a protocol-relative (open) redirect.
pub(crate) fn canonical_redirect_location(
    raw_request_path: &str,
    query_string: Option<&str>,
) -> Option<String> {
    let path = raw_request_path.strip_suffix('/')?;
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return None;
    }
    Some(match query_string.filter(|query| !query.is_empty()) {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaticCandidate {
    /// Path relative to the deployment root.
    pub path: PathBuf,
    pub matched: StaticFileMatch,
}

#[derive(Debug)]
pub(crate) struct OpenedStaticFile {
    pub file: File,
    pub canonical_path: PathBuf,
    pub metadata: Metadata,
    pub matched: StaticFileMatch,
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

    // Only a missing candidate moves on to the next one. Any other failure
    // (permission denied, symlink loop, a symlink escaping the deployment)
    // propagates as-is rather than being masked by a fallback.
    let candidates = static_request_candidates(
        &relative_request_path,
        is_directory_request(raw_request_path),
    );
    for candidate in candidates {
        if let Some(canonical_path) =
            resolve_file_candidate(&canonical_deployment_root, &candidate.path).await?
        {
            return open_resolved_file(
                canonical_path,
                &canonical_deployment_root,
                candidate.matched,
            )
            .await;
        }
    }

    let requested = canonical_deployment_root.join(&relative_request_path);
    Err(StaticFileUnavailable::NotFound {
        path: requested.display().to_string(),
        operation: "resolve requested file",
        reason: std::io::ErrorKind::NotFound.into(),
    })
}

/// Canonicalize one candidate under the deployment root. Returns `None` when it
/// does not exist or is a directory (its `index.html` is a separate candidate).
///
/// The canonicalization here is intentionally the last pathname operation
/// before `File::open`, so it catches directory-index symlinks as well as
/// ordinary file symlinks.
async fn resolve_file_candidate(
    canonical_deployment_root: &Path,
    candidate: &Path,
) -> Result<Option<PathBuf>, StaticFileUnavailable> {
    let joined = canonical_deployment_root.join(candidate);
    let canonical = match fs::canonicalize(&joined).await {
        Ok(canonical) => canonical,
        Err(reason) if is_missing(&reason) => return Ok(None),
        Err(reason) => return Err(io_error(&joined, "resolve requested file", reason)),
    };
    ensure_contained(
        &canonical,
        canonical_deployment_root,
        canonical_deployment_root,
        "validate requested path",
    )?;
    let metadata = fs::metadata(&canonical)
        .await
        .map_err(|reason| io_error(&canonical, "inspect requested path", reason))?;
    if metadata.is_dir() {
        return Ok(None);
    }
    Ok(Some(canonical))
}

/// `NotADirectory` covers a request that treats a file as a directory
/// (`/app.js/extra`): nothing can exist there, exactly like a missing path.
fn is_missing(reason: &std::io::Error) -> bool {
    matches!(
        reason.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

async fn open_resolved_file(
    canonical_path: PathBuf,
    canonical_deployment_root: &Path,
    matched: StaticFileMatch,
) -> Result<OpenedStaticFile, StaticFileUnavailable> {
    ensure_contained(
        &canonical_path,
        canonical_deployment_root,
        canonical_deployment_root,
        "validate final file",
    )?;
    let canonical_relative_path = canonical_path
        .strip_prefix(canonical_deployment_root)
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
        matched,
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

/// Ordered plan for resolving a static-site request against an
/// object-store-backed deployment (S3-compatible `TEMPS_STATIC_STORAGE_BACKEND=s3`),
/// mirroring [`open_static_file`]'s observable resolution order without any of
/// its filesystem-specific machinery (canonicalization, symlink rejection —
/// neither concept exists for object storage; path-traversal and
/// sensitive-path protection is unaffected, since it happens in
/// `validate_static_dir`/`normalize_static_request_path` before either
/// backend ever sees a key).
///
/// Both backends try the candidates from [`static_request_candidates`] in
/// order and serve the first one found. An object store has no notion of "is a
/// directory", so a key that names a directory simply misses and the next
/// candidate (its `index.html`) is tried — the same observable result as the
/// disk path.
pub(crate) struct StaticObjectRequest {
    pub relative_static_dir: PathBuf,
    /// Candidate object keys (relative to `relative_static_dir`), most
    /// specific first.
    pub candidates: Vec<StaticCandidate>,
}

pub(crate) fn resolve_static_object_request(
    stored_static_dir: &str,
    raw_request_path: &str,
) -> Result<StaticObjectRequest, StaticFileUnavailable> {
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

    Ok(StaticObjectRequest {
        relative_static_dir,
        candidates: static_request_candidates(
            &relative_request_path,
            is_directory_request(raw_request_path),
        ),
    })
}

/// Ordered candidates for a normalized request path, shared by the disk and
/// object-store backends:
///
/// 1. the requested path itself (`index.html` for `/`);
/// 2. `<path>/index.html`, for any path — a directory may have a dot in its
///    name (`/releases/v1.2/`);
/// 3. for an extensionless path, `<path>.html` — the page layout static site
///    generators emit with `trailingSlash: false` (Next.js export),
///    `build.format: "file"` (Astro) or `uglyURLs` (Hugo). For a
///    trailing-slash request it is a [`StaticFileMatch::CanonicalRedirect`]
///    to the slashless URL instead of the page itself;
/// 4. the deployment's top-level `404.html`, served with status 404;
/// 5. for an extensionless path, the root `index.html` as the SPA shell.
///
/// Because `404.html` precedes the SPA shell, a deployment that ships one gets
/// real 404s for unknown paths, while a pure SPA keeps client-side routing.
/// Requesting the 404 page itself (`/404`, `/404.html`) also answers 404, so
/// it never becomes an indexable 200 page.
fn static_request_candidates(
    relative_request_path: &Path,
    directory_request: bool,
) -> Vec<StaticCandidate> {
    let not_found_page = Path::new(STATIC_NOT_FOUND_PAGE);
    let requested = |path: PathBuf| {
        let matched = if path == not_found_page {
            StaticFileMatch::NotFoundPage
        } else {
            StaticFileMatch::Requested
        };
        StaticCandidate { path, matched }
    };
    let is_root = relative_request_path.as_os_str().is_empty();
    let is_spa_route = is_spa_route(relative_request_path);
    let mut candidates = Vec::with_capacity(5);
    if is_root {
        candidates.push(requested(PathBuf::from("index.html")));
    } else {
        candidates.push(requested(relative_request_path.to_path_buf()));
        candidates.push(requested(relative_request_path.join("index.html")));
        if is_spa_route {
            let mut html_page = relative_request_path.as_os_str().to_owned();
            html_page.push(".html");
            let html_page = requested(PathBuf::from(html_page));
            candidates.push(
                if directory_request && html_page.matched == StaticFileMatch::Requested {
                    StaticCandidate {
                        matched: StaticFileMatch::CanonicalRedirect,
                        ..html_page
                    }
                } else {
                    html_page
                },
            );
        }
    }
    if !candidates
        .iter()
        .any(|candidate| candidate.path == not_found_page)
    {
        candidates.push(StaticCandidate {
            path: not_found_page.to_path_buf(),
            matched: StaticFileMatch::NotFoundPage,
        });
    }
    if !is_root && is_spa_route {
        candidates.push(StaticCandidate {
            path: PathBuf::from("index.html"),
            matched: StaticFileMatch::SpaShell,
        });
    }
    candidates
}

/// Whether the client asked for a directory (`/about/`). Normalization drops
/// the trailing slash, so this is read from the raw request path.
fn is_directory_request(raw_request_path: &str) -> bool {
    raw_request_path.len() > 1 && raw_request_path.ends_with('/')
}

/// Build the object-store key for one candidate under a validated static
/// directory. Always POSIX-style (`/`-joined): both `relative_static_dir` and
/// `candidate` were built from validated components that never contain `\`
/// (rejected by `validate_static_dir`/`normalize_static_request_path`), so
/// this is safe even if this process ever ran on a non-Unix host.
pub(crate) fn static_object_key(relative_static_dir: &Path, candidate: &Path) -> String {
    format!("{}/{}", relative_static_dir.display(), candidate.display())
}

/// Build a weak validator for object-store-backed static content from its key
/// and size. There is no mtime to fold in (no local filesystem), but none is
/// needed: every object-store key is written exactly once by
/// `StaticDeployer::deploy` under a fresh, date-partitioned deployment slug
/// (see `temps_deployer::static_deployer::storage_relative_path`), so `(key,
/// size)` already uniquely identifies one immutable version of one file.
pub(crate) fn object_etag(key: &str, size_bytes: u64) -> String {
    let mut digest = Sha256::new();
    digest.update(key.as_bytes());
    digest.update(size_bytes.to_le_bytes());
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
    async fn falls_back_to_root_index_for_a_real_directory_with_no_index_of_its_own() {
        let (root, deployment) = deployment().await;
        let assets_only_dir = deployment.join("guide");
        fs::create_dir_all(&assets_only_dir)
            .await
            .expect("create asset-only directory");
        fs::write(assets_only_dir.join("screenshot.png"), b"png")
            .await
            .expect("write asset inside directory");

        // The bare directory path (no index.html inside it) must still resolve
        // to the SPA shell — this is a client-side route that happens to share
        // its first path segment with an asset directory in the build output.
        for request in ["/guide", "/guide/"] {
            let opened = open_static_file(root.path(), STORED_DIR, request)
                .await
                .unwrap_or_else(|error| panic!("{request} should fall back to SPA shell: {error}"));
            assert!(
                opened.canonical_path.ends_with("index.html")
                    && !opened.canonical_path.ends_with("guide/index.html"),
                "{request} resolved to {:?}, expected the deployment root index.html",
                opened.canonical_path
            );
        }

        // A real file inside that same directory must still resolve normally.
        let asset = open_static_file(root.path(), STORED_DIR, "/guide/screenshot.png")
            .await
            .expect("existing asset inside the directory should still open");
        assert!(asset.canonical_path.ends_with("guide/screenshot.png"));
    }

    async fn open_matched(root: &TempDir, request: &str) -> (String, StaticFileMatch) {
        let mut opened = open_static_file(root.path(), STORED_DIR, request)
            .await
            .unwrap_or_else(|error| panic!("{request} should resolve: {error}"));
        let mut body = String::new();
        opened
            .file
            .read_to_string(&mut body)
            .await
            .expect("read resolved file");
        (body, opened.matched)
    }

    #[tokio::test]
    async fn spa_without_404_page_serves_the_shell_for_unknown_extensionless_paths() {
        let (root, _) = deployment().await;

        assert_eq!(
            open_matched(&root, "/does-not-exist").await,
            ("spa".to_owned(), StaticFileMatch::SpaShell)
        );
        assert_eq!(
            open_matched(&root, "/docs/").await,
            ("docs".to_owned(), StaticFileMatch::Requested)
        );
    }

    #[tokio::test]
    async fn deployment_with_404_page_serves_it_for_unknown_paths_instead_of_the_shell() {
        let (root, deployment) = deployment().await;
        fs::write(deployment.join("404.html"), b"not found page")
            .await
            .expect("write 404 page");

        for request in ["/does-not-exist", "/nested/missing/", "/missing.js"] {
            assert_eq!(
                open_matched(&root, request).await,
                ("not found page".to_owned(), StaticFileMatch::NotFoundPage),
                "{request}"
            );
        }

        // Real pages are unaffected by the presence of a 404 page.
        assert_eq!(
            open_matched(&root, "/").await,
            ("spa".to_owned(), StaticFileMatch::Requested)
        );
        assert_eq!(
            open_matched(&root, "/docs").await,
            ("docs".to_owned(), StaticFileMatch::Requested)
        );
    }

    #[tokio::test]
    async fn extensionless_path_resolves_to_its_html_page() {
        let (root, deployment) = deployment().await;
        fs::write(deployment.join("404.html"), b"not found page")
            .await
            .expect("write 404 page");
        fs::write(deployment.join("about.html"), b"about")
            .await
            .expect("write html page");
        // `trailingSlash: false` exports emit `blog.html` next to a `blog/`
        // directory holding the nested pages, with no `blog/index.html`.
        fs::create_dir_all(deployment.join("blog"))
            .await
            .expect("create nested page directory");
        fs::write(deployment.join("blog.html"), b"blog")
            .await
            .expect("write section page");
        fs::write(deployment.join("blog/first-post.html"), b"first post")
            .await
            .expect("write nested page");

        for (request, body) in [
            ("/about", "about"),
            ("/blog", "blog"),
            ("/blog/first-post", "first post"),
        ] {
            assert_eq!(
                open_matched(&root, request).await,
                (body.to_owned(), StaticFileMatch::Requested),
                "{request}"
            );
        }

        // `/about/` is a directory request: serving `about.html` there would
        // resolve its relative links one level too deep, so it redirects.
        assert_eq!(
            open_matched(&root, "/about/").await,
            ("about".to_owned(), StaticFileMatch::CanonicalRedirect)
        );
        // A real directory index still wins over the redirect.
        assert_eq!(
            open_matched(&root, "/docs/").await,
            ("docs".to_owned(), StaticFileMatch::Requested)
        );
    }

    #[tokio::test]
    async fn dotted_directory_serves_its_own_index() {
        let (root, deployment) = deployment().await;
        fs::create_dir_all(deployment.join("releases/v1.2"))
            .await
            .expect("create dotted directory");
        fs::write(
            deployment.join("releases/v1.2/index.html"),
            b"release notes",
        )
        .await
        .expect("write dotted directory index");

        for request in ["/releases/v1.2", "/releases/v1.2/"] {
            assert_eq!(
                open_matched(&root, request).await,
                ("release notes".to_owned(), StaticFileMatch::Requested),
                "{request}"
            );
        }
    }

    #[tokio::test]
    async fn requesting_the_404_page_directly_still_answers_404() {
        let (root, deployment) = deployment().await;
        fs::write(deployment.join("404.html"), b"not found page")
            .await
            .expect("write 404 page");

        for request in ["/404", "/404.html"] {
            assert_eq!(
                open_matched(&root, request).await,
                ("not found page".to_owned(), StaticFileMatch::NotFoundPage),
                "{request}"
            );
        }
    }

    #[tokio::test]
    async fn path_through_a_file_is_treated_as_missing() {
        let (root, deployment) = deployment().await;

        assert_eq!(
            open_matched(&root, "/app.js/extra").await,
            ("spa".to_owned(), StaticFileMatch::SpaShell)
        );

        fs::write(deployment.join("404.html"), b"not found page")
            .await
            .expect("write 404 page");
        assert_eq!(
            open_matched(&root, "/app.js/extra").await,
            ("not found page".to_owned(), StaticFileMatch::NotFoundPage)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn not_found_page_symlink_cannot_escape_the_deployment() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let outside = root.path().join("private.html");
        fs::write(&outside, b"private")
            .await
            .expect("write outside file");
        symlink(&outside, deployment.join("404.html")).expect("create 404 page symlink");

        let error = open_static_file(root.path(), STORED_DIR, "/does-not-exist")
            .await
            .expect_err("escaping 404 page symlink must fail");
        assert!(matches!(error, StaticFileUnavailable::EscapesRoot { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_index_failures_other_than_not_found_are_not_masked_by_the_spa_fallback() {
        use std::os::unix::fs::symlink;

        let (root, deployment) = deployment().await;
        let broken_dir = deployment.join("broken");
        fs::create_dir_all(&broken_dir)
            .await
            .expect("create directory with a broken index");
        // A self-referential symlink makes canonicalize fail with a symlink-loop
        // error (not NotFound), simulating any non-missing resolution failure
        // (permission denied, loop, etc.) on the directory's own index.html.
        symlink("index.html", broken_dir.join("index.html")).expect("create symlink loop");

        let error = open_static_file(root.path(), STORED_DIR, "/broken")
            .await
            .expect_err("a symlink-loop failure must not be masked as a missing index");

        assert!(
            matches!(error, StaticFileUnavailable::Unusable { .. }),
            "expected the underlying resolution failure to propagate, got {error:?}"
        );
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

    fn candidate(path: &str, matched: StaticFileMatch) -> StaticCandidate {
        StaticCandidate {
            path: PathBuf::from(path),
            matched,
        }
    }

    #[test]
    fn resolve_static_object_request_root_path_tries_index_then_the_404_page() {
        let request = resolve_static_object_request(STORED_DIR, "/").unwrap();
        assert_eq!(
            request.candidates,
            vec![
                candidate("index.html", StaticFileMatch::Requested),
                candidate("404.html", StaticFileMatch::NotFoundPage),
            ]
        );
    }

    #[test]
    fn resolve_static_object_request_ordinary_asset_never_falls_back_to_the_spa_shell() {
        let request = resolve_static_object_request(STORED_DIR, "/assets/app.js").unwrap();
        assert_eq!(
            request.candidates,
            vec![
                candidate("assets/app.js", StaticFileMatch::Requested),
                candidate("assets/app.js/index.html", StaticFileMatch::Requested),
                candidate("404.html", StaticFileMatch::NotFoundPage),
            ],
            "a path with an extension is never treated as an SPA route"
        );
    }

    #[test]
    fn resolve_static_object_request_extensionless_path_tries_pages_then_404_then_spa_shell() {
        let request = resolve_static_object_request(STORED_DIR, "/docs").unwrap();
        assert_eq!(
            request.candidates,
            vec![
                candidate("docs", StaticFileMatch::Requested),
                candidate("docs/index.html", StaticFileMatch::Requested),
                candidate("docs.html", StaticFileMatch::Requested),
                candidate("404.html", StaticFileMatch::NotFoundPage),
                candidate("index.html", StaticFileMatch::SpaShell),
            ]
        );
    }

    #[test]
    fn resolve_static_object_request_html_page_keeps_dots_in_parent_segments() {
        let request = resolve_static_object_request(STORED_DIR, "/v1.2/guide").unwrap();
        assert_eq!(
            request.candidates[2],
            candidate("v1.2/guide.html", StaticFileMatch::Requested)
        );
    }

    #[test]
    fn resolve_static_object_request_dotted_directory_tries_its_index() {
        let request = resolve_static_object_request(STORED_DIR, "/releases/v1.2/").unwrap();
        assert_eq!(
            request.candidates,
            vec![
                candidate("releases/v1.2", StaticFileMatch::Requested),
                candidate("releases/v1.2/index.html", StaticFileMatch::Requested),
                candidate("404.html", StaticFileMatch::NotFoundPage),
            ],
            "a dotted name is not an SPA route, but may still be a directory"
        );
    }

    #[test]
    fn resolve_static_object_request_trailing_slash_redirects_to_the_html_page() {
        let request = resolve_static_object_request(STORED_DIR, "/about/").unwrap();
        assert_eq!(
            request.candidates,
            vec![
                candidate("about", StaticFileMatch::Requested),
                candidate("about/index.html", StaticFileMatch::Requested),
                candidate("about.html", StaticFileMatch::CanonicalRedirect),
                candidate("404.html", StaticFileMatch::NotFoundPage),
                candidate("index.html", StaticFileMatch::SpaShell),
            ]
        );
    }

    #[test]
    fn canonical_redirect_location_drops_the_slash_and_keeps_the_query() {
        assert_eq!(
            canonical_redirect_location("/about/", None).as_deref(),
            Some("/about")
        );
        assert_eq!(
            canonical_redirect_location("/blog/first-post/", Some("ref=home&x=1")).as_deref(),
            Some("/blog/first-post?ref=home&x=1")
        );
        assert_eq!(
            canonical_redirect_location("/about/", Some("")).as_deref(),
            Some("/about")
        );
    }

    #[test]
    fn canonical_redirect_location_never_builds_an_off_origin_redirect() {
        for raw in [
            "/",
            "//evil.example/",
            "/\\evil.example/",
            "about/",
            "/about",
        ] {
            assert_eq!(canonical_redirect_location(raw, None), None, "{raw}");
        }
    }

    #[test]
    fn resolve_static_object_request_the_404_page_itself_always_answers_404() {
        for request in ["/404", "/404.html"] {
            let candidates = resolve_static_object_request(STORED_DIR, request)
                .unwrap()
                .candidates;
            let not_found_pages: Vec<_> = candidates
                .iter()
                .filter(|candidate| candidate.path == Path::new("404.html"))
                .collect();
            assert_eq!(not_found_pages.len(), 1, "{request}: probed once");
            assert_eq!(
                not_found_pages[0].matched,
                StaticFileMatch::NotFoundPage,
                "{request}"
            );
        }
    }

    #[test]
    fn static_file_match_status_matches_the_response_it_produces() {
        assert_eq!(StaticFileMatch::Requested.status(), 200);
        assert_eq!(StaticFileMatch::SpaShell.status(), 200);
        assert_eq!(StaticFileMatch::CanonicalRedirect.status(), 308);
        assert_eq!(StaticFileMatch::NotFoundPage.status(), 404);
    }

    #[test]
    fn resolve_static_object_request_rejects_traversal_and_sensitive_paths() {
        for request in ["/../secret", "/.git/config", "/.env"] {
            assert!(resolve_static_object_request(STORED_DIR, request).is_err());
        }
        assert!(resolve_static_object_request("../escape", "/app.js").is_err());
    }

    #[test]
    fn static_object_key_joins_directory_and_candidate_with_a_forward_slash() {
        let key = static_object_key(
            Path::new("projects/site/production/deploy-1"),
            Path::new("assets/app.js"),
        );
        assert_eq!(key, "projects/site/production/deploy-1/assets/app.js");
    }

    #[test]
    fn object_etag_is_stable_and_distinguishes_key_or_size_changes() {
        let first = object_etag("projects/a/deploy-1/index.html", 100);
        let same = object_etag("projects/a/deploy-1/index.html", 100);
        let different_size = object_etag("projects/a/deploy-1/index.html", 101);
        let different_key = object_etag("projects/a/deploy-2/index.html", 100);
        assert_eq!(first, same);
        assert_ne!(first, different_size);
        assert_ne!(first, different_key);
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
