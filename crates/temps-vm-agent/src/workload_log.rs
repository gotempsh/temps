// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output of a spawned workload process, kept on a finite rootfs.
//!
//! A microVM boots straight into `temps-vm-agent`: there is no init, no
//! logrotate and no journald, and the rootfs is a fixed-size ext4 image. A
//! long-lived app that logs every request would, with a plain append-only
//! file, eventually fill that image and start failing with `ENOSPC`. So the
//! agent owns the process's stdout/stderr pipes and copies them into a
//! [`RotatingLog`]: at most `max_files` files of at most `max_bytes` each
//! (`path`, `path.1`, …, newest first), the same `max-size`/`max-file` model
//! Docker's `json-file` driver uses for container logs.
//!
//! The copy never blocks the app on the disk. Each pipe is drained by a
//! [`pump`] thread that hands chunks to the writer through a bounded queue
//! with `try_send`: when the writer falls behind (a slow or full disk), the
//! newest chunks are dropped and counted, and the writer records how many
//! bytes were lost in the log itself. Memory per process is bounded by
//! [`LOG_QUEUE_CHUNKS`] × [`LOG_CHUNK_BYTES`].

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Size of one file when the request names none: Docker's common
/// `max-size=10m`.
pub const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Files kept (current + rotated) when the request names none.
pub const DEFAULT_LOG_MAX_FILES: u32 = 3;

/// Smallest file size honoured; anything smaller would rotate on every line.
pub const MIN_LOG_MAX_BYTES: u64 = 64 * 1024;

/// Largest file size honoured, so a mistyped size cannot reintroduce the
/// unbounded file this module exists to prevent.
pub const MAX_LOG_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Most files honoured (current + rotated).
pub const MAX_LOG_MAX_FILES: u32 = 16;

/// Bytes read from a pipe per chunk.
pub const LOG_CHUNK_BYTES: usize = 16 * 1024;

/// Chunks queued between the pipe readers and the writer before output is
/// dropped: with [`LOG_CHUNK_BYTES`] this bounds the queue at 1 MiB.
pub const LOG_QUEUE_CHUNKS: usize = 64;

/// Most bytes a tail read returns, whatever the line count asked for.
pub const MAX_LOG_TAIL_BYTES: usize = 4 * 1024 * 1024;

/// How large the files get and how many are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLimits {
    pub max_bytes: u64,
    pub max_files: u32,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_LOG_MAX_BYTES,
            max_files: DEFAULT_LOG_MAX_FILES,
        }
    }
}

impl LogLimits {
    /// The limits a spawn request asks for, defaulted when absent and
    /// clamped to [`MIN_LOG_MAX_BYTES`]..=[`MAX_LOG_MAX_BYTES`] and
    /// 1..=[`MAX_LOG_MAX_FILES`].
    pub fn from_request(max_bytes: Option<u64>, max_files: Option<u32>) -> Self {
        Self {
            max_bytes: max_bytes
                .unwrap_or(DEFAULT_LOG_MAX_BYTES)
                .clamp(MIN_LOG_MAX_BYTES, MAX_LOG_MAX_BYTES),
            max_files: max_files
                .unwrap_or(DEFAULT_LOG_MAX_FILES)
                .clamp(1, MAX_LOG_MAX_FILES),
        }
    }
}

