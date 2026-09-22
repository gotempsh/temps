// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Materialize public GitHub Compose sources without delegating network access to Docker.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

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
    cache_root: &Path,
) -> Result<(PathBuf, PathBuf), ComposeError> {
    let source = parse_reference(reference)?;
    let bytes = tokio::time::timeout(FETCH_TIMEOUT, download_archive(&source.archive_url))
        .await
        .map_err(|_| rejected("remote Compose archive download exceeded 60 seconds"))??;
    let cache_root = cache_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        extract_archive(&bytes, &cache_root, source.compose_path.as_deref())
    })
    .await
    .map_err(|_| rejected("remote Compose archive extraction task failed"))?
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
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if started.elapsed() > FETCH_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "remote Compose extraction exceeded 60 seconds",
            ));
        }
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        output.write_all(&buffer[..count])?;
    }
}

fn extract_archive(
    bytes: &[u8],
    cache_root: &Path,
    compose_path: Option<&Path>,
) -> Result<(PathBuf, PathBuf), ComposeError> {
    let snapshot = tempfile::Builder::new()
        .prefix("repository-")
        .tempdir_in(cache_root)
        .map_err(|e| rejected(format!("could not create remote Compose snapshot: {e}")))?;
    // Limit the whole decompressed stream, including metadata and padding, not just file sizes.
    let decoder = flate2::read::GzDecoder::new(bytes).take(MAX_EXTRACTED_BYTES + 1);
    let mut archive = tar::Archive::new(decoder);
    let started = Instant::now();
    let mut total_size = 0_u64;
    let mut repository_top = None;
    for (index, entry) in archive
        .entries()
        .map_err(|e| rejected(format!("invalid remote Compose archive: {e}")))?
        .enumerate()
    {
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
            copy_with_deadline(&mut entry, &mut output, started).map_err(|e| {
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
    copy_with_deadline(&mut decoder, &mut std::io::sink(), started)
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
    let _ = snapshot.keep();
    Ok((selected, root))
}

#[cfg(test)]
mod tests {
    use super::*;

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
