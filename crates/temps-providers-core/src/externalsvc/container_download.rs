// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Get a file out of a container through the Docker archive API.
//!
//! The counterpart of [`super::container_upload`]. A helper container that
//! writes its output to a bind-mounted host directory only works when the
//! Docker daemon sees the host's filesystem; with Docker in a VM (Colima,
//! Docker Desktop, Lima) or on a remote host the bind source is created
//! inside the VM, the helper writes there, and the Temps host finds nothing
//! at the path. Writing into the container's own filesystem and copying the
//! file out with `GET /containers/{id}/archive` works wherever the daemon is.
//!
//! Memory stays constant regardless of the file's size: the tar stream is fed
//! through a bounded channel into a blocking tar reader that copies the single
//! entry straight to disk.

use std::io::{Read, Write};
use std::path::Path;

use bollard::query_parameters::DownloadFromContainerOptionsBuilder;
use bollard::Docker;
use bytes::{Buf, Bytes};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Chunks buffered between the Docker download and the tar extractor.
/// Bollard yields chunks of at most a few tens of KiB, so this bounds the
/// in-flight memory to well under a MiB regardless of the file's size.
const DOWNLOAD_CHANNEL_CAPACITY: usize = 8;

/// Read at most the last `max_bytes` (plus a few bytes of slack so a UTF-8
/// character cut by the seek can be dropped cleanly) of the file at `path`.
/// A failing tool can write an arbitrarily large diagnostics file; only its
/// tail is ever shown, so only its tail is loaded into memory.
pub async fn read_file_tail(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let window = (max_bytes as u64).saturating_add(4);
    if len > window {
        file.seek(std::io::SeekFrom::Start(len - window)).await?;
    }
    let mut tail = Vec::with_capacity(len.min(window) as usize);
    file.take(window).read_to_end(&mut tail).await?;
    Ok(tail)
}

/// Why [`copy_file_out_of_container`] did not produce the file.
#[derive(Debug, PartialEq, Eq)]
pub enum CopyOutFailure {
    Cancelled,
    Failed(String),
}

/// Stream one regular file out of a container (running or exited) into
/// `host_path`, which must not exist yet. Returns the bytes written.
///
/// The Docker archive API returns a tar stream; it is fed through a bounded
/// channel into a blocking tar reader that copies the single entry straight
/// to disk, so memory use does not depend on the file size. A partial host
/// file is deleted on failure.
pub async fn copy_file_out_of_container(
    docker: &Docker,
    container: &str,
    container_path: &str,
    host_path: &Path,
    cancel: &CancellationToken,
) -> Result<u64, CopyOutFailure> {
    let expected_name = Path::new(container_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            CopyOutFailure::Failed(format!(
                "'{container_path}' does not name a file inside the container"
            ))
        })?
        .to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
    let dest = host_path.to_path_buf();
    let extractor = tokio::task::spawn_blocking(move || {
        extract_single_file(ChannelReader::new(rx), &expected_name, &dest)
    });

    let mut stream = docker.download_from_container(
        container,
        Some(
            DownloadFromContainerOptionsBuilder::new()
                .path(container_path)
                .build(),
        ),
    );
    let mut download_error: Option<String> = None;
    let mut cancelled = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break;
            }
            next = stream.next() => match next {
                Some(Ok(chunk)) => {
                    // The extractor stops reading once it has the file (or
                    // has failed); its error is reported below.
                    if tx.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    let message = e.to_string();
                    let _ = tx.send(Err(std::io::Error::other(message.clone()))).await;
                    download_error = Some(message);
                    break;
                }
                None => break,
            }
        }
    }
    // Closing the channel ends the extractor's input, so it always returns.
    drop(tx);
    drop(stream);
    let (extracted, created) = match extractor.await {
        Ok(Extracted { result, created }) => (result, created),
        Err(join_error) => (
            Err(format!("the tar extraction task failed: {join_error}")),
            false,
        ),
    };

    let failure = if cancelled {
        CopyOutFailure::Cancelled
    } else if let Some(message) = download_error {
        CopyOutFailure::Failed(format!("the Docker archive download failed: {message}"))
    } else {
        match extracted {
            Ok(size) => return Ok(size),
            Err(reason) => CopyOutFailure::Failed(reason),
        }
    };
    // Only a file this call created is deleted: `create_new` refuses to open
    // a path that already exists, so a pre-existing file is never touched.
    if created {
        remove_if_present(host_path).await;
    }
    Err(failure)
}

/// Result of [`extract_single_file`], plus whether it created `dest` (and
/// so owns any partial file left there on failure).
struct Extracted {
    result: Result<u64, String>,
    created: bool,
}

async fn remove_if_present(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), error = %e, "container_download: could not delete partial copy");
        }
    }
}

/// Read the single regular file `expected_name` from a tar stream into a new
/// file at `dest`. Fails, rather than returning a short count, when the
/// stream ends before the entry's declared size.
fn extract_single_file<R: Read>(reader: R, expected_name: &str, dest: &Path) -> Extracted {
    let mut created = false;
    let result = extract_single_file_inner(reader, expected_name, dest, &mut created);
    Extracted { result, created }
}