/// The path of rotated file `index` (`0` is the file being written).
pub fn rotated_path(path: &Path, index: u32) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// A size-bounded log: `path` is written until it holds `max_bytes`, then
/// it becomes `path.1` (the previous `path.1` becomes `path.2`, …) and the
/// oldest file beyond `max_files` is deleted. No file ever exceeds
/// `max_bytes`, so the whole log never exceeds `max_bytes * max_files`.
#[derive(Debug)]
pub struct RotatingLog {
    path: PathBuf,
    limits: LogLimits,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    /// Open (appending to) `path`, creating its directory. Rotated files a
    /// previous run left beyond `max_files` are removed, so lowering the
    /// limit also lowers what is on disk.
    pub fn open(path: impl Into<PathBuf>, limits: LogLimits) -> std::io::Result<Self> {
        let path = path.into();
        let limits = LogLimits {
            max_bytes: limits.max_bytes.max(1),
            max_files: limits.max_files.max(1),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for index in limits.max_files..=MAX_LOG_MAX_FILES.max(limits.max_files) {
            remove_if_present(&rotated_path(&path, index))?;
        }
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            limits,
            file: Some(file),
            size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn limits(&self) -> LogLimits {
        self.limits
    }

    /// Append `data`, rotating first when it would not fit in the current
    /// file. A chunk larger than a whole file is split across files.
    pub fn write_all(&mut self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            if self.size > 0 && self.size.saturating_add(data.len() as u64) > self.limits.max_bytes
            {
                self.rotate()?;
            }
            let room = self.limits.max_bytes.saturating_sub(self.size);
            let n = usize::try_from(room).unwrap_or(usize::MAX).min(data.len());
            let file = match self.file.as_mut() {
                Some(file) => file,
                None => self.file.insert(open_append(&self.path)?),
            };
            if let Err(error) = file.write_all(&data[..n]) {
                // A partial write leaves the size unknown: re-read it so
                // the bound still holds once the disk has room again.
                self.size = file.metadata().map(|m| m.len()).unwrap_or(self.size);
                return Err(error);
            }
            self.size += n as u64;
            data = &data[n..];
        }
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        let max_files = self.limits.max_files;
        if max_files > 1 {
            remove_if_present(&rotated_path(&self.path, max_files - 1))?;
            for index in (1..max_files - 1).rev() {
                rename_if_present(
                    &rotated_path(&self.path, index),
                    &rotated_path(&self.path, index + 1),
                )?;
            }
            rename_if_present(&self.path, &rotated_path(&self.path, 1))?;
        } else {
            remove_if_present(&self.path)?;
        }
        self.file = Some(open_append(&self.path)?);
        self.size = 0;
        Ok(())
    }
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename_if_present(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The last `lines` lines of the log at `path`, read across the current
/// file and its rotated predecessors (newest first, so a tail that spans a
/// rotation is still in order). Reads backwards in fixed-size blocks, so it
/// costs at most [`MAX_LOG_TAIL_BYTES`] of memory whatever the files hold;
/// a tail cut by that cap starts mid-line.
pub fn read_tail(path: &Path, lines: usize) -> std::io::Result<Vec<u8>> {
    read_tail_capped(path, lines, MAX_LOG_TAIL_BYTES)
}

fn read_tail_capped(path: &Path, lines: usize, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    const BLOCK: u64 = 64 * 1024;
    if lines == 0 || max_bytes == 0 {
        return Ok(Vec::new());
    }
    // Newest first; each block keeps its forward byte order.
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    let mut newlines = 0usize;
    let mut at_stream_end = true;
    'files: for index in 0..=MAX_LOG_MAX_FILES {
        let file_path = rotated_path(path, index);
        let mut file = match File::open(&file_path) {
            Ok(file) => file,
            // The current file can be missing for an instant mid-rotation;
            // a missing rotated file means there are no older ones.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if index == 0 {
                    continue;
                }
                break;
            }
            Err(e) => return Err(e),
        };
        let mut pos = file.metadata()?.len();
        while pos > 0 {
            let budget = (max_bytes - total) as u64;
            if budget == 0 {
                break 'files;
            }
            let want = BLOCK.min(pos).min(budget);
            pos -= want;
            file.seek(SeekFrom::Start(pos))?;
            let mut block = vec![0u8; want as usize];
            file.read_exact(&mut block)?;
            for i in (0..block.len()).rev() {
                let is_last_byte = at_stream_end;
                at_stream_end = false;
                // The stream's own trailing newline ends the last line; it
                // does not start a new one.
                if block[i] == b'\n' && !is_last_byte {
                    newlines += 1;
                    if newlines == lines {
                        blocks.push(block[i + 1..].to_vec());
                        break 'files;
                    }
                }
            }
            total += block.len();
            blocks.push(block);
        }
    }
    blocks.reverse();
    Ok(blocks.concat())
}

