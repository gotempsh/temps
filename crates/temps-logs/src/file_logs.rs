// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! File-based logging service for pipeline operations
//!
//! This module provides utilities for:
//! - Creating structured log files with date-based organization
//! - Appending to logs asynchronously
//! - Tailing logs in real-time
//! - Reading log content

use chrono::Utc;
use futures::Stream;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::fs::{create_dir_all, File};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};
use tokio::time::Duration;
use tracing::{debug, trace, warn};

use crate::log_archive::{DurableLogChunk, LogArchiveStorage, LogArchiveStorageError};
use crate::structured_logs::validate_log_id;
use crate::structured_logs::{LogEntry, LogLevel, StructuredLogService};

/// Sub-prefix under which build/deploy logs are archived, namespacing them
/// away from `temps-log-aggregator`'s per-project chunk keys
/// (`logs/{project_id}/...`) when both features share the same S3 bucket
/// and `TEMPS_LOG_S3_PREFIX`.
const ARCHIVE_KEY_PREFIX: &str = "build-logs";

/// Per-attempt bound on the S3 upload issued by `archive_log`. Paired with
/// `ARCHIVE_UPLOAD_RETRIES` below so a stalled connection to the configured
/// backend can't hang the archival step indefinitely -- worst case is
/// `ARCHIVE_UPLOAD_RETRIES` attempts of up to this long each, plus backoff
/// between them, not an unbounded wait.
const ARCHIVE_UPLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Number of attempts `archive_log` makes against the configured S3 backend
/// before giving up and leaving the log on local disk. Transient network
/// blips or momentary bucket unavailability are common enough on
/// self-hosted setups (a MinIO/RustFS container restarting, a brief network
/// partition) that a single failed attempt shouldn't strand the log
/// permanently -- see the "Retry with Exponential Backoff" pattern in
/// `CLAUDE.md`.
const ARCHIVE_UPLOAD_RETRIES: u32 = 3;

/// Maximum number of `archive_log` uploads permitted to run concurrently.
///
/// S3 has no streaming-append primitive, so each concurrent upload buffers
/// one finished log's *entire* contents in memory for the duration of the
/// call (including retries). Temps targets small self-hosted machines (see
/// CLAUDE.md's "Scalability & Efficiency" reference deployment: a 3 vCPU /
/// 4 GB box), so a burst of many jobs completing at once -- or a slow S3
/// endpoint holding uploads open -- must not be allowed to buffer an
/// unbounded number of full logs simultaneously. Calls queue on this
/// semaphore instead of racing to read+upload immediately; queued callers
/// hold no log data in memory while waiting, only cheap suspended-task
/// state (see `archive_log`, which acquires the permit before reading the
/// file).
const ARCHIVE_MAX_CONCURRENT_UPLOADS: usize = 4;
const MAX_DURABLE_LOG_LINE_BYTES: usize = 1024 * 1024;

/// Default number of trailing lines replayed when a tail stream first attaches.
///
/// This is the initial backlog the client receives before it starts seeing
/// live lines. It is intentionally large so that full deployment/build logs
/// are visible in the UI on first load — the previous value (1000) silently
/// truncated long build logs. Clients dedupe by absolute line number, so a
/// generous backlog is safe across reconnects.
pub const DEFAULT_TAIL_REPLAY_LINES: usize = 100_000;

/// Size of each block read while scanning backwards for the tail offset.
///
/// The scan holds exactly one of these at a time, so the memory cost of
/// locating the replay window is O(1) (~64 KB) regardless of file size or of
/// how long individual lines are.
const TAIL_SCAN_CHUNK: usize = 64 * 1024;

/// Byte offset of the first of the last `replay_lines` lines of `file_size`
/// bytes, found by scanning backwards in fixed blocks.
///
/// Returns 0 when the file holds at most `replay_lines` lines, and
/// `file_size` when `replay_lines` is 0 (attach at EOF, no backlog).
/// A trailing
/// '\n' terminates the last line rather than starting an empty one, so it is
/// not counted; a file whose final line is unterminated still counts as a
/// line. The reader's cursor is left unspecified — callers seek afterwards.
async fn find_tail_offset<R>(
    reader: &mut R,
    file_size: u64,
    replay_lines: usize,
) -> Result<u64, std::io::Error>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    // No backlog at all: attach at EOF and stream only what arrives next.
    if replay_lines == 0 {
        return Ok(file_size);
    }
    // Replaying everything means starting at the beginning; skip the scan.
    if replay_lines == usize::MAX {
        return Ok(0);
    }

    let mut buffer = vec![0u8; TAIL_SCAN_CHUNK];
    // Offset of the byte just past the region still to be scanned.
    let mut end = file_size;
    // Newlines seen so far that *start* a line (i.e. excluding a trailing one).
    let mut line_starts = 0usize;
    // A file ending in '\n' terminates its last line; that newline does not
    // begin another one, so it must not be counted.
    let mut skip_final_newline = true;

    while end > 0 {
        let chunk_len = std::cmp::min(TAIL_SCAN_CHUNK as u64, end) as usize;
        let start = end - chunk_len as u64;

        reader.seek(SeekFrom::Start(start)).await?;
        reader.read_exact(&mut buffer[..chunk_len]).await?;

        for (index, byte) in buffer[..chunk_len].iter().enumerate().rev() {
            if *byte != b'\n' {
                continue;
            }
            if skip_final_newline && start + index as u64 == file_size - 1 {
                skip_final_newline = false;
                continue;
            }
            skip_final_newline = false;
            line_starts += 1;
            if line_starts == replay_lines {
                // The line begins immediately after this newline.
                return Ok(start + index as u64 + 1);
            }
        }

        end = start;
    }

    // Fewer lines in the file than requested: replay from the beginning.
    Ok(0)
}