fn extract_single_file_inner<R: Read>(
    reader: R,
    expected_name: &str,
    dest: &Path,
    created: &mut bool,
) -> Result<u64, String> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| format!("could not read the tar archive returned by Docker: {e}"))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| format!("could not read the tar archive returned by Docker: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let entry_path = entry
            .path()
            .map_err(|e| format!("the tar archive returned by Docker has an unreadable path: {e}"))?
            .into_owned();
        let entry_name = entry_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if entry_name != expected_name {
            return Err(format!(
                "the tar archive returned by Docker contained '{}' instead of '{expected_name}'",
                entry_path.display()
            ));
        }
        let declared = entry
            .header()
            .size()
            .map_err(|e| format!("the tar entry for '{expected_name}' has an invalid size: {e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .map_err(|e| {
                format!(
                    "could not create '{}' on the Temps host: {e}",
                    dest.display()
                )
            })?;
        *created = true;
        let copied = std::io::copy(&mut entry, &mut file).map_err(|e| {
            format!(
                "copying '{expected_name}' into '{}' failed: {e}",
                dest.display()
            )
        })?;
        if copied != declared {
            return Err(format!(
                "the tar stream for '{expected_name}' ended after {copied} of {declared} bytes"
            ));
        }
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("could not flush '{}' to disk: {e}", dest.display()))?;
        return Ok(copied);
    }
    Err(format!(
        "the tar archive returned by Docker contained no regular file named '{expected_name}'"
    ))
}

/// Blocking `Read` over a bounded channel of chunks: the bridge between the
/// async Docker download and the synchronous `tar` reader. Ends (EOF) when
/// the sender is dropped; an `Err` chunk surfaces as a read error.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
    current: Bytes,
}

impl ChannelReader {
    fn new(rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Self {
        Self {
            rx,
            current: Bytes::new(),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.current.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.current = chunk,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len());
        buf[..n].copy_from_slice(&self.current[..n]);
        self.current.advance(n);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *data)
                .expect("append tar entry");
        }
        builder.into_inner().expect("finish tar")
    }

    // ── Tar extraction ───────────────────────────────────────────────────

    #[test]
    fn extracts_the_single_expected_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("dump.rdb.gz", b"payload-bytes")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert_eq!(extracted.result, Ok(13));
        assert!(extracted.created);
        assert_eq!(std::fs::read(&dest).expect("dest"), b"payload-bytes");
    }

    #[test]
    fn rejects_an_archive_holding_a_different_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("other.bin", b"x")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        let reason = extracted.result.expect_err("wrong entry");
        assert!(reason.contains("other.bin"), "{reason}");
        assert!(!extracted.created);
        assert!(!dest.exists());
    }

    #[test]
    fn rejects_an_archive_without_a_regular_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "dump.rdb.gz/", std::io::empty())
            .expect("dir entry");
        let archive = builder.into_inner().expect("finish");

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted
            .result
            .expect_err("no file")
            .contains("no regular file named 'dump.rdb.gz'"));
    }

    #[test]
    fn a_truncated_stream_fails_instead_of_returning_a_short_file() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let payload = vec![7_u8; 4096];
        let mut archive = tar_with(&[("dump.rdb.gz", payload.as_slice())]);
        // Header (512) plus half the payload: the stream ends mid-entry.
        archive.truncate(512 + 2048);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted.result.is_err(), "{:?}", extracted.result);
        assert!(extracted.created, "the partial file is ours to delete");
    }

    #[test]
    fn never_overwrites_or_claims_a_pre_existing_destination() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        std::fs::write(&dest, b"not ours").expect("pre-existing");
        let archive = tar_with(&[("dump.rdb.gz", b"new")]);

        let extracted = extract_single_file(Cursor::new(archive), "dump.rdb.gz", &dest);

        assert!(extracted.result.is_err());
        assert!(
            !extracted.created,
            "a file this call did not create must not be cleaned up by it"
        );
        assert_eq!(std::fs::read(&dest).expect("kept"), b"not ours");
    }

    // ── Channel bridge ───────────────────────────────────────────────────

    #[tokio::test]
    async fn channel_reader_streams_chunks_into_the_tar_reader() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let payload: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        let archive = tar_with(&[("dump.rdb.gz", payload.as_slice())]);

        let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
        let dest_for_task = dest.clone();
        let extractor = tokio::task::spawn_blocking(move || {
            extract_single_file(ChannelReader::new(rx), "dump.rdb.gz", &dest_for_task)
        });
        // Odd-sized chunks, so tar headers straddle chunk boundaries.
        for chunk in archive.chunks(1_337) {
            if tx.send(Ok(Bytes::copy_from_slice(chunk))).await.is_err() {
                break;
            }
        }
        drop(tx);

        let extracted = extractor.await.expect("join");
        assert_eq!(extracted.result, Ok(payload.len() as u64));
        assert_eq!(std::fs::read(&dest).expect("dest"), payload);
    }

    #[tokio::test]
    async fn channel_reader_surfaces_a_download_error() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out.gz");
        let archive = tar_with(&[("dump.rdb.gz", vec![1_u8; 8192].as_slice())]);

        let (tx, rx) = tokio::sync::mpsc::channel(DOWNLOAD_CHANNEL_CAPACITY);
        let dest_for_task = dest.clone();
        let extractor = tokio::task::spawn_blocking(move || {
            extract_single_file(ChannelReader::new(rx), "dump.rdb.gz", &dest_for_task)
        });
        tx.send(Ok(Bytes::copy_from_slice(&archive[..1024])))
            .await
            .expect("first chunk");
        tx.send(Err(std::io::Error::other("connection reset by daemon")))
            .await
            .expect("error chunk");
        drop(tx);

        let extracted = extractor.await.expect("join");
        let reason = extracted.result.expect_err("download broke");
        assert!(reason.contains("connection reset by daemon"), "{reason}");
    }
}