/// Where the bytes a [`LogPipeline`] collects end up. [`RotatingLog`] in the
/// agent; tests substitute a sink that blocks to prove the app never does.
pub trait LogSink: Send + 'static {
    fn write_chunk(&mut self, chunk: &[u8]) -> std::io::Result<()>;
}

impl LogSink for RotatingLog {
    fn write_chunk(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.write_all(chunk)
    }
}

/// The writer side of a process's output: one thread owning the sink, fed
/// by any number of [`pump`] threads (one per pipe, and new ones each time a
/// supervised process is restarted).
pub struct LogPipeline {
    tx: SyncSender<Vec<u8>>,
    dropped: Arc<AtomicU64>,
    writer: JoinHandle<()>,
}

impl LogPipeline {
    /// Start the writer thread. `on_error` is called when writing starts to
    /// fail (not for every failing chunk), so a full disk produces one
    /// record instead of one per line.
    pub fn start<S, E>(sink: S, on_error: E) -> std::io::Result<Self>
    where
        S: LogSink,
        E: Fn(&std::io::Error) + Send + 'static,
    {
        let (tx, rx) = sync_channel::<Vec<u8>>(LOG_QUEUE_CHUNKS);
        let dropped = Arc::new(AtomicU64::new(0));
        let writer_dropped = dropped.clone();
        let writer = std::thread::Builder::new()
            .name("workload-log-writer".to_string())
            .spawn(move || write_loop(sink, rx, &writer_dropped, on_error))?;
        Ok(Self {
            tx,
            dropped,
            writer,
        })
    }

    /// Drain `reader` (a child's stdout or stderr) into the log on its own
    /// thread, until it reaches end of file.
    pub fn attach<R: Read + Send + 'static>(
        &self,
        reader: R,
        name: &str,
    ) -> std::io::Result<JoinHandle<()>> {
        let tx = self.tx.clone();
        let dropped = self.dropped.clone();
        std::thread::Builder::new()
            .name(format!("workload-log-{name}"))
            .spawn(move || pump(reader, &tx, &dropped))
    }

    /// Stop accepting output and wait for the writer to flush what is
    /// queued. Pumps still running keep their own sender, so this returns
    /// once they have reached end of file too.
    pub fn finish(self) {
        drop(self.tx);
        let _ = self.writer.join();
    }
}

/// Copy `reader` into the queue until end of file. Never blocks on the
/// writer: a full queue drops the chunk and counts it, so the process
/// writing to the pipe keeps running whatever the disk does.
pub fn pump<R: Read>(mut reader: R, tx: &SyncSender<Vec<u8>>, dropped: &AtomicU64) {
    let mut buf = vec![0u8; LOG_CHUNK_BYTES];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        match tx.try_send(buf[..n].to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(chunk)) | Err(TrySendError::Disconnected(chunk)) => {
                dropped.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
        }
    }
}

/// The writer thread's sink plus its error reporting: a failure is reported
/// when writing starts to fail, not once per chunk.
struct ReportingSink<S, E> {
    sink: S,
    on_error: E,
    failing: bool,
}

impl<S: LogSink, E: Fn(&std::io::Error)> ReportingSink<S, E> {
    fn write(&mut self, bytes: &[u8]) {
        match self.sink.write_chunk(bytes) {
            Ok(()) => self.failing = false,
            Err(error) => {
                if !self.failing {
                    (self.on_error)(&error);
                }
                self.failing = true;
            }
        }
    }

    /// Record in the log itself how much output was dropped since the last
    /// record.
    fn record_loss(&mut self, dropped: &AtomicU64) {
        let lost = dropped.swap(0, Ordering::Relaxed);
        if lost > 0 {
            let marker = format!(
                "\n[temps-vm-agent] dropped {lost} bytes of output: the log writer fell behind\n"
            );
            self.write(marker.as_bytes());
        }
    }
}