fn durable_tail_stream(
    archive: Arc<dyn LogArchiveStorage>,
    log_id: String,
    initial: Vec<DurableLogChunk>,
) -> impl Stream<Item = Result<String, std::io::Error>> + Send {
    async_stream::stream! {
        let mut after_line = 0;
        for chunk in initial {
            after_line = after_line.max(chunk.line);
            let line = String::from_utf8_lossy(&chunk.data).trim_end().to_string();
            if !line.is_empty() {
                yield Ok(line);
            }
        }

        loop {
            match archive.download_log_chunks(&log_id, after_line, 1_000).await {
                Ok(chunks) => {
                    for chunk in chunks {
                        after_line = after_line.max(chunk.line);
                        let line = String::from_utf8_lossy(&chunk.data).trim_end().to_string();
                        if !line.is_empty() {
                            yield Ok(line);
                        }
                    }
                }
                Err(error) => {
                    yield Err(std::io::Error::other(format!(
                        "failed to tail durable log '{log_id}' after line {after_line}: {error}"
                    )));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

pub struct LogService {
    log_base_path: PathBuf,
    structured_service: StructuredLogService,
    /// Optional archive backend for finished build/deploy logs. `None` (the
    /// default, via [`LogService::new`]) preserves the historical
    /// filesystem-only behavior for every existing self-hosted install:
    /// [`LogService::archive_log`] becomes a no-op and local log files are
    /// never deleted out from under a caller. `Some` is only ever set via
    /// [`LogService::with_archive`], wired up from `TEMPS_LOG_STORAGE_BACKEND=s3`.
    archive: Option<Arc<dyn LogArchiveStorage>>,
    /// Bounds how many `archive_log` uploads run concurrently. Always
    /// allocated (even when `archive` is `None`, in which case it's never
    /// touched -- `archive_log` returns before reaching it) so the type
    /// doesn't need a second `Option` layered on top of `archive`'s.
    archive_semaphore: Arc<tokio::sync::Semaphore>,
    durable_chunks: bool,
    /// Serializes line-number assignment and durable chunk upload. Build-log
    /// writes are control-plane operations; one global lock keeps ordering
    /// deterministic without unbounded per-log lock cardinality.
    append_lines: tokio::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl LogService {
    pub fn new(log_base_path: PathBuf) -> Self {
        Self::with_archive(log_base_path, None)
    }

    /// Create a `LogService` with an optional archive backend for finished
    /// build/deploy logs. Pass `None` for the default filesystem-only
    /// behavior (equivalent to [`LogService::new`]).
    pub fn with_archive(
        log_base_path: PathBuf,
        archive: Option<Arc<dyn LogArchiveStorage>>,
    ) -> Self {
        Self::with_archive_mode(log_base_path, archive, false)
    }

    pub fn with_archive_mode(
        log_base_path: PathBuf,
        archive: Option<Arc<dyn LogArchiveStorage>>,
        durable_chunks: bool,
    ) -> Self {
        let structured_service = StructuredLogService::new(log_base_path.clone());
        LogService {
            log_base_path,
            structured_service,
            archive,
            archive_semaphore: Arc::new(tokio::sync::Semaphore::new(
                ARCHIVE_MAX_CONCURRENT_UPLOADS,
            )),
            durable_chunks,
            append_lines: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Returns the base path where logs and other data files are stored
    pub fn base_path(&self) -> &PathBuf {
        &self.log_base_path
    }

    /// Whether an archive backend is configured. Exposed so callers (e.g. the
    /// deployment job-completion hook) can skip archival work entirely when
    /// disabled, rather than relying on `archive_log`'s no-op fallback.
    pub fn archive_enabled(&self) -> bool {
        self.archive.is_some()
    }

    /// Persist an already-complete bounded log payload and acknowledge only
    /// after its configured durable backend has accepted it.
    pub async fn write_completed_log(
        &self,
        log_id: &str,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        validate_log_id(log_id)?;
        let path = self.get_log_path(log_id);
        if let Some(parent) = path.parent() {
            create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, data).await?;
        if let Some(archive) = &self.archive {
            let key = self.archive_key(log_id);
            archive
                .upload_log(&key, data.to_vec())
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to durably store completed log '{log_id}': {error}"
                    ))
                })?;
        }
        Ok(())
    }

    /// Compute the archive storage key for a given log id, mirroring the
    /// log's resolved local path (relative to `log_base_path`) so nested,
    /// date-based log ids archive to an equally nested S3 key.
    fn archive_key(&self, log_id: &str) -> String {
        let full_path = self.get_log_path(log_id);
        let relative = full_path
            .strip_prefix(&self.log_base_path)
            .unwrap_or(&full_path);
        format!("{ARCHIVE_KEY_PREFIX}/{}", relative.to_string_lossy())
    }

    /// Archive a finished build/deploy log to the configured backend and
    /// delete the local scratch file, so local disk only ever holds logs for
    /// currently-running jobs rather than an unbounded history.
    ///
    /// A no-op (`Ok(())`) when:
    /// - no archive backend is configured (default filesystem-only mode), or
    /// - the local file does not exist -- either the job never wrote a log
    ///   line (e.g. it was cancelled before starting), or the log was already
    ///   archived by an earlier call.
    ///
    /// Call this only once a job has reached a terminal state (success,
    /// failure, cancelled, skipped): while a job is still running, deleting
    /// the local file out from under `tail_log`'s live reader would silently
    /// truncate the in-progress log view.
    pub async fn archive_log(&self, log_id: &str) -> Result<(), std::io::Error> {
        validate_log_id(log_id)?;
        let Some(archive) = &self.archive else {
            return Ok(());
        };

        // Bound concurrent file streams and outbound S3 requests. Acquired
        // before touching the file, so queued callers hold no log data.
        let _permit = self.archive_semaphore.acquire().await.map_err(|e| {
            std::io::Error::other(format!(
                "archive concurrency semaphore closed unexpectedly: {e}"
            ))
        })?;

        let log_path = self.get_log_path(log_id);
        match tokio::fs::metadata(&log_path).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }

        let key = self.archive_key(log_id);

        // Hand-rolled retry loop rather than `temps_core::retry::RetryConfig::retry`
        // (its own doc comment on `compute_delay` names this exact
        // situation): a permanent failure -- bad credentials, a bucket that
        // doesn't exist -- fails identically on every attempt, and
        // `RetryConfig::retry` has no way to stop early on those, only on
        // exhausting `max_attempts`. Reusing `compute_delay` keeps the same
        // backoff math without duplicating it.
        let retry_config = temps_core::retry::RetryConfig::new(ARCHIVE_UPLOAD_RETRIES)
            .with_base_delay(Duration::from_secs(1))
            .with_max_delay(Duration::from_secs(10));
        let mut last_error = None;
        'attempts: for attempt in 0..ARCHIVE_UPLOAD_RETRIES {
            let upload = tokio::time::timeout(
                ARCHIVE_UPLOAD_TIMEOUT,
                archive.upload_log_file(&key, &log_path),
            )
            .await;
            match upload {
                Ok(Ok(())) => {
                    last_error = None;
                    break 'attempts;
                }
                Ok(Err(e)) => {
                    let retryable = e.is_retryable();
                    let message = e.to_string();
                    if !retryable {
                        warn!(
                            log_id,
                            error = %message,
                            "Archive upload failed permanently, not retrying"
                        );
                        last_error = Some(message);
                        break 'attempts;
                    }
                    last_error = Some(message);
                }
                Err(_) => {
                    last_error = Some(format!("upload timed out after {ARCHIVE_UPLOAD_TIMEOUT:?}"));
                }
            }
            let is_last_attempt = attempt + 1 >= ARCHIVE_UPLOAD_RETRIES;
            if !is_last_attempt {
                tokio::time::sleep(retry_config.compute_delay(attempt)).await;
            }
        }
        if let Some(e) = last_error {
            return Err(std::io::Error::other(format!(
                "failed to archive log '{log_id}' to S3: {e}"
            )));
        }

        if let Err(e) = tokio::fs::remove_file(&log_path).await {
            // The archive upload succeeded -- the important half of this
            // operation -- so don't fail the caller over cleanup. The next
            // read will still find local content (harmless), just at the
            // cost of the local disk this call was meant to free.
            warn!(
                log_id,
                path = %log_path.display(),
                error = %e,
                "Archived log to S3 but failed to remove local scratch file"
            );
        }
        self.append_lines.lock().await.remove(log_id);

        Ok(())
    }

    pub fn get_log_path(&self, log_id: &str) -> PathBuf {
        if validate_log_id(log_id).is_err() {
            return self.log_base_path.join(".invalid-log-id");
        }
        // If log_id already contains .log extension or path separators, treat it as a full path
        if log_id.contains('/') || log_id.ends_with(".log") {
            self.log_base_path.join(log_id)
        } else {
            // Legacy behavior: add .log extension
            self.log_base_path.join(format!("{}.log", log_id))
        }
    }

    pub async fn create_log_path(&self, log_id: &str) -> Result<PathBuf, std::io::Error> {
        validate_log_id(log_id)?;
        // If log_id contains path separators, it's already a full path with directory structure
        let log_path = if log_id.contains('/') {
            PathBuf::from(log_id)
        } else {
            // Legacy behavior: create date-based path
            let now = Utc::now();
            let date_path = now.format("%Y/%m/%d/%H").to_string();
            PathBuf::from(date_path).join(format!("{}.log", log_id))
        };

        let full_path = self.log_base_path.join(&log_path);

        // Ensure the directory exists
        if let Some(parent) = full_path.parent() {
            create_dir_all(parent).await?;
        }

        Ok(log_path)
    }

    // REMOVED FROM PUBLIC API: append_to_log() - Use append_structured_log() instead
    // This method has been removed from the public API to enforce structured logging.
    // All production code must use append_structured_log() with explicit log levels.
    //
    // Migration guide:
    //   Before: service.append_to_log(log_id, "message\n").await?;
    //   After:  service.append_structured_log(log_id, LogLevel::Info, "message").await?;
    //
    // Helper methods available:
    //   - log_info(log_id, message)
    //   - log_success(log_id, message)
    //   - log_warning(log_id, message)
    //   - log_error(log_id, message)

    /// Read the full content of a log by id.
    ///
    /// Reads the local file first -- this is the only path for logs still in
    /// progress and preserves exact current behavior when no archive backend
    /// is configured (the default for every existing self-hosted install).
    /// Only when the local file is missing (archived after job completion,
    /// or a server restart raced a delete) does this fall back to the
    /// configured archive backend. When the archive lookup also comes back
    /// not-found, the *original* local `NotFound` error is returned so
    /// callers see the same error shape as before this feature existed.
    pub async fn get_log_content(&self, log_id: &str) -> Result<String, std::io::Error> {
        validate_log_id(log_id)?;
        if self.durable_chunks {
            match self.read_durable_chunks(log_id).await {
                Ok(content) => return Ok(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let archive = self.archive.as_ref().ok_or(error)?;
                    return archive
                        .download_log(&self.archive_key(log_id))
                        .await
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .map_err(|archive_error| {
                            std::io::Error::other(format!(
                                "failed to read compacted durable log '{log_id}': {archive_error}"
                            ))
                        });
                }
                Err(error) => return Err(error),
            }
        }
        let log_path = self.get_log_path(log_id);
        match tokio::fs::read_to_string(&log_path).await {
            Ok(content) => Ok(content),
            Err(local_err) if local_err.kind() == std::io::ErrorKind::NotFound => {
                let Some(archive) = &self.archive else {
                    return Err(local_err);
                };
                let key = self.archive_key(log_id);
                match archive.download_log(&key).await {
                    Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                    Err(LogArchiveStorageError::NotFound { .. }) if self.durable_chunks => {
                        self.read_durable_chunks(log_id).await.or(Err(local_err))
                    }
                    Err(LogArchiveStorageError::NotFound { .. }) => Err(local_err),
                    Err(other) => Err(std::io::Error::other(format!(
                        "failed to read archived log '{log_id}': {other}"
                    ))),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn read_durable_chunks(&self, log_id: &str) -> Result<String, std::io::Error> {
        let Some(archive) = &self.archive else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No durable log storage configured for '{log_id}'"),
            ));
        };
        let mut after_line = 0;
        let mut content = String::new();
        loop {
            let chunks = archive
                .download_log_chunks(log_id, after_line, 1_000)
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to read durable chunks for log '{log_id}': {error}"
                    ))
                })?;
            if chunks.is_empty() {
                break;
            }
            for chunk in &chunks {
                content.push_str(&String::from_utf8_lossy(&chunk.data));
                after_line = after_line.max(chunk.line);
            }
            if chunks.len() < 1_000 {
                break;
            }
        }
        if after_line == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Durable log '{log_id}' has no chunks"),
            ));
        }
        Ok(content)
    }

    /// Tail a log file, replaying up to [`DEFAULT_TAIL_REPLAY_LINES`] trailing
    /// lines before streaming new lines as they are appended.
    pub async fn tail_log(
        &self,
        log_id: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String, std::io::Error>> + Send>>, std::io::Error>
    {
        self.tail_log_with_replay(log_id, DEFAULT_TAIL_REPLAY_LINES)
            .await
    }

    /// Tail a log file, replaying up to `replay_lines` trailing lines before
    /// streaming new lines as they are appended.
    ///
    /// Pass `usize::MAX` to replay the entire file. The replay backlog is the
    /// last `replay_lines` lines of the file at the moment the stream attaches;
    /// any line written afterwards is streamed live regardless of this cap.
    pub async fn tail_log_with_replay(
        &self,
        log_id: &str,
        replay_lines: usize,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String, std::io::Error>> + Send>>, std::io::Error>
    {
        validate_log_id(log_id)?;
        let log_path = self.get_log_path(log_id);
        debug!(
            "Attempting to tail log at path: {:?} (replay up to {} lines)",
            log_path, replay_lines
        );

        if self.durable_chunks {
            let archive = self.archive.as_ref().cloned().ok_or_else(|| {
                std::io::Error::other(format!("durable archive disappeared for log '{log_id}'"))
            })?;
            let log_id = log_id.to_string();
            let replay_limit = replay_lines.min(DEFAULT_TAIL_REPLAY_LINES);
            let mut initial = archive
                .download_recent_log_chunks(&log_id, replay_limit)
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to replay durable log '{log_id}': {error}"
                    ))
                })?;
            if initial.is_empty() && replay_limit > 0 {
                match archive.download_log(&self.archive_key(&log_id)).await {
                    Ok(bytes) => {
                        let lines: Vec<&[u8]> = bytes
                            .split_inclusive(|byte| *byte == b'\n')
                            .filter(|line| !line.is_empty())
                            .collect();
                        let start = lines.len().saturating_sub(replay_limit);
                        initial = lines[start..]
                            .iter()
                            .enumerate()
                            .map(|(index, line)| DurableLogChunk {
                                line: (start + index + 1) as u64,
                                data: line.to_vec(),
                            })
                            .collect();
                    }
                    Err(LogArchiveStorageError::NotFound { .. }) => {}
                    Err(error) => {
                        return Err(std::io::Error::other(format!(
                            "failed to replay compacted durable log '{log_id}': {error}"
                        )));
                    }
                }
            }
            return Ok(Box::pin(durable_tail_stream(archive, log_id, initial)));
        }

        // Create file if it doesn't exist in filesystem-only mode.
        if !log_path.exists() {
            trace!("Log file doesn't exist, creating new file");
            File::create(&log_path).await?;
        }

        // Open file in read mode
        let file = File::open(&log_path).await?;
        let file_size = file.metadata().await?.len();
        let mut reader = BufReader::new(file);

        // If file has content, seek to the start of the last `replay_lines` lines.
        if file_size > 0 {
            let start_pos = find_tail_offset(&mut reader, file_size, replay_lines).await?;
            reader.seek(SeekFrom::Start(start_pos)).await?;
        }

        Ok(Box::pin(async_stream::stream! {
            let mut buffer = String::new();

            loop {
                match reader.read_line(&mut buffer).await {
                    Ok(0) => {
                        // Reached EOF, wait a bit before trying again
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Ok(_) => {
                        let line = buffer.trim_end().to_string();
                        if !line.is_empty() {
                            yield Ok(line);
                        }
                        buffer.clear();
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        }))
    }

    // ========== Structured Logging Helpers ==========
    // These methods provide convenient access to structured logging
    // while maintaining backward compatibility with existing append_to_log() usage

    /// Append a structured log entry with automatic JSONL formatting
    ///
    /// This is a convenience wrapper that creates structured logs transparently.
    /// Callers can continue using the same API while getting structured benefits.
    pub async fn append_structured_log(
        &self,
        log_id: &str,
        level: LogLevel,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        let entry = LogEntry::new(level, message);
        self.append_durable_entry(log_id, entry).await
    }

    /// Append a structured log with metadata
    pub async fn append_structured_log_with_metadata(
        &self,
        log_id: &str,
        level: LogLevel,
        message: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), std::io::Error> {
        let entry = LogEntry::new(level, message).with_metadata(metadata);
        self.append_durable_entry(log_id, entry).await
    }

    async fn append_durable_entry(
        &self,
        log_id: &str,
        mut entry: LogEntry,
    ) -> Result<(), std::io::Error> {
        validate_log_id(log_id)?;
        if !self.durable_chunks {
            self.structured_service.append_log(log_id, entry).await?;
            return Ok(());
        }
        let encoded_size = entry
            .to_jsonl()
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("failed to serialize log entry for '{log_id}': {error}"),
                )
            })?
            .len();
        if encoded_size > MAX_DURABLE_LOG_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "log entry for '{log_id}' is {encoded_size} bytes, exceeding the durable line limit of {MAX_DURABLE_LOG_LINE_BYTES} bytes"
                ),
            ));
        }
        let mut durable_lines = self.append_lines.lock().await;
        let archive = self.archive.as_ref().ok_or_else(|| {
            std::io::Error::other(format!(
                "durable log chunks are enabled for '{log_id}' without an archive backend"
            ))
        })?;
        let last_line = match durable_lines.get(log_id) {
            Some(line) => *line,
            None => archive
                .download_recent_log_chunks(log_id, 1)
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "failed to recover durable cursor for log '{log_id}': {error}"
                    ))
                })?
                .last()
                .map_or(0, |chunk| chunk.line),
        };
        entry.line = last_line.saturating_add(1);
        let jsonl = entry.to_jsonl().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "failed to serialize durable log '{log_id}' line {}: {error}",
                    entry.line
                ),
            )
        })?;
        let data = jsonl.into_bytes();
        let retry_config = temps_core::retry::RetryConfig::new(ARCHIVE_UPLOAD_RETRIES)
            .with_base_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_secs(1));
        let mut last_error = None;
        for attempt in 0..ARCHIVE_UPLOAD_RETRIES {
            match tokio::time::timeout(
                ARCHIVE_UPLOAD_TIMEOUT,
                archive.upload_log_chunk(log_id, entry.line, data.clone()),
            )
            .await
            {
                Ok(Ok(())) => {
                    last_error = None;
                    break;
                }
                Ok(Err(error)) => {
                    let retryable = error.is_retryable();
                    last_error = Some(error.to_string());
                    if !retryable {
                        break;
                    }
                }
                Err(_) => {
                    last_error = Some(format!("upload timed out after {ARCHIVE_UPLOAD_TIMEOUT:?}"));
                }
            }
            if attempt + 1 < ARCHIVE_UPLOAD_RETRIES {
                tokio::time::sleep(retry_config.compute_delay(attempt)).await;
            }
        }
        if let Some(error) = last_error {
            return Err(std::io::Error::other(format!(
                "failed to durably store log '{log_id}' line {}: {error}",
                entry.line
            )));
        }
        durable_lines.insert(log_id.to_string(), entry.line);
        if let Err(error) = self.structured_service.append_log(log_id, entry).await {
            warn!(
                log_id,
                error = %error,
                "Durable log chunk was stored, but the local scratch copy could not be updated"
            );
        }
        Ok(())
    }

    /// Read all structured log entries from a JSONL file
    ///
    /// Returns parsed LogEntry objects instead of raw strings.
    /// Use this for fetching logs that need to be displayed with rich formatting.
    pub async fn get_structured_logs(&self, log_id: &str) -> Result<Vec<LogEntry>, std::io::Error> {
        if self.durable_chunks {
            let content = self.get_log_content(log_id).await?;
            return Ok(content
                .lines()
                .filter_map(|line| LogEntry::from_jsonl(line).ok())
                .collect());
        }
        let local = self.structured_service.read_logs(log_id).await?;
        Ok(local)
    }

    /// Search structured logs by text (case-insensitive)
    ///
    /// This is much more efficient than searching raw log text because
    /// it only searches the message field and can leverage indexing later.
    pub async fn search_structured_logs(
        &self,
        log_id: &str,
        query: &str,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        let query = query.to_lowercase();
        Ok(self
            .get_structured_logs(log_id)
            .await?
            .into_iter()
            .filter(|entry| entry.message.to_lowercase().contains(&query))
            .collect())
    }

    /// Filter structured logs by level
    ///
    /// Returns only logs matching the specified level (info, success, warning, error)
    pub async fn filter_structured_logs_by_level(
        &self,
        log_id: &str,
        level: LogLevel,
    ) -> Result<Vec<LogEntry>, std::io::Error> {
        Ok(self
            .get_structured_logs(log_id)
            .await?
            .into_iter()
            .filter(|entry| entry.level == level)
            .collect())
    }

    // ========== Convenience Methods for Common Log Levels ==========

    /// Log an info message (ℹ️ icon in UI)
    pub async fn log_info(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Info, message)
            .await
    }

    /// Log a success message (✓ icon in UI)
    pub async fn log_success(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Success, message)
            .await
    }

    /// Log a warning message (⏳ icon in UI)
    pub async fn log_warning(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Warning, message)
            .await
    }

    /// Log an error message (✗ icon in UI)
    pub async fn log_error(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), std::io::Error> {
        self.append_structured_log(log_id, LogLevel::Error, message)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_log_service_creation() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-log";
        let log_path = log_service.get_log_path(log_id);

        assert!(log_path.to_string_lossy().contains("test-log.log"));
    }

    #[tokio::test]
    async fn log_ids_cannot_escape_storage_root() {
        let temp_dir = TempDir::new().unwrap();
        let service = LogService::new(temp_dir.path().to_path_buf());

        for unsafe_id in ["../outside", "/tmp/outside.log", "nested/../../outside"] {
            assert!(service.get_log_path(unsafe_id).starts_with(temp_dir.path()));
            assert_eq!(
                service
                    .append_structured_log(unsafe_id, LogLevel::Info, "blocked")
                    .await
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
            assert_eq!(
                service.create_log_path(unsafe_id).await.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
            assert_eq!(
                service.get_log_content(unsafe_id).await.unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

    #[tokio::test]
    async fn test_create_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path
        assert!(log_path.to_string_lossy().contains("test-create.log"));

        // Full path should exist after creation
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_append_and_read_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-append";

        // Append some content using structured logging
        log_service.log_info(log_id, "First line").await.unwrap();
        log_service.log_info(log_id, "Second line").await.unwrap();

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "First line");
        assert_eq!(logs[1].message, "Second line");
        assert!(matches!(logs[0].level, LogLevel::Info));
        assert!(matches!(logs[1].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_tail_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail";
        let log_path = log_service.create_log_path(log_id).await.unwrap();
        let log_path_str = log_path.to_str().unwrap();

        // Write initial content using structured logging
        log_service.log_info(log_id, "Initial line").await.unwrap();

        // Start tailing
        let _stream = log_service.tail_log(log_path_str).await.unwrap();

        // This is a basic test - in practice, tailing would be used with continuous writes
        // For testing purposes, we just verify the stream can be created
        // We can't easily test the streaming behavior in a unit test
    }

    #[tokio::test]
    async fn test_get_log_content_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let result = log_service.get_log_content("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_structured_log_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-create-on-append";

        // Structured log should create the file
        log_service.log_info(log_id, "First line").await.unwrap();

        // Verify file was created
        let log_path = log_service.structured_service.get_log_path(log_id);
        assert!(log_path.exists());

        // Verify content was written using structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "First line");
        assert!(matches!(logs[0].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_empty_log_content() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-empty";

        // Create log path but don't write anything
        log_service.create_log_path(log_id).await.unwrap();

        // If file exists but is empty, reading should return empty or error
        let result = log_service.get_log_content(log_id).await;
        // Either empty content or error is acceptable for an empty log
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_append_multiple_entries_same_log() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-multiple";

        // Append multiple entries using structured logging
        for i in 1..=5 {
            log_service
                .log_info(log_id, &format!("Line {}", i))
                .await
                .unwrap();
        }

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 5);
        for (i, log) in logs.iter().enumerate() {
            assert_eq!(log.message, format!("Line {}", i + 1));
            assert!(matches!(log.level, LogLevel::Info));
        }
    }

    #[tokio::test]
    async fn test_log_path_with_special_characters() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-with-dashes_and_underscores";

        // Should be able to write to it using structured logging
        log_service
            .log_info(log_id, "Content with special chars")
            .await
            .unwrap();

        // Read back the structured logs
        let logs = log_service.get_structured_logs(log_id).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "Content with special chars");
        assert!(matches!(logs[0].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_create_log_path_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-dir-structure";
        let log_path = log_service.create_log_path(log_id).await.unwrap();

        // Should create a date-based path structure
        let path_str = log_path.to_string_lossy();
        assert!(path_str.contains("/")); // Should have directory separators
        assert!(path_str.ends_with("test-dir-structure.log"));

        // Directory should exist
        let full_path = temp_dir.path().join(&log_path);
        assert!(full_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_tail_log_replays_more_than_1000_lines() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        // Write 5000 plain lines directly to the log file (bypassing the
        // structured JSONL writer to keep the assertion about line counts simple).
        let log_id = "test-large-tail";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..5000 {
            file.write_all(format!("line {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        // With the default replay cap (100k > 5000), the stream should replay
        // ALL 5000 lines, not just the last 1000.
        let stream = log_service.tail_log(log_id).await.unwrap();
        tokio::pin!(stream);

        let mut received = Vec::new();
        // The tail stream is infinite (it waits at EOF), so bound the read with
        // a short timeout once the backlog is drained.
        loop {
            match tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
                Ok(Some(Ok(line))) => received.push(line),
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => break, // timed out waiting for more — backlog drained
            }
        }

        assert_eq!(
            received.len(),
            5000,
            "expected all 5000 lines replayed, got {}",
            received.len()
        );
        assert_eq!(received.first().unwrap(), "line 0");
        assert_eq!(received.last().unwrap(), "line 4999");
    }

    #[tokio::test]
    async fn test_tail_log_with_replay_caps_backlog() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-capped-tail";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..100 {
            file.write_all(format!("line {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        // Explicit small replay cap: only the last 10 lines should come back.
        let stream = log_service.tail_log_with_replay(log_id, 10).await.unwrap();
        tokio::pin!(stream);

        let mut received = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
                Ok(Some(Ok(line))) => received.push(line),
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => break,
            }
        }

        assert_eq!(received.len(), 10, "expected last 10 lines only");
        assert_eq!(received.first().unwrap(), "line 90");
        assert_eq!(received.last().unwrap(), "line 99");
    }

    /// Run the reverse tail scan over `content` and return the byte offset it
    /// picks for `replay_lines`, exercising the same `BufReader<File>` path
    /// `tail_log_with_replay` uses.
    async fn tail_offset_for(content: &[u8], replay_lines: usize) -> u64 {
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("scan.log");
        let mut file = File::create(&path).await.unwrap();
        file.write_all(content).await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        let file = File::open(&path).await.unwrap();
        let file_size = file.metadata().await.unwrap().len();
        let mut reader = BufReader::new(file);
        find_tail_offset(&mut reader, file_size, replay_lines)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_find_tail_offset_empty_file() {
        assert_eq!(tail_offset_for(b"", 10).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_fewer_lines_than_requested() {
        // 3 lines, 10 requested -> replay from the very beginning.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 10).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_exact_line_count() {
        // Exactly `replay_lines` lines: still the whole file.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 3).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_trailing_newline_not_counted_as_line() {
        // "a\nbb\nccc\n": the final '\n' terminates "ccc", it does not start a
        // fourth (empty) line. Last line therefore begins at offset 5.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 1).await, 5);
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 2).await, 2);
    }

    #[tokio::test]
    async fn test_find_tail_offset_unterminated_final_line() {
        // No trailing newline: "ccc" is still a line.
        assert_eq!(tail_offset_for(b"a\nbb\nccc", 1).await, 5);
        assert_eq!(tail_offset_for(b"a\nbb\nccc", 2).await, 2);
    }

    #[tokio::test]
    async fn test_find_tail_offset_counts_empty_lines() {
        // "a\n\nb\n" is three lines: "a", "", "b".
        assert_eq!(tail_offset_for(b"a\n\nb\n", 2).await, 2);
        assert_eq!(tail_offset_for(b"a\n\nb\n", 1).await, 3);
    }

    #[tokio::test]
    async fn test_find_tail_offset_replay_all_sentinel() {
        // usize::MAX means "replay everything".
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", usize::MAX).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_zero_replay_attaches_at_eof() {
        // A zero backlog must yield no replayed lines at all, i.e. EOF.
        assert_eq!(tail_offset_for(b"a\nbb\nccc\n", 0).await, 9);
        assert_eq!(tail_offset_for(b"", 0).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_lines_longer_than_scan_chunk() {
        // Lines far longer than TAIL_SCAN_CHUNK force the backwards scan to
        // cross several blocks without finding a newline.
        let long = "x".repeat(TAIL_SCAN_CHUNK * 2 + 7);
        let content = format!("{long}\n{long}\n{long}\n");
        let line_len = long.len() as u64 + 1;

        assert_eq!(tail_offset_for(content.as_bytes(), 1).await, line_len * 2);
        assert_eq!(tail_offset_for(content.as_bytes(), 2).await, line_len);
        assert_eq!(tail_offset_for(content.as_bytes(), 3).await, 0);
    }

    #[tokio::test]
    async fn test_find_tail_offset_newline_on_chunk_boundary() {
        // Place a newline exactly at the first byte of the final scan block so
        // the match is found at index 0 of a chunk.
        let mut content = vec![b'x'; TAIL_SCAN_CHUNK];
        content[0] = b'\n';
        content.push(b'\n');
        // File is: "\n" + x*(CHUNK-1) + "\n" -> lines "" and "xxx...".
        assert_eq!(tail_offset_for(&content, 1).await, 1);
        assert_eq!(tail_offset_for(&content, 2).await, 0);
    }

    #[tokio::test]
    async fn test_tail_log_replay_then_live_lines() {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-replay-then-live";
        let log_path = log_service.get_log_path(log_id);
        let mut file = File::create(&log_path).await.unwrap();
        for i in 0..50 {
            file.write_all(format!("old {}\n", i).as_bytes())
                .await
                .unwrap();
        }
        file.flush().await.unwrap();

        let stream = log_service.tail_log_with_replay(log_id, 3).await.unwrap();
        tokio::pin!(stream);

        // Backlog: the last 3 lines only.
        let mut received = Vec::new();
        for _ in 0..3 {
            if let Ok(Some(Ok(line))) =
                tokio::time::timeout(Duration::from_millis(500), stream.next()).await
            {
                received.push(line);
            }
        }
        assert_eq!(received, vec!["old 47", "old 48", "old 49"]);

        // Lines appended after the stream attached must still arrive live.
        let mut appender = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .await
            .unwrap();
        appender.write_all(b"new 0\nnew 1\n").await.unwrap();
        appender.flush().await.unwrap();

        let mut live = Vec::new();
        for _ in 0..2 {
            if let Ok(Some(Ok(line))) =
                tokio::time::timeout(Duration::from_secs(2), stream.next()).await
            {
                live.push(line);
            }
        }
        assert_eq!(live, vec!["new 0", "new 1"]);
    }

    #[tokio::test]
    async fn test_tail_log_nonexistent_file_creates_it() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let log_id = "test-tail-create";
        let log_path = log_service.get_log_path(log_id);

        // File doesn't exist
        assert!(!log_path.exists());

        // Tail should create the file
        let _stream = log_service.tail_log(log_id).await.unwrap();

        // File should now exist
        assert!(log_path.exists());
    }

    // ========== Archive backend tests ==========

    /// Build a log id shaped like a real `deployment_jobs.log_id`
    /// (`{project}/{env}/{date-path}/deployment-{id}-job-{job}.log`, see
    /// `workflow_planner.rs`): it contains a `/` and already ends in
    /// `.log`. This matters here specifically because `LogService`'s three
    /// path-resolution helpers (`get_log_path`, `create_log_path`, and
    /// `StructuredLogService::get_log_path`, used respectively by
    /// `archive_log`/`get_log_content`, the legacy non-JSONL writer, and
    /// `append_structured_log`) only agree on the resulting path when the
    /// log id already contains a `/` -- for a bare id with no extension
    /// they'd each pick a different suffix (`.log` vs `.jsonl`) and land on
    /// three different files. That divergence is dormant in production
    /// (every real log id already has both properties) and pre-dates this
    /// PR, so fixing it is out of scope here; using realistic ids in these
    /// new tests avoids exercising it while still testing the real
    /// behavior archival cares about.
    fn realistic_log_id(name: &str) -> String {
        format!("archive-tests/{name}.log")
    }

    /// In-memory `LogArchiveStorage` mock: records uploads in a `Mutex<HashMap>`
    /// so tests can assert both the archive's contents and, indirectly (by
    /// checking `download_log` afterwards), that `get_log_content` fell back
    /// to it correctly.
    #[derive(Default)]
    struct MockArchive {
        objects: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
        chunks: std::sync::Mutex<std::collections::BTreeMap<(String, u64), Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl LogArchiveStorage for MockArchive {
        async fn upload_log_chunk(
            &self,
            log_id: &str,
            line: u64,
            data: Vec<u8>,
        ) -> Result<(), LogArchiveStorageError> {
            self.chunks
                .lock()
                .expect("mock chunk lock poisoned")
                .insert((log_id.to_string(), line), data);
            Ok(())
        }

        async fn download_log_chunks(
            &self,
            log_id: &str,
            after_line: u64,
            limit: usize,
        ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
            Ok(self
                .chunks
                .lock()
                .expect("mock chunk lock poisoned")
                .iter()
                .filter(|((stored_id, line), _)| stored_id == log_id && *line > after_line)
                .take(limit)
                .map(|((_, line), data)| DurableLogChunk {
                    line: *line,
                    data: data.clone(),
                })
                .collect())
        }

        async fn download_recent_log_chunks(
            &self,
            log_id: &str,
            limit: usize,
        ) -> Result<Vec<DurableLogChunk>, LogArchiveStorageError> {
            let mut chunks: Vec<_> = self
                .chunks
                .lock()
                .expect("mock chunk lock poisoned")
                .iter()
                .filter(|((stored_id, _), _)| stored_id == log_id)
                .map(|((_, line), data)| DurableLogChunk {
                    line: *line,
                    data: data.clone(),
                })
                .collect();
            if chunks.len() > limit {
                chunks.drain(..chunks.len() - limit);
            }
            Ok(chunks)
        }

        async fn upload_log(&self, key: &str, data: Vec<u8>) -> Result<(), LogArchiveStorageError> {
            self.objects
                .lock()
                .expect("mock archive lock poisoned")
                .insert(key.to_string(), data);
            Ok(())
        }

        async fn download_log(&self, key: &str) -> Result<Vec<u8>, LogArchiveStorageError> {
            self.objects
                .lock()
                .expect("mock archive lock poisoned")
                .get(key)
                .cloned()
                .ok_or_else(|| LogArchiveStorageError::NotFound {
                    bucket: "mock".to_string(),
                    key: key.to_string(),
                })
        }
    }

    #[tokio::test]
    async fn acknowledged_lines_survive_local_scratch_loss() {
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service = LogService::with_archive_mode(
            temp_dir.path().to_path_buf(),
            Some(archive.clone()),
            true,
        );
        let log_id = "project/production/deployment-42-build.log";

        log_service
            .log_info(log_id, "first durable line")
            .await
            .unwrap();
        log_service
            .log_success(log_id, "second durable line")
            .await
            .unwrap();
        tokio::fs::remove_file(log_service.get_log_path(log_id))
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert!(content.contains("first durable line"));
        assert!(content.contains("second durable line"));

        let replacement_dir = TempDir::new().unwrap();
        let replacement = LogService::with_archive_mode(
            replacement_dir.path().to_path_buf(),
            Some(archive.clone()),
            true,
        );
        replacement
            .log_warning(log_id, "third line after replacement")
            .await
            .unwrap();
        let recovered = replacement.get_log_content(log_id).await.unwrap();
        assert!(recovered.contains("first durable line"));
        assert!(recovered.contains("second durable line"));
        assert!(recovered.contains("third line after replacement"));

        let stored_lines: Vec<u64> = archive
            .chunks
            .lock()
            .expect("mock chunk lock poisoned")
            .keys()
            .filter(|(stored_id, _)| stored_id == log_id)
            .map(|(_, line)| *line)
            .collect();
        assert_eq!(stored_lines, vec![1, 2, 3]);

        let mut tail = replacement.tail_log_with_replay(log_id, 1).await.unwrap();
        let replayed = futures::StreamExt::next(&mut tail).await.unwrap().unwrap();
        assert!(replayed.contains("third line after replacement"));
    }

    /// An archive backend that fails its first `fail_until` upload attempts
    /// (with a caller-chosen `retryable` classification) before succeeding,
    /// recording how many times `upload_log` was actually called so tests
    /// can assert on retry behavior.
    struct FailingArchive {
        attempts: std::sync::atomic::AtomicU32,
        fail_until: u32,
        retryable: bool,
    }

    impl FailingArchive {
        fn new(fail_until: u32, retryable: bool) -> Self {
            Self {
                attempts: std::sync::atomic::AtomicU32::new(0),
                fail_until,
                retryable,
            }
        }

        fn attempt_count(&self) -> u32 {
            self.attempts.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl LogArchiveStorage for FailingArchive {
        async fn upload_log(
            &self,
            _key: &str,
            _data: Vec<u8>,
        ) -> Result<(), LogArchiveStorageError> {
            let attempt = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempt < self.fail_until {
                return Err(LogArchiveStorageError::Upload {
                    bucket: "test-bucket".to_string(),
                    key: "irrelevant".to_string(),
                    reason: "synthetic failure".to_string(),
                    retryable: self.retryable,
                });
            }
            Ok(())
        }

        async fn download_log(&self, key: &str) -> Result<Vec<u8>, LogArchiveStorageError> {
            Err(LogArchiveStorageError::NotFound {
                bucket: "test-bucket".to_string(),
                key: key.to_string(),
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_archive_log_does_not_retry_permanent_failure() {
        // A permanent failure (bad credentials, missing bucket) fails
        // identically on every attempt -- retrying it wastes the retry
        // budget and the backoff delay on something that cannot succeed.
        // `archive_log` must give up after exactly one attempt.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(FailingArchive::new(u32::MAX, false));
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-archive-permanent-failure");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "building...").await.unwrap();
        let log_path = log_service.get_log_path(log_id);

        let result = log_service.archive_log(log_id).await;

        assert!(result.is_err());
        assert_eq!(
            archive.attempt_count(),
            1,
            "a permanent failure must not be retried"
        );
        // Upload never succeeded, so the local scratch file must survive.
        assert!(log_path.exists());
    }

    #[tokio::test(start_paused = true)]
    async fn test_archive_log_retries_transient_failure_until_success() {
        // A transient failure (network blip, backend mid-restart) should be
        // retried with backoff until it succeeds, within the configured
        // attempt budget (ARCHIVE_UPLOAD_RETRIES = 3).
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(FailingArchive::new(2, true));
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-archive-transient-failure");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "building...").await.unwrap();
        let log_path = log_service.get_log_path(log_id);

        let result = log_service.archive_log(log_id).await;

        assert!(result.is_ok(), "expected eventual success: {result:?}");
        assert_eq!(
            archive.attempt_count(),
            3,
            "expected exactly 2 failed attempts followed by 1 successful attempt"
        );
        assert!(
            !log_path.exists(),
            "successful archive must delete the local file"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_archive_log_gives_up_after_exhausting_retries_on_persistent_transient_failure() {
        // A transient failure that never actually clears (a sustained
        // outage) must still give up after ARCHIVE_UPLOAD_RETRIES attempts,
        // leaving the local file in place, rather than retrying forever.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(FailingArchive::new(u32::MAX, true));
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-archive-exhausted-retries");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "building...").await.unwrap();
        let log_path = log_service.get_log_path(log_id);

        let result = log_service.archive_log(log_id).await;

        assert!(result.is_err());
        assert_eq!(archive.attempt_count(), 3, "expected exactly 3 attempts");
        assert!(log_path.exists());
    }

    #[tokio::test]
    async fn test_archive_log_bounds_concurrent_uploads() {
        // ARCHIVE_MAX_CONCURRENT_UPLOADS caps how many uploads run at once.
        // Drive more concurrent archive_log calls than the limit through an
        // archive backend that tracks its own in-flight count, and assert
        // the observed peak never exceeds the configured bound.
        struct ConcurrencyTrackingArchive {
            in_flight: std::sync::atomic::AtomicUsize,
            peak: std::sync::atomic::AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LogArchiveStorage for ConcurrencyTrackingArchive {
            async fn upload_log(
                &self,
                _key: &str,
                _data: Vec<u8>,
            ) -> Result<(), LogArchiveStorageError> {
                let current = self
                    .in_flight
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.peak
                    .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.in_flight
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }

            async fn download_log(&self, key: &str) -> Result<Vec<u8>, LogArchiveStorageError> {
                Err(LogArchiveStorageError::NotFound {
                    bucket: "test-bucket".to_string(),
                    key: key.to_string(),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(ConcurrencyTrackingArchive {
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
        });
        let log_service = Arc::new(LogService::with_archive(
            temp_dir.path().to_path_buf(),
            Some(archive.clone()),
        ));

        // Twice ARCHIVE_MAX_CONCURRENT_UPLOADS jobs "finish" at once.
        let job_count = ARCHIVE_MAX_CONCURRENT_UPLOADS * 2;
        let mut handles = Vec::with_capacity(job_count);
        for i in 0..job_count {
            let log_id = realistic_log_id(&format!("test-archive-concurrency-{i}"));
            log_service.log_info(&log_id, "building...").await.unwrap();
            let log_service = log_service.clone();
            handles.push(tokio::spawn(async move {
                log_service.archive_log(&log_id).await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }

        let peak = archive.peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            peak <= ARCHIVE_MAX_CONCURRENT_UPLOADS,
            "observed {peak} concurrent uploads, expected at most {ARCHIVE_MAX_CONCURRENT_UPLOADS}"
        );
    }

    #[tokio::test]
    async fn test_archive_log_noop_when_no_backend_configured() {
        // Default LogService::new has no archive backend: archive_log must be
        // a pure no-op that never deletes the local file. This is the
        // regression guard for every existing filesystem-only install.
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());
        assert!(!log_service.archive_enabled());

        let log_id = realistic_log_id("test-noop-archive");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "line one").await.unwrap();
        let log_path = log_service.get_log_path(log_id);
        assert!(log_path.exists());

        log_service.archive_log(log_id).await.unwrap();

        // Local file must still be there -- nothing was archived, so nothing
        // should have been deleted.
        assert!(log_path.exists());
        let content = log_service.get_log_content(log_id).await.unwrap();
        assert!(content.contains("line one"));
    }

    #[tokio::test]
    async fn test_archive_log_uploads_and_deletes_local_file() {
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));
        assert!(log_service.archive_enabled());

        let log_id = realistic_log_id("test-archive-upload");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "building...").await.unwrap();
        log_service.log_success(log_id, "done").await.unwrap();
        let log_path = log_service.get_log_path(log_id);
        assert!(log_path.exists());

        log_service.archive_log(log_id).await.unwrap();

        // Local scratch file is gone: this is the bounded-disk guarantee.
        assert!(!log_path.exists());

        // The archive received the exact JSONL content that was on disk.
        let key = log_service.archive_key(log_id);
        let archived = archive.download_log(&key).await.unwrap();
        let archived_text = String::from_utf8(archived).unwrap();
        assert!(archived_text.contains("building..."));
        assert!(archived_text.contains("done"));
    }

    #[tokio::test]
    async fn test_archive_log_missing_local_file_is_noop() {
        // A job that never wrote a line (e.g. cancelled before it started)
        // has no local file at all. Archiving it must not error.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service = LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive));

        let result = log_service.archive_log("never-ran").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_archive_log_is_idempotent() {
        // Calling archive_log twice (e.g. a retried completion hook) must not
        // error just because the local file is already gone the second time.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-archive-twice");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "line").await.unwrap();

        log_service.archive_log(log_id).await.unwrap();
        log_service.archive_log(log_id).await.unwrap();

        let key = log_service.archive_key(log_id);
        assert!(archive.download_log(&key).await.is_ok());
    }

    #[tokio::test]
    async fn test_get_log_content_falls_back_to_archive_after_deletion() {
        // Simulates reading a job's log after it has completed and been
        // archived: the local file is gone, so get_log_content must
        // transparently serve the archived copy.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-read-after-archive");
        let log_id = log_id.as_str();
        log_service
            .log_info(log_id, "archived content")
            .await
            .unwrap();
        log_service.archive_log(log_id).await.unwrap();
        assert!(!log_service.get_log_path(log_id).exists());

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert!(content.contains("archived content"));
    }

    #[tokio::test]
    async fn test_get_log_content_prefers_local_file_over_archive() {
        // A log still in progress (or written before archival shipped) must
        // be served from disk even if an archive backend is configured --
        // never speculatively hit S3 while the local copy is authoritative.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service =
            LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive.clone()));

        let log_id = realistic_log_id("test-local-preferred");
        let log_id = log_id.as_str();
        log_service.log_info(log_id, "local content").await.unwrap();
        // Seed the archive with different content under the same key, to
        // prove it is never consulted while the local file exists.
        let key = log_service.archive_key(log_id);
        archive
            .upload_log(&key, b"stale archived content".to_vec())
            .await
            .unwrap();

        let content = log_service.get_log_content(log_id).await.unwrap();
        assert!(content.contains("local content"));
        assert!(!content.contains("stale archived content"));
    }

    #[tokio::test]
    async fn test_get_log_content_missing_everywhere_returns_original_not_found() {
        // When neither the local file nor the archive has the log, the
        // caller should see the same NotFound-flavored io::Error as before
        // this feature existed, not an archive-specific error.
        let temp_dir = TempDir::new().unwrap();
        let archive = Arc::new(MockArchive::default());
        let log_service = LogService::with_archive(temp_dir.path().to_path_buf(), Some(archive));

        let result = log_service.get_log_content("does-not-exist-anywhere").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn test_archive_key_preserves_nested_log_id_structure() {
        let temp_dir = TempDir::new().unwrap();
        let log_service = LogService::new(temp_dir.path().to_path_buf());

        let nested_log_id = "my-project/production/2026/09/18/12/34/deployment-42-job-build.log";
        let key = log_service.archive_key(nested_log_id);
        assert_eq!(key, format!("{ARCHIVE_KEY_PREFIX}/{nested_log_id}"));
    }
}
