// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Materialize public GitHub Compose sources without delegating network access to Docker.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::compose::ComposeError;

const ARCHIVE_HOST: &str = "codeload.github.com";
const MAX_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

fn rejected(reason: impl Into<String>) -> ComposeError {
    ComposeError::InvalidComposePath {
        field: "Compose reference".to_string(),
        // Never echo untrusted URLs: userinfo, query strings and fragments can contain secrets.
        path: "<remote Compose source>".to_string(),
        reason: reason.into(),
    }
}

#[derive(Debug)]
struct GitSource {
    archive_url: reqwest::Url,
    compose_path: Option<PathBuf>,
}

fn safe_relative_path(path: &str) -> Result<PathBuf, ComposeError> {
    let path_buf = PathBuf::from(path);
    if path.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || path_buf
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(rejected(
            "remote Compose paths must be relative and cannot contain parent traversal",
        ));
    }
    Ok(path_buf)
}

fn parse_reference(reference: &str) -> Result<GitSource, ComposeError> {
    let url = reqwest::Url::parse(reference).map_err(|_| rejected("invalid remote Compose URL"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
    {
        return Err(rejected("only public https://github.com/owner/repository.git#ref:path Compose references are supported; credentials, queries and custom ports are not supported"));
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        })
    {
        return Err(rejected(
            "remote Compose URL must identify a GitHub owner and repository",
        ));
    }
    let repo = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
    if repo.is_empty() || repo == "." || repo == ".." {
        return Err(rejected("remote Compose repository name is invalid"));
    }
    let fragment = urlencoding::decode(url.fragment().unwrap_or_default())
        .map_err(|_| rejected("invalid encoding in remote Compose reference"))?;
    let (git_ref, compose_path) = match fragment.split_once(':') {
        Some((git_ref, path)) => (git_ref, Some(safe_relative_path(path)?)),
        None => (fragment.as_ref(), None),
    };
    let git_ref = if git_ref.is_empty() { "HEAD" } else { git_ref };
    if git_ref.chars().any(|c| c.is_control()) || git_ref == "." || git_ref == ".." {
        return Err(rejected("invalid Git revision in remote Compose reference"));
    }
    let mut archive_url = reqwest::Url::parse("https://codeload.github.com/")
        .map_err(|_| rejected("could not construct trusted archive URL"))?;
    archive_url
        .path_segments_mut()
        .map_err(|_| rejected("could not construct trusted archive path"))?
        .extend([parts[0], repo, "tar.gz", git_ref]);
    Ok(GitSource {
        archive_url,
        compose_path,
    })
}

/// Return the selected Compose file and its repository root. The caller owns the
/// cache directory lifetime, which must extend through deployment of relative assets.
pub(super) async fn materialize(
    reference: &str,
    cache: &Path,
) -> Result<(PathBuf, PathBuf), ComposeError> {
    let source = parse_reference(reference)?;
    let bytes = tokio::time::timeout(FETCH_TIMEOUT, download_archive(&source.archive_url))
        .await
        .map_err(|_| rejected("remote Compose archive download exceeded 60 seconds"))??;
    receive_extraction(cache, move |snapshot, cancellation| {
        extract_archive_contents(
            bytes.as_slice(),
            snapshot,
            source.compose_path.as_deref(),
            cancellation,
        )
    })
    .await
}

struct ExtractedArchive {
    snapshot: tempfile::TempDir,
    selected: PathBuf,
    root: PathBuf,
}

impl ExtractedArchive {
    #[cfg(test)]
    fn persist(self) -> (PathBuf, PathBuf) {
        let _ = self.snapshot.keep();
        (self.selected, self.root)
    }
}

async fn receive_extraction<F>(cache: &Path, extract: F) -> Result<(PathBuf, PathBuf), ComposeError>
where
    F: FnOnce(tempfile::TempDir, &CancellationToken) -> Result<ExtractedArchive, ComposeError>
        + Send
        + 'static,
{
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let result = tokio::task::spawn_blocking(move || {
        check_cancelled(&cancellation).map_err(|e| rejected(e.to_string()))?;
        let snapshot = tempfile::Builder::new()
            .prefix("temps-compose-staging-")
            .tempdir()
            .map_err(|e| rejected(format!("could not create remote Compose snapshot: {e}")))?;
        let archive = extract(snapshot, &cancellation)?;
        check_cancelled(&cancellation).map_err(|e| rejected(e.to_string()))?;
        Ok::<_, ComposeError>(archive)
    })
    .await
    .map_err(|_| rejected("remote Compose archive extraction task failed"))??;
    // The worker returns its owning guard. The receiver retains both staging
    // and adoption guards until every asynchronous copy operation has finished.
    adopt_archive(result, cache, |source, destination| {
        std::fs::rename(source, destination)
    })
    .await
}

/// All path creation happens synchronously on the receiving task. The async
/// fallback only submits I/O on already-open descriptors, so a cancelled copy
/// cannot recreate paths after the snapshot guards or checkout are removed.
async fn adopt_archive(
    archive: ExtractedArchive,
    cache: &Path,
    rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(PathBuf, PathBuf), ComposeError> {
    let staging_root = archive.snapshot.path().canonicalize().map_err(|e| {
        rejected(format!(
            "could not resolve remote Compose staging directory: {e}"
        ))
    })?;
    let selected = archive
        .selected
        .strip_prefix(&staging_root)
        .map_err(|_| rejected("selected Compose file escaped archive staging"))?
        .to_path_buf();
    let root = archive
        .root
        .strip_prefix(&staging_root)
        .map_err(|_| rejected("Compose repository root escaped archive staging"))?
        .to_path_buf();
    let adopted = tempfile::Builder::new()
        .prefix("repository-")
        .tempdir_in(cache)
        .map_err(|e| {
            rejected(format!(
                "could not create remote Compose adoption directory: {e}"
            ))
        })?;
    let destination = adopted.path().join("snapshot");
    match rename(archive.snapshot.path(), &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            tokio::time::timeout(
                FETCH_TIMEOUT,
                copy_snapshot(archive.snapshot.path(), &destination),
            )
            .await
            .map_err(|_| rejected("remote Compose adoption exceeded 60 seconds"))?
            .map_err(|e| {
                rejected(format!(
                    "could not adopt remote Compose archive across filesystems: {e}"
                ))
            })?;
        }
        Err(error) => {
            return Err(rejected(format!(
                "could not adopt remote Compose archive: {error}"
            )))
        }
    }
    let selected = destination
        .join(selected)
        .canonicalize()
        .map_err(|e| rejected(format!("could not resolve adopted Compose file: {e}")))?;
    let root = destination
        .join(root)
        .canonicalize()
        .map_err(|e| rejected(format!("could not resolve adopted Compose root: {e}")))?;
    let _ = adopted.keep();
    Ok((selected, root))
}

async fn copy_snapshot(source: &Path, destination: &Path) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    let mut entries = 0_usize;
    let mut remaining = MAX_EXTRACTED_BYTES;
    let mut buffer = vec![0_u8; 64 * 1024];
    while let Some((source, destination)) = pending.pop() {
        // Never delegate namespace mutations to tokio::fs or a blocking worker:
        // those operations could outlive cancellation and recreate the checkout.
        std::fs::create_dir(&destination)?;
        tokio::task::yield_now().await;
        for entry in std::fs::read_dir(&source)? {
            tokio::task::yield_now().await;
            entries += 1;
            // Include directories implicitly created for archives that omit them.
            if entries > MAX_ARCHIVE_ENTRIES * 2 {
                return Err(std::io::Error::other(
                    "remote Compose adoption exceeds its file-count limit",
                ));
            }
            let entry = entry?;
            let path = entry.path();
            let target = destination.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((path, target));
            } else if kind.is_file() {
                let input = std::fs::File::open(&path)?;
                let output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                output.set_permissions(input.metadata()?.permissions())?;
                let mut input = tokio::fs::File::from_std(input);
                let mut output = tokio::fs::File::from_std(output);
                loop {
                    tokio::task::yield_now().await;
                    let count = input.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    remaining = remaining.checked_sub(count as u64).ok_or_else(|| {
                        std::io::Error::other("remote Compose adoption exceeds the 64 MiB limit")
                    })?;
                    output.write_all(&buffer[..count]).await?;
                }
                // Tokio may buffer a write in its blocking pool. Await completion
                // before exposing the adopted paths to Docker Compose.
                output.flush().await?;
            } else {
                return Err(std::io::Error::other(
                    "remote Compose adoption encountered a link or special file",
                ));
            }
        }
    }
    Ok(())
}

