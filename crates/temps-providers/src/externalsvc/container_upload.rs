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
//!
//! Uploads are bounded by a stall timeout, not a total deadline: a backup can
//! be arbitrarily large and the link to the daemon arbitrarily slow, so the
//! only signal that an upload is broken is that the daemon stops accepting
//! data. The body is pulled under backpressure, so every chunk pulled is a
//! chunk the daemon took. Once the last byte is sent the daemon may still be
//! writing the file out, so the wait for its answer gets its own window that
//! grows with the file's size.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;

/// Why a file could not be put into a container.
#[derive(Debug, thiserror::Error)]
pub enum ContainerUploadError {
    /// The host-side source file could not be opened or read.
    #[error("Failed to read upload source '{path}': {reason}")]
    Source { path: String, reason: String },

    /// The destination file name cannot be encoded in a tar header.
    #[error("Invalid upload file name '{name}': {reason}")]
    InvalidName { name: String, reason: String },

    /// The Docker daemon stopped accepting data for longer than the stall
    /// timeout, before the whole archive was sent.
    #[error(
        "Upload of '{dest_name}' into container {container_id} stalled: the Docker daemon \
         accepted no data for {stall_secs}s after {sent} of {size} bytes"
    )]
    Stalled {
        container_id: String,
        dest_name: String,
        size: u64,
        sent: u64,
        stall_secs: u64,
    },

    /// The whole archive was sent, but the Docker daemon did not confirm the
    /// upload within the window allowed for writing out a file this size.
    #[error(
        "Upload of '{dest_name}' into container {container_id} was not confirmed: the Docker \
         daemon received all {size} bytes but did not answer within {waited_secs}s"
    )]
    Unconfirmed {
        container_id: String,
        dest_name: String,
        size: u64,
        waited_secs: u64,
    },

    /// The Docker daemon rejected or aborted the upload.
    #[error(
        "Failed to upload '{dest_name}' ({size} bytes) into {container_id}:{dest_dir}: {reason}"
    )]
    Upload {
        container_id: String,
        dest_dir: String,
        dest_name: String,
        size: u64,
        reason: String,
    },
}

/// Read size when streaming a host file into a container upload.
const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;

/// How long the daemon may accept no data before an upload is abandoned.
const UPLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Slowest rate at which the daemon is assumed to write out an uploaded file
/// once it has received all of it. Sizes the wait for its answer.
const MIN_DAEMON_WRITE_BYTES_PER_SEC: u64 = 8 * 1024 * 1024;

/// How long to wait for the daemon's answer after the last byte of a `size`
/// byte upload: the stall timeout, plus the time to write the whole file out
/// at [`MIN_DAEMON_WRITE_BYTES_PER_SEC`] (the daemon normally extracts while
/// it receives, so this is a generous bound, not an expected duration).
fn confirmation_timeout(size: u64) -> Duration {
    UPLOAD_STALL_TIMEOUT + Duration::from_secs(size / MIN_DAEMON_WRITE_BYTES_PER_SEC)
}

/// Why an upload was abandoned by [`run_until_stalled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandoned {
    /// No chunk was taken for the stall timeout; `sent` bytes had gone out.
    Stalled { sent: u64 },
    /// Every byte was sent, but no answer came within the confirmation window.
    Unconfirmed { waited: Duration },
}

/// When an upload last made progress, how far it got, and whether the whole
/// body has been sent.
#[derive(Clone)]
struct UploadProgress {
    started: tokio::time::Instant,
    /// Milliseconds after `started` at which the last chunk was taken (or the
    /// body ended).
    last_progress_ms: Arc<AtomicU64>,
    sent: Arc<AtomicU64>,
    body_sent: Arc<AtomicBool>,
}

impl UploadProgress {
    fn new() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            last_progress_ms: Arc::new(AtomicU64::new(0)),
            sent: Arc::new(AtomicU64::new(0)),
            body_sent: Arc::new(AtomicBool::new(false)),
        }
    }

    fn touch(&self) {
        let elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_progress_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    fn record(&self, bytes: usize) {
        self.touch();
        self.sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// The body is exhausted: from now on the upload is waiting for the
    /// daemon's answer, not for it to take more data.
    fn finish_body(&self) {
        self.touch();
        self.body_sent.store(true, Ordering::Relaxed);
    }

    fn body_sent(&self) -> bool {
        self.body_sent.load(Ordering::Relaxed)
    }

    fn last_progress(&self) -> tokio::time::Instant {
        self.started + Duration::from_millis(self.last_progress_ms.load(Ordering::Relaxed))
    }

    fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
}

/// Wrap `stream` so every chunk taken from it, and the end of the body, is
/// recorded in `progress`.
fn track_progress<S>(
    stream: S,
    progress: UploadProgress,
) -> impl futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    use futures::{StreamExt, TryStreamExt};
    let at_end = progress.clone();
    stream
        .inspect_ok(move |chunk| progress.record(chunk.len()))
        .chain(futures::stream::poll_fn(move |_| {
            at_end.finish_body();
            std::task::Poll::Ready(None)
        }))
}