fn write_loop<S, E>(sink: S, rx: Receiver<Vec<u8>>, dropped: &AtomicU64, on_error: E)
where
    S: LogSink,
    E: Fn(&std::io::Error),
{
    let mut writer = ReportingSink {
        sink,
        on_error,
        failing: false,
    };
    while let Ok(chunk) = rx.recv() {
        writer.record_loss(dropped);
        writer.write(&chunk);
    }
    // Output dropped after the last queued chunk is still accounted for.
    writer.record_loss(dropped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "temps-vm-agent-log-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn limits(max_bytes: u64, max_files: u32) -> LogLimits {
        LogLimits {
            max_bytes,
            max_files,
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn request_limits_are_defaulted_and_clamped() {
        assert_eq!(LogLimits::from_request(None, None), LogLimits::default());
        assert_eq!(
            LogLimits::from_request(Some(1), Some(0)),
            limits(MIN_LOG_MAX_BYTES, 1)
        );
        assert_eq!(
            LogLimits::from_request(Some(u64::MAX), Some(u32::MAX)),
            limits(MAX_LOG_MAX_BYTES, MAX_LOG_MAX_FILES)
        );
        assert_eq!(
            LogLimits::from_request(Some(5 * 1024 * 1024), Some(5)),
            limits(5 * 1024 * 1024, 5)
        );
    }

    #[test]
    fn the_log_rotates_when_the_next_write_would_exceed_the_size() {
        let dir = temp_dir("rotate");
        let path = dir.join("app.log");
        let mut log = RotatingLog::open(&path, limits(10, 3)).expect("open");
        log.write_all(b"aaaa\n").unwrap();
        log.write_all(b"bbbb\n").unwrap();
        assert_eq!(read(&path), "aaaa\nbbbb\n", "exactly at the limit");
        log.write_all(b"cccc\n").unwrap();
        assert_eq!(read(&path), "cccc\n");
        assert_eq!(read(&rotated_path(&path, 1)), "aaaa\nbbbb\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_number_of_files_and_every_files_size_stay_bounded() {
        let dir = temp_dir("bound");
        let path = dir.join("app.log");
        let mut log = RotatingLog::open(&path, limits(64, 3)).expect("open");
        for i in 0..1_000 {
            log.write_all(format!("request {i:04} served\n").as_bytes())
                .unwrap();
        }
        // A chunk larger than a whole file is split, never written whole.
        log.write_all(&[b'x'; 200]).unwrap();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["app.log", "app.log.1", "app.log.2"]);
        for name in &files {
            let size = std::fs::metadata(dir.join(name)).unwrap().len();
            assert!(size <= 64, "{name} is {size} bytes");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_file_limit_truncates_instead_of_keeping_history() {
        let dir = temp_dir("single");
        let path = dir.join("app.log");
        let mut log = RotatingLog::open(&path, limits(8, 1)).expect("open");
        log.write_all(b"one\ntwo\n").unwrap();
        log.write_all(b"three\n").unwrap();
        assert_eq!(read(&path), "three\n");
        assert!(!rotated_path(&path, 1).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_with_fewer_files_removes_the_surplus_and_keeps_appending() {
        let dir = temp_dir("reopen");
        let path = dir.join("app.log");
        {
            let mut log = RotatingLog::open(&path, limits(4, 5)).expect("open");
            for line in [
                "a\n", "b\n", "c\n", "d\n", "e\n", "f\n", "g\n", "h\n", "i\n",
            ] {
                log.write_all(line.as_bytes()).unwrap();
            }
        }
        assert!(rotated_path(&path, 4).exists());
        let mut log = RotatingLog::open(&path, limits(4, 2)).expect("reopen");
        assert!(!rotated_path(&path, 2).exists());
        assert!(!rotated_path(&path, 4).exists());
        log.write_all(b"j\n").unwrap();
        assert_eq!(read(&path), "i\nj\n", "appends to the existing file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_spans_the_current_and_the_rotated_files_in_order() {
        let dir = temp_dir("tail");
        let path = dir.join("app.log");
        let mut log = RotatingLog::open(&path, limits(12, 3)).expect("open");
        for i in 0..10 {
            log.write_all(format!("line-{i}\n").as_bytes()).unwrap();
        }
        // 7-byte lines, 12-byte files: one line per file; three files kept.
        let tail = String::from_utf8(read_tail(&path, 2).unwrap()).unwrap();
        assert_eq!(tail, "line-8\nline-9\n");
        let tail = String::from_utf8(read_tail(&path, 3).unwrap()).unwrap();
        assert_eq!(tail, "line-7\nline-8\nline-9\n");
        // More lines than kept: everything that is left, oldest first.
        let tail = String::from_utf8(read_tail(&path, 50).unwrap()).unwrap();
        assert_eq!(tail, "line-7\nline-8\nline-9\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_handles_a_partial_last_line_a_missing_log_and_the_byte_cap() {
        let dir = temp_dir("tail-edges");
        let path = dir.join("app.log");
        assert!(read_tail(&path, 10).unwrap().is_empty(), "no log yet");
        std::fs::write(&path, "a\nb\nc").unwrap();
        assert_eq!(read_tail(&path, 2).unwrap(), b"b\nc");
        assert_eq!(read_tail(&path, 0).unwrap(), b"");
        // A tail cut by the byte cap returns the newest bytes.
        assert_eq!(read_tail_capped(&path, 100, 3).unwrap(), b"b\nc");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sink that blocks until released, as a stalled disk would.
    struct StalledSink {
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl LogSink for StalledSink {
        fn write_chunk(&mut self, chunk: &[u8]) -> std::io::Result<()> {
            let (lock, cvar) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
            self.written.lock().unwrap().extend_from_slice(chunk);
            Ok(())
        }
    }

    #[test]
    fn a_writer_that_falls_behind_drops_output_instead_of_blocking_the_process() {
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let written = Arc::new(Mutex::new(Vec::new()));
        let pipeline = LogPipeline::start(
            StalledSink {
                release: release.clone(),
                written: written.clone(),
            },
            |_| {},
        )
        .expect("start");
        // Far more than the queue holds, offered while the writer is stuck.
        // `#` never appears in the drop marker, so it counts payload only.
        let output = vec![b'#'; LOG_CHUNK_BYTES * (LOG_QUEUE_CHUNKS + 40)];
        let total = output.len() as u64;
        let pump = pipeline
            .attach(std::io::Cursor::new(output), "stdout")
            .expect("attach");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pump.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the pipe reader blocked on a stalled writer"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        pump.join().expect("pump");

        {
            let (lock, cvar) = &*release;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        pipeline.finish();
        let written = written.lock().unwrap();
        let text = String::from_utf8_lossy(&written);
        // The loss is recorded in the log itself, byte-exact.
        let dropped: u64 = text
            .split("dropped ")
            .skip(1)
            .filter_map(|rest| rest.split(' ').next()?.parse::<u64>().ok())
            .sum();
        assert!(
            dropped > 0,
            "nothing was dropped: {} bytes kept",
            written.len()
        );
        assert!(dropped < total, "nothing was queued");
        let kept = written.iter().filter(|b| **b == b'#').count() as u64;
        assert_eq!(kept + dropped, total, "every byte is kept or counted");
    }

    #[test]
    fn a_failing_sink_reports_once_and_keeps_draining() {
        struct FullDisk;
        impl LogSink for FullDisk {
            fn write_chunk(&mut self, _: &[u8]) -> std::io::Result<()> {
                Err(std::io::Error::other("no space left on device"))
            }
        }
        let reports = Arc::new(AtomicU64::new(0));
        let counter = reports.clone();
        let pipeline = LogPipeline::start(FullDisk, move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .expect("start");
        for _ in 0..3 {
            let output = vec![b'z'; LOG_CHUNK_BYTES * 4];
            pipeline
                .attach(std::io::Cursor::new(output), "stdout")
                .expect("attach")
                .join()
                .expect("pump");
        }
        pipeline.finish();
        assert_eq!(reports.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_pipeline_into_a_rotating_log_keeps_every_byte_in_order() {
        let dir = temp_dir("pipeline");
        let path = dir.join("app.log");
        let log = RotatingLog::open(&path, limits(1024 * 1024, 2)).expect("open");
        let pipeline = LogPipeline::start(log, |_| {}).expect("start");
        let text: String = (0..500).map(|i| format!("event {i}\n")).collect();
        pipeline
            .attach(std::io::Cursor::new(text.clone().into_bytes()), "stdout")
            .expect("attach")
            .join()
            .expect("pump");
        pipeline.finish();
        assert_eq!(read(&path), text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