fn check_cancelled(cancellation: &CancellationToken) -> std::io::Result<()> {
    if cancellation.is_cancelled() {
        Err(std::io::Error::other(
            "remote Compose archive extraction cancelled",
        ))
    } else {
        Ok(())
    }
}

struct CancellableReader<'a, R> {
    inner: R,
    cancellation: &'a CancellationToken,
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        check_cancelled(self.cancellation)?;
        let count = self.inner.read(buffer)?;
        check_cancelled(self.cancellation)?;
        Ok(count)
    }
}

async fn download_archive(url: &reqwest::Url) -> Result<Vec<u8>, ComposeError> {
    let addresses = temps_core::url_validation::resolve_and_validate_domain(ARCHIVE_HOST, 443)
        .await
        .map_err(|_| {
            rejected("trusted Compose archive host did not resolve to public addresses")
        })?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(ARCHIVE_HOST, &addresses)
        .connect_timeout(Duration::from_secs(10))
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|_| rejected("could not initialize remote Compose archive client"))?;
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|_| rejected("failed to download remote Compose archive from GitHub"))?;
    if !response.status().is_success() {
        return Err(rejected(format!("GitHub Compose archive request returned HTTP {}; verify that the repository and revision exist and are public", response.status())));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64)
    {
        return Err(rejected(
            "remote Compose archive exceeds the 16 MiB download limit",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| rejected("failed while reading remote Compose archive"))?
    {
        if chunk.len() > MAX_DOWNLOAD_BYTES.saturating_sub(bytes.len()) {
            return Err(rejected(
                "remote Compose archive exceeds the 16 MiB download limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn copy_with_deadline(
    input: &mut impl Read,
    output: &mut impl Write,
    started: Instant,
    cancellation: &CancellationToken,
) -> std::io::Result<u64> {
    let mut copied = 0;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancelled(cancellation)?;
        if started.elapsed() > FETCH_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "remote Compose extraction exceeded 60 seconds",
            ));
        }
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(copied);
        }
        check_cancelled(cancellation)?;
        output.write_all(&buffer[..count])?;
        copied += count as u64;
    }
}