/// Drive `upload` to completion with two bounds instead of a total deadline:
/// while the body is being sent, give up after `stall` without a chunk taken;
/// once it is all sent, give up after `confirmation` without an answer. A slow
/// upload that keeps moving is never cut off, however long it takes in total,
/// and a daemon still writing out a large file gets time to finish.
async fn run_until_stalled<F, T>(
    upload: F,
    progress: &UploadProgress,
    stall: Duration,
    confirmation: Duration,
) -> Result<T, Abandoned>
where
    F: std::future::Future<Output = T>,
{
    let window = |progress: &UploadProgress| {
        if progress.body_sent() {
            confirmation
        } else {
            stall
        }
    };
    tokio::pin!(upload);
    loop {
        let deadline = progress.last_progress() + window(progress);
        tokio::select! {
            output = &mut upload => return Ok(output),
            _ = tokio::time::sleep_until(deadline) => {
                let window = window(progress);
                if progress.last_progress() + window <= tokio::time::Instant::now() {
                    return Err(if progress.body_sent() {
                        Abandoned::Unconfirmed { waited: window }
                    } else {
                        Abandoned::Stalled { sent: progress.sent() }
                    });
                }
            }
        }
    }
}

/// The tar stream that uploads one regular file named `name` of `size`
/// bytes with permission bits `mode`: header, then `body`, then padding and
/// the end-of-archive marker. Built lazily, so memory does not depend on
/// `size`.
pub(crate) fn single_file_tar_stream<S>(
    name: &str,
    size: u64,
    mode: u32,
    body: S,
) -> Result<
    impl futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
    ContainerUploadError,