fn extract_archive_contents(
    input: impl Read,
    snapshot: tempfile::TempDir,
    compose_path: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<ExtractedArchive, ComposeError> {
    check_cancelled(cancellation).map_err(|e| rejected(e.to_string()))?;
    // Check every underlying read, including metadata tar consumes internally.
    let input = CancellableReader {
        inner: input,
        cancellation,
    };
    // Limit the whole decompressed stream, including metadata and padding, not just file sizes.
    let decoder = flate2::read::GzDecoder::new(input).take(MAX_EXTRACTED_BYTES + 1);
    let mut archive = tar::Archive::new(decoder);
    let started = Instant::now();
    let mut total_size = 0_u64;
    let mut repository_top = None;
    for (index, entry) in archive
        .entries()
        .map_err(|e| rejected(format!("invalid remote Compose archive: {e}")))?
        .enumerate()
    {
        check_cancelled(cancellation).map_err(|e| rejected(e.to_string()))?;
        if index >= MAX_ARCHIVE_ENTRIES || started.elapsed() > FETCH_TIMEOUT {
            return Err(rejected(
                "remote Compose archive exceeds the file-count or extraction-time limit",
            ));
        }
        let mut entry =
            entry.map_err(|e| rejected(format!("invalid remote Compose archive entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        // GitHub emits a PAX global header containing its commit identifier. It is
        // archive metadata, never a filesystem object; do not materialize it.
        if entry_type.is_pax_global_extensions() {
            continue;
        }
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(rejected(
                "remote Compose archives cannot contain symlinks, hard links or special files",
            ));
        }
        let path = entry
            .path()
            .map_err(|_| rejected("invalid path in remote Compose archive"))?
            .into_owned();
        let path_text = path
            .to_str()
            .ok_or_else(|| rejected("remote Compose archive paths must be UTF-8"))?;
        safe_relative_path(path_text)?;
        let mut components = path.components();
        let Some(Component::Normal(top)) = components.next() else {
            return Err(rejected(
                "remote Compose archive must have a single repository directory",
            ));
        };
        if let Some(expected) = &repository_top {
            if expected != top {
                return Err(rejected(
                    "remote Compose archive contains multiple repository roots",
                ));
            }
        } else {
            repository_top = Some(top.to_os_string());
        }
        if components.next().is_none() && !entry_type.is_dir() {
            return Err(rejected("remote Compose archive root must be a directory"));
        }
        let size = entry.size();
        total_size = total_size
            .checked_add(size)
            .filter(|size| *size <= MAX_EXTRACTED_BYTES)
            .ok_or_else(|| {
                rejected("remote Compose archive exceeds the 64 MiB extraction limit")
            })?;
        check_cancelled(cancellation).map_err(|e| rejected(e.to_string()))?;
        let destination = snapshot.path().join(&path);
        if entry_type.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| {
                rejected(format!(
                    "could not create remote Compose archive directory: {e}"
                ))
            })?;
        } else {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    rejected(format!(
                        "could not create remote Compose archive parent: {e}"
                    ))
                })?;
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|e| {
                    rejected(format!("could not create remote Compose archive file: {e}"))
                })?;
            copy_with_deadline(&mut entry, &mut output, started, cancellation).map_err(|e| {
                rejected(format!(
                    "could not extract remote Compose archive file: {e}"
                ))
            })?;
            output.flush().map_err(|e| {
                rejected(format!("could not flush remote Compose archive file: {e}"))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry
                    .header()
                    .mode()
                    .map_err(|_| rejected("invalid file mode in remote Compose archive"))?;
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
                )
                .map_err(|e| {
                    rejected(format!(
                        "could not set remote Compose file permissions: {e}"
                    ))
                })?;
            }
        }
    }
    let mut decoder = archive.into_inner();
    copy_with_deadline(&mut decoder, &mut std::io::sink(), started, cancellation)
        .map_err(|e| rejected(format!("invalid compressed remote Compose archive: {e}")))?;
    if decoder.limit() == 0 {
        return Err(rejected(
            "remote Compose archive exceeds the 64 MiB decompressed-stream limit",
        ));
    }
    let top = repository_top.ok_or_else(|| rejected("remote Compose archive is empty"))?;
    let root = snapshot.path().join(top).canonicalize().map_err(|e| {
        rejected(format!(
            "could not resolve remote Compose repository root: {e}"
        ))
    })?;
    let requested = compose_path.map_or_else(|| root.clone(), |path| root.join(path));
    let selected = if requested.is_dir() {
        [
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ]
        .iter()
        .map(|name| requested.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            rejected("remote repository directory has no default Compose file; specify #ref:path")
        })?
    } else {
        requested
    };
    if !selected.is_file() {
        return Err(rejected(
            "requested Compose file does not exist in the remote repository",
        ));
    }
    let selected = selected.canonicalize().map_err(|e| {
        rejected(format!(
            "could not resolve selected remote Compose file: {e}"
        ))
    })?;
    if !selected.starts_with(&root) {
        return Err(rejected(
            "selected Compose file escapes its remote repository",
        ));
    }
    check_cancelled(cancellation).map_err(|e| rejected(e.to_string()))?;
    Ok(ExtractedArchive {
        snapshot,
        selected,
        root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn extract_archive(
        bytes: &[u8],
        cache_root: &Path,
        compose_path: Option<&Path>,
    ) -> Result<(PathBuf, PathBuf), ComposeError> {
        let snapshot = tempfile::tempdir_in(cache_root).unwrap();
        extract_archive_contents(bytes, snapshot, compose_path, &CancellationToken::new())
            .map(ExtractedArchive::persist)
    }

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        let metadata = b"18 comment=commit\n";
        let mut global = tar::Header::new_gnu();
        global.set_entry_type(tar::EntryType::XGlobalHeader);
        global.set_size(metadata.len() as u64);
        global.set_mode(0o644);
        global.set_cksum();
        archive
            .append_data(&mut global, "pax_global_header", metadata.as_slice())
            .unwrap();
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            archive.append_data(&mut header, path, *bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    fn extraction_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    struct PausedReader {
        bytes: std::io::Cursor<Vec<u8>>,
        entered: Option<std::sync::mpsc::Sender<()>>,
        resume: std::sync::mpsc::Receiver<()>,
    }

    impl Read for PausedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.bytes.read(buffer)
        }
    }

    #[test]
    fn cancellation_during_header_read_cleans_detached_staging() {
        extraction_runtime().block_on(async {
            let cache = Arc::new(tempfile::tempdir().unwrap());
            let cache_path = cache.path().to_path_buf();
            let weak = Arc::downgrade(&cache);
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let input = PausedReader {
                bytes: std::io::Cursor::new(archive(&[("root/compose.yaml", b"services: {}")])),
                entered: Some(entered_tx),
                resume: resume_rx,
            };
            let mut future = Box::pin(receive_extraction(cache.path(), move |snapshot, token| {
                let result = extract_archive_contents(input, snapshot, None, token);
                result_tx
                    .send(result.as_ref().err().map(ToString::to_string))
                    .unwrap();
                result
            }));
            assert!(futures::poll!(future.as_mut()).is_pending());
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(future);
            drop(cache);
            // Checkout cleanup is immediate; the worker only touches external staging.
            assert!(!cache_path.exists());
            assert!(weak.upgrade().is_none());
            resume_tx.send(()).unwrap();
            // A single blocking thread makes this a deterministic completion fence.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(result_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .contains("cancelled"));
            assert!(weak.upgrade().is_none());
            assert!(!cache_path.exists());
        });
    }

    #[test]
    fn cancelled_worker_never_recreates_deleted_checkout_ancestor() {
        extraction_runtime().block_on(async {
            let checkout = tempfile::tempdir().unwrap();
            let checkout_path = checkout.path().to_path_buf();
            let cache = Arc::new(tempfile::tempdir_in(&checkout_path).unwrap());
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let (staging_tx, staging_rx) = std::sync::mpsc::channel();
            let input = PausedReader {
                bytes: std::io::Cursor::new(archive(&[("root/compose.yaml", b"services: {}")])),
                entered: Some(entered_tx),
                resume: resume_rx,
            };
            let mut future = Box::pin(receive_extraction(cache.path(), move |snapshot, token| {
                staging_tx.send(snapshot.path().to_path_buf()).unwrap();
                extract_archive_contents(input, snapshot, None, token)
            }));
            assert!(futures::poll!(future.as_mut()).is_pending());
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let staging = staging_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(!staging.starts_with(&checkout_path));
            // Mirrors independent deployment cleanup, which can bypass Arc leases.
            std::fs::remove_dir_all(&checkout_path).unwrap();
            drop(future);
            drop(cache);
            resume_tx.send(()).unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!checkout_path.exists());
            assert!(!staging.exists());
        });
    }

    #[test]
    fn adoption_handles_rename_cross_device_copy_and_failed_copy_cleanup() {
        extraction_runtime().block_on(async {
            for cross_device in [false, true] {
                let cache = tempfile::tempdir().unwrap();
                let snapshot = tempfile::tempdir().unwrap();
                let staging = snapshot.path().to_path_buf();
                let bytes = archive(&[
                    ("root/docker/compose.yaml", b"services: {}"),
                    ("root/docker/asset.txt", b"asset"),
                ]);
                let archive = extract_archive_contents(
                    bytes.as_slice(),
                    snapshot,
                    Some(Path::new("docker/compose.yaml")),
                    &CancellationToken::new(),
                )
                .unwrap();
                let (selected, root) = adopt_archive(archive, cache.path(), |source, target| {
                    if cross_device {
                        Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
                    } else {
                        std::fs::rename(source, target)
                    }
                })
                .await
                .unwrap();
                assert!(selected.starts_with(cache.path().canonicalize().unwrap()));
                assert_eq!(
                    std::fs::read(root.join("docker/asset.txt")).unwrap(),
                    b"asset"
                );
                assert!(!staging.exists());
            }
            let cache = tempfile::tempdir().unwrap();
            let snapshot = tempfile::tempdir().unwrap();
            let staging = snapshot.path().to_path_buf();
            let bytes = archive(&[("root/compose.yaml", b"services: {}")]);
            let archive = extract_archive_contents(
                bytes.as_slice(),
                snapshot,
                None,
                &CancellationToken::new(),
            )
            .unwrap();
            // Force a bounded-copy error without allocating a large buffer.
            std::fs::OpenOptions::new()
                .write(true)
                .open(&archive.selected)
                .unwrap()
                .set_len(MAX_EXTRACTED_BYTES + 1)
                .unwrap();
            assert!(
                adopt_archive(archive, cache.path(), |_, _| Err(std::io::Error::from(
                    std::io::ErrorKind::CrossesDevices
                )))
                .await
                .is_err()
            );
            assert!(!staging.exists());
            assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none());
        });
    }

    #[test]
    fn cross_device_adoption_yields_to_outer_deadline_and_cleans_both_guards() {
        extraction_runtime().block_on(async {
            let cache = tempfile::tempdir().unwrap();
            let snapshot = tempfile::tempdir().unwrap();
            let staging = snapshot.path().to_path_buf();
            let bytes = archive(&[("root/compose.yaml", b"services: {}")]);
            let extracted = extract_archive_contents(
                bytes.as_slice(),
                snapshot,
                None,
                &CancellationToken::new(),
            )
            .unwrap();
            // A zero-duration timeout can share the timer driver's current tick
            // with this tiny copy and let it finish first. Use an elapsed deadline:
            // Timeout polls adoption first, so synchronous copying would still
            // incorrectly succeed, while a yielding copy must be cancelled.
            let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
            let result = tokio::time::timeout_at(
                deadline,
                adopt_archive(extracted, cache.path(), |_, _| {
                    Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
                }),
            )
            .await;
            assert!(
                result.is_err(),
                "outer timeout must be polled while adopting"
            );
            assert!(!staging.exists());
            assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none());
        });
    }

    #[test]
    fn cancelling_partial_cross_device_copy_never_recreates_checkout() {
        extraction_runtime().block_on(async {
            let checkout = tempfile::tempdir().unwrap();
            let checkout_path = checkout.path().to_path_buf();
            let cache = tempfile::tempdir_in(&checkout_path).unwrap();
            let snapshot = tempfile::tempdir().unwrap();
            let staging = snapshot.path().to_path_buf();
            let bytes = archive(&[("root/compose.yaml", b"services: {}")]);
            let extracted = extract_archive_contents(
                bytes.as_slice(),
                snapshot,
                None,
                &CancellationToken::new(),
            )
            .unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&extracted.selected)
                .unwrap()
                .set_len(MAX_EXTRACTED_BYTES)
                .unwrap();
            let mut adoption = Box::pin(adopt_archive(extracted, cache.path(), |_, _| {
                Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
            }));
            let mut pending_polls = 0;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    assert!(futures::poll!(adoption.as_mut()).is_pending());
                    pending_polls += 1;
                    if let Some(adopted) = std::fs::read_dir(cache.path()).unwrap().next() {
                        let copied = adopted.unwrap().path().join("snapshot/root/compose.yaml");
                        if let Ok(metadata) = copied.metadata() {
                            if metadata.len() >= 64 * 1024 {
                                assert!(metadata.len() < MAX_EXTRACTED_BYTES);
                                break;
                            }
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(
                pending_polls > 1,
                "copy must cooperate with the runtime between chunks"
            );
            // Cancel with asynchronous descriptor I/O potentially still in flight.
            std::fs::remove_dir_all(&checkout_path).unwrap();
            drop(adoption);
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!checkout_path.exists());
            assert!(!staging.exists());
        });
    }

    #[test]
    fn cancelled_queued_extraction_never_runs_or_recreates_cache() {
        extraction_runtime().block_on(async {
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let cache = Arc::new(tempfile::tempdir().unwrap());
            let cache_path = cache.path().to_path_buf();
            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_called = Arc::clone(&called);
            let mut future = Box::pin(receive_extraction(cache.path(), move |_, _| {
                worker_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(rejected("queued extraction should not run"))
            }));
            assert!(futures::poll!(future.as_mut()).is_pending());
            drop(future);
            drop(cache);
            assert!(!cache_path.exists());
            resume_tx.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
            assert!(!cache_path.exists());
        });
    }

    #[test]
    fn completed_unreceived_extraction_retains_cleanup_guards() {
        extraction_runtime().block_on(async {
            let cache = Arc::new(tempfile::tempdir().unwrap());
            let cache_path = cache.path().to_path_buf();
            let bytes = archive(&[("root/compose.yaml", b"services: {}")]);
            let (staging_tx, staging_rx) = std::sync::mpsc::channel();
            let mut future = Box::pin(receive_extraction(cache.path(), move |snapshot, token| {
                staging_tx.send(snapshot.path().to_path_buf()).unwrap();
                extract_archive_contents(bytes.as_slice(), snapshot, None, token)
            }));
            assert!(futures::poll!(future.as_mut()).is_pending());
            // Complete the worker without ever polling its owning future again.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            let staging = staging_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(staging.is_dir());
            assert!(std::fs::read_dir(&cache_path).unwrap().next().is_none());
            drop(future);
            drop(cache);
            assert!(!cache_path.exists());
            assert!(!staging.exists());
        });
    }

    #[test]
    fn parses_git_revision_and_compose_path() {
        let source = parse_reference(
            "https://github.com/example/stack.git#feature/test:docker/compose.yaml",
        )
        .unwrap();
        assert_eq!(
            source.archive_url.as_str(),
            "https://codeload.github.com/example/stack/tar.gz/feature%2Ftest"
        );
        assert_eq!(
            source.compose_path,
            Some(PathBuf::from("docker/compose.yaml"))
        );
        let source = parse_reference("https://github.com/example/stack.git").unwrap();
        assert!(source.archive_url.as_str().ends_with("/HEAD"));
        assert!(source.compose_path.is_none());
    }

    #[test]
    fn rejects_unsupported_and_secret_bearing_references_without_echoing_them() {
        for reference in [
            "http://github.com/example/stack.git",
            "https://localhost/example/stack.git",
            "https://secret@github.com/example/stack.git",
            "https://github.com/example/stack.git?token=secret",
            "https://github.com/example/stack.git#main:../../secret",
            "https://github.com/example/stack.git#main:/secret",
            "https://github.com/example/stack.git#main:..%2Fsecret",
            "https://github.com/example/stack.git#main:..\\secret",
            "https://github.com/example/.git",
        ] {
            let error = parse_reference(reference).unwrap_err().to_string();
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn extracts_selected_file_and_preserves_relative_assets() {
        let cache = tempfile::tempdir().unwrap();
        let data = archive(&[
            ("stack-main/docker/compose.yml", b"services: {}"),
            ("stack-main/docker/config.txt", b"config"),
        ]);
        let (selected, root) =
            extract_archive(&data, cache.path(), Some(Path::new("docker/compose.yml"))).unwrap();
        assert_eq!(std::fs::read(selected).unwrap(), b"services: {}");
        assert_eq!(
            std::fs::read(root.join("docker/config.txt")).unwrap(),
            b"config"
        );
        assert!(
            extract_archive(&data, cache.path(), Some(Path::new("docker")))
                .unwrap()
                .0
                .ends_with("docker/compose.yml")
        );
        assert!(root.starts_with(cache.path().canonicalize().unwrap()));
    }

    #[test]
    fn finds_default_compose_and_rejects_missing_file() {
        let cache = tempfile::tempdir().unwrap();
        let data = archive(&[("stack-main/docker-compose.yml", b"services: {}")]);
        assert!(extract_archive(&data, cache.path(), None)
            .unwrap()
            .0
            .ends_with("docker-compose.yml"));
        assert!(extract_archive(&data, cache.path(), Some(Path::new("missing.yml"))).is_err());
        assert_eq!(
            std::fs::read_dir(cache.path()).unwrap().count(),
            1,
            "failed extraction must clean its snapshot"
        );
    }

    #[test]
    fn global_pax_paths_cannot_redirect_extracted_files() {
        let cache = tempfile::tempdir().unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let metadata = b"22 path=../../outside\n26 linkpath=../../outside\n";
        let mut global = tar::Header::new_gnu();
        global.set_entry_type(tar::EntryType::XGlobalHeader);
        global.set_size(metadata.len() as u64);
        global.set_mode(0o644);
        global.set_cksum();
        builder
            .append_data(&mut global, "pax_global_header", metadata.as_slice())
            .unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(0);
        file.set_mode(0o644);
        file.set_cksum();
        builder
            .append_data(&mut file, "root/compose.yaml", b"".as_slice())
            .unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let (selected, root) = extract_archive(&bytes, cache.path(), None).unwrap();
        assert_eq!(selected, root.join("compose.yaml"));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn rejects_traversal_hardlinks_and_oversized_entries() {
        let cache = tempfile::tempdir().unwrap();
        for (path, kind, size) in [
            ("../compose.yaml", tar::EntryType::Regular, 0),
            ("/tmp/compose.yaml", tar::EntryType::Regular, 0),
            ("stack-main/compose.yaml", tar::EntryType::Link, 0),
            ("stack-main/compose.yaml", tar::EntryType::Fifo, 0),
            (
                "stack-main/compose.yaml",
                tar::EntryType::Regular,
                MAX_EXTRACTED_BYTES + 1,
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_entry_type(kind);
            header.set_size(size);
            // Construct hostile raw headers; tar::Builder correctly refuses traversal paths.
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(header.as_bytes()).unwrap();
            encoder.write_all(&[0; 1024]).unwrap();
            let data = encoder.finish().unwrap();
            assert!(extract_archive(&data, cache.path(), None).is_err());
        }
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_excessive_decompressed_padding_and_entry_count() {
        let cache = tempfile::tempdir().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let padding = [0_u8; 64 * 1024];
        for _ in 0..=(MAX_EXTRACTED_BYTES / padding.len() as u64) {
            encoder.write_all(&padding).unwrap();
        }
        let error = extract_archive(&encoder.finish().unwrap(), cache.path(), None).unwrap_err();
        assert!(error.to_string().contains("decompressed-stream limit"));
        let files: Vec<_> = (0..=MAX_ARCHIVE_ENTRIES)
            .map(|i| format!("root/file-{i}"))
            .collect();
        let files: Vec<_> = files
            .iter()
            .map(|name| (name.as_str(), b"".as_slice()))
            .collect();
        let error = extract_archive(&archive(&files), cache.path(), None).unwrap_err();
        assert!(error.to_string().contains("file-count"));
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_symlinks_duplicate_files_and_multiple_roots() {
        let cache = tempfile::tempdir().unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_link(&mut header, "stack-main/compose.yaml", "/etc/passwd")
            .unwrap();
        let data = builder.into_inner().unwrap().finish().unwrap();
        assert!(extract_archive(&data, cache.path(), None).is_err());
        for files in [
            vec![
                ("a/compose.yaml", b"a".as_slice()),
                ("a/compose.yaml", b"b".as_slice()),
            ],
            vec![
                ("a/compose.yaml", b"a".as_slice()),
                ("b/file", b"b".as_slice()),
            ],
        ] {
            assert!(extract_archive(&archive(&files), cache.path(), None).is_err());
        }
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }
}