>
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    use futures::StreamExt;

    let mut header = tar::Header::new_gnu();
    header
        .set_path(name)
        .map_err(|e| ContainerUploadError::InvalidName {
            name: name.to_string(),
            reason: e.to_string(),
        })?;
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
    tar_stream: S,
) -> Result<(), ContainerUploadError>
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    let progress = UploadProgress::new();
    let confirmation = confirmation_timeout(size);
    run_until_stalled(
        docker.upload_to_container(
            container_id,
            Some(bollard::query_parameters::UploadToContainerOptions {
                path: dest_dir.to_string(),
                ..Default::default()
            }),
            bollard::body_try_stream(track_progress(tar_stream, progress.clone())),
        ),
        &progress,
        UPLOAD_STALL_TIMEOUT,
        confirmation,
    )
    .await
    .map_err(|abandoned| match abandoned {
        Abandoned::Stalled { sent } => ContainerUploadError::Stalled {
            container_id: container_id.to_string(),
            dest_name: dest_name.to_string(),
            size,
            sent,
            stall_secs: UPLOAD_STALL_TIMEOUT.as_secs(),
        },
        Abandoned::Unconfirmed { waited } => ContainerUploadError::Unconfirmed {
            container_id: container_id.to_string(),
            dest_name: dest_name.to_string(),
            size,
            waited_secs: waited.as_secs(),
        },
    })?
    .map_err(|e| ContainerUploadError::Upload {
        container_id: container_id.to_string(),
        dest_dir: dest_dir.to_string(),
        dest_name: dest_name.to_string(),
        size,
        reason: e.to_string(),
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
) -> Result<(), ContainerUploadError> {
    let source_error = |e: std::io::Error| ContainerUploadError::Source {
        path: host_path.display().to_string(),
        reason: e.to_string(),
    };
    let file = tokio::fs::File::open(host_path)
        .await
        .map_err(source_error)?;
    let size = file.metadata().await.map_err(source_error)?.len();
    // A read error on the host file mid-stream reaches the daemon as an
    // aborted upload; remember it so it is reported as what it is.
    let source_failure = Arc::new(Mutex::new(None));
    let body = file_chunks(file, size, Arc::clone(&source_failure));
    let tar_stream = single_file_tar_stream(dest_name, size, mode, body)?;
    let uploaded =
        upload_tar_stream(docker, container_id, dest_dir, dest_name, size, tar_stream).await;
    uploaded.map_err(|error| attribute_upload_failure(error, &source_failure, host_path))
}

/// Stream `size` bytes of `file` in chunks. A read failure is recorded in
/// `failure` (as well as ending the stream) so the caller can tell a bad
/// source from a Docker-side failure.
fn file_chunks(
    file: tokio::fs::File,
    size: u64,
    failure: Arc<Mutex<Option<String>>>,
) -> impl futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static {
    futures::stream::try_unfold(
        (file, size, failure),
        |(mut file, remaining, failure)| async move {
            use tokio::io::AsyncReadExt;
            if remaining == 0 {
                return Ok(None);
            }
            let want = remaining.min(UPLOAD_CHUNK_BYTES as u64) as usize;
            let mut buf = vec![0_u8; want];
            if let Err(e) = file.read_exact(&mut buf).await {
                if let Ok(mut slot) = failure.lock() {
                    *slot = Some(format!(
                        "read failed with {} bytes still expected: {}",
                        remaining, e
                    ));
                }
                return Err(e);
            }
            Ok(Some((
                bytes::Bytes::from(buf),
                (file, remaining - want as u64, failure),
            )))
        },
    )
}

/// Report a failed upload as a source error when reading the host file is
/// what broke it, so the message names the file instead of the daemon.
fn attribute_upload_failure(
    error: ContainerUploadError,
    source_failure: &Mutex<Option<String>>,
    host_path: &std::path::Path,
) -> ContainerUploadError {
    let recorded = source_failure.lock().ok().and_then(|mut slot| slot.take());
    match recorded {
        Some(reason) => ContainerUploadError::Source {
            path: host_path.display().to_string(),
            reason,
        },
        None => error,
    }
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
) -> Result<(), ContainerUploadError> {
    let size = contents.len() as u64;
    let body = futures::stream::once(async move { Ok(bytes::Bytes::from(contents)) });
    let tar_stream = single_file_tar_stream(dest_name, size, mode, body)?;
    upload_tar_stream(docker, container_id, dest_dir, dest_name, size, tar_stream).await
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

    #[tokio::test]
    async fn a_missing_source_is_a_typed_source_error() {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(_) => {
                println!("Docker client unavailable, skipping");
                return;
            }
        };
        let missing = std::path::Path::new("/nonexistent/temps-upload-source.gz");
        let err = upload_file_to_container(&docker, "unused", missing, "/tmp", "archive.gz", 0o644)
            .await
            .expect_err("a missing source must fail before contacting Docker");
        assert!(
            matches!(&err, ContainerUploadError::Source { path, .. } if path.contains("temps-upload-source")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_source_that_shrinks_mid_stream_is_reported_as_a_source_error() {
        use futures::StreamExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("restore.rdb");
        std::fs::write(&path, vec![7_u8; 1_000]).expect("write source");
        let file = tokio::fs::File::open(&path).await.expect("open source");
        let failure = Arc::new(Mutex::new(None));

        // Claim more bytes than the file holds: the read hits EOF exactly as
        // it would if the file were truncated after its size was checked.
        let mut chunks = Box::pin(file_chunks(file, 5_000, Arc::clone(&failure)));
        let mut saw_error = false;
        while let Some(chunk) = chunks.next().await {
            if chunk.is_err() {
                saw_error = true;
                break;
            }
        }
        assert!(
            saw_error,
            "a short source must end the stream with an error"
        );

        let attributed = attribute_upload_failure(
            ContainerUploadError::Upload {
                container_id: "helper".into(),
                dest_dir: "/tmp".into(),
                dest_name: "restore.rdb".into(),
                size: 5_000,
                reason: "error trying to connect".into(),
            },
            &failure,
            &path,
        );
        match attributed {
            ContainerUploadError::Source {
                path: reported,
                reason,
            } => {
                assert!(reported.ends_with("restore.rdb"), "{reported}");
                assert!(reason.contains("5000 bytes still expected"), "{reason}");
            }
            other => panic!("expected a source error, got {other:?}"),
        }
    }

    #[test]
    fn a_docker_side_failure_stays_an_upload_error() {
        let untouched = Mutex::new(None);
        let attributed = attribute_upload_failure(
            ContainerUploadError::Stalled {
                container_id: "helper".into(),
                dest_name: "archive.gz".into(),
                size: 10,
                sent: 4,
                stall_secs: 1,
            },
            &untouched,
            std::path::Path::new("/tmp/archive.gz"),
        );
        assert!(matches!(attributed, ContainerUploadError::Stalled { .. }));
    }

    /// Drain `stream` like the daemon would, taking one chunk every `pace`.
    async fn drain_slowly<S>(stream: S, pace: Duration) -> u64
    where
        S: futures::Stream<Item = std::io::Result<bytes::Bytes>>,
    {
        use futures::StreamExt;
        tokio::pin!(stream);
        let mut taken = 0_u64;
        while let Some(Ok(chunk)) = stream.next().await {
            taken += chunk.len() as u64;
            tokio::time::sleep(pace).await;
        }
        taken
    }

    #[tokio::test]
    async fn a_slow_upload_that_keeps_moving_is_never_cut_off() {
        let stall = Duration::from_millis(200);
        let progress = UploadProgress::new();
        // 12 chunks, one every 50ms: 600ms in total, three times the stall
        // timeout, but never 200ms without progress.
        let chunks = futures::stream::iter(
            (0..12).map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(&[7_u8; 100]))),
        );
        let started = tokio::time::Instant::now();

        let outcome = run_until_stalled(
            drain_slowly(
                track_progress(chunks, progress.clone()),
                Duration::from_millis(50),
            ),
            &progress,
            stall,
            stall,
        )
        .await;

        assert_eq!(outcome, Ok(1_200));
        assert!(
            started.elapsed() > stall * 2,
            "the test must outlast the stall timeout"
        );
    }

    #[tokio::test]
    async fn an_upload_that_stops_moving_fails_with_the_bytes_sent() {
        use futures::StreamExt;
        let stall = Duration::from_millis(150);
        let progress = UploadProgress::new();
        // Two chunks, then the source never yields again.
        let chunks = futures::stream::iter(
            (0..2).map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(&[1_u8; 64]))),
        )
        .chain(futures::stream::pending());

        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            run_until_stalled(
                drain_slowly(
                    track_progress(chunks, progress.clone()),
                    Duration::from_millis(1),
                ),
                &progress,
                stall,
                Duration::from_secs(60),
            ),
        )
        .await
        .expect("a stalled upload must be abandoned, not hang");

        assert_eq!(outcome, Err(Abandoned::Stalled { sent: 128 }));
    }

    /// Send every chunk at once, then answer only after `answer_after`, like
    /// a daemon that is still writing out a large file.
    async fn send_then_answer<S>(stream: S, answer_after: Duration) -> u64
    where
        S: futures::Stream<Item = std::io::Result<bytes::Bytes>>,
    {
        let taken = drain_slowly(stream, Duration::ZERO).await;
        tokio::time::sleep(answer_after).await;
        taken
    }

    #[tokio::test]
    async fn a_daemon_still_writing_out_the_file_is_given_time_to_answer() {
        let stall = Duration::from_millis(100);
        let progress = UploadProgress::new();
        let chunks = futures::stream::iter(
            (0..3).map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(&[2_u8; 10]))),
        );

        // The answer comes 4x the stall timeout after the last byte, inside
        // the confirmation window.
        let outcome = run_until_stalled(
            send_then_answer(
                track_progress(chunks, progress.clone()),
                Duration::from_millis(400),
            ),
            &progress,
            stall,
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(outcome, Ok(30));
    }

    #[tokio::test]
    async fn a_daemon_that_never_answers_after_the_last_byte_is_unconfirmed() {
        let progress = UploadProgress::new();
        let chunks = futures::stream::iter(
            (0..3).map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(&[3_u8; 10]))),
        );
        let confirmation = Duration::from_millis(300);

        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            run_until_stalled(
                send_then_answer(
                    track_progress(chunks, progress.clone()),
                    Duration::from_secs(3600),
                ),
                &progress,
                Duration::from_millis(100),
                confirmation,
            ),
        )
        .await
        .expect("an unanswered upload must be abandoned, not hang");

        assert_eq!(
            outcome,
            Err(Abandoned::Unconfirmed {
                waited: confirmation
            })
        );
    }

    #[test]
    fn the_confirmation_window_grows_with_the_file() {
        assert_eq!(confirmation_timeout(0), UPLOAD_STALL_TIMEOUT);
        // 80 GiB at 8 MiB/s is 10240s on top of the stall timeout.
        assert_eq!(
            confirmation_timeout(80 * 1024 * 1024 * 1024),
            UPLOAD_STALL_TIMEOUT + Duration::from_secs(10_240)
        );
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

        let file_upload = match wrote {
            Ok(()) => {
                upload_file_to_container(
                    &docker,
                    &created.id,
                    &host_file,
                    "/tmp",
                    "archive.gz",
                    0o644,
                )
                .await
            }
            Err(e) => Err(ContainerUploadError::Source {
                path: host_file.display().to_string(),
                reason: e.to_string(),
            }),
        };
        let bytes_upload = upload_bytes_to_container(
            &docker,
            &created.id,
            b"password: 'x'\n".to_vec(),
            "/tmp",
            "restore.yaml",
            0o600,
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
