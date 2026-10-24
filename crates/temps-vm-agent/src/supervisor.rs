// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Supervision of a workload's main process inside a microVM (ADR-055
//! Phase 10).
//!
//! [`Supervision::start`] starts the process, pipes its stdout/stderr into a
//! [`LogPipeline`] (a size-bounded, rotating log; see
//! [`crate::workload_log`]) and hands it to a thread that waits for it to
//! exit and restarts it per its [`SpawnRestartPolicy`] with an exponential
//! [`RestartBackoff`]. A deliberate stop ([`Supervision::mark_stopped`]) is
//! never restarted.
//!
//! The first start is synchronous so the host learns at once that a command
//! does not exist; a restart that fails to start counts as a failed run
//! ([`SPAWN_FAILED_EXIT_CODE`]) and is retried under the same backoff.
//!
//! Threads per supervised process: the supervisor, the log writer, and one
//! reader per pipe of the current run.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::json;
use thiserror::Error;

use crate::logging::{self, Level};
use crate::restart::{should_restart, RestartBackoff, SpawnRestartPolicy, SPAWN_FAILED_EXIT_CODE};
use crate::workload_log::LogPipeline;

/// `PATH` for a process whose environment names none: PID 1's own
/// environment is empty and guest images vary.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

#[derive(Debug, Error)]
pub enum SuperviseError {
    #[error("spawn: empty command")]
    EmptyCommand,
    #[error("spawn {program} (cwd {cwd}): {source}")]
    Spawn {
        program: String,
        cwd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("spawn {program}: starting the supervisor thread: {source}")]
    Thread {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// What to run, kept for every restart.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: String,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl ProcessSpec {
    fn program(&self) -> &str {
        self.cmd.first().map(String::as_str).unwrap_or_default()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.program());
        command
            .args(self.cmd.iter().skip(1))
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            // Pipes, never the log file itself: the agent bounds and
            // rotates what the process writes.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.env.contains_key("PATH") {
            command.env("PATH", DEFAULT_PATH);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        {
            use std::os::unix::process::CommandExt;
            if let Some(gid) = self.gid.or(self.uid) {
                command.gid(gid);
            }
            if let Some(uid) = self.uid {
                command.uid(uid);
            }
        }
        command
    }

    /// Start one run and connect its pipes to `log`.
    fn spawn(&self, log: &LogPipeline) -> Result<Child, SuperviseError> {
        let mut child = self
            .command()
            .spawn()
            .map_err(|source| SuperviseError::Spawn {
                program: self.program().to_string(),
                cwd: self.cwd.clone(),
                source,
            })?;
        let pid = child.id();
        for (name, pipe) in [
            ("stdout", child.stdout.take().map(PipeEnd::Out)),
            ("stderr", child.stderr.take().map(PipeEnd::Err)),
        ] {
            let Some(pipe) = pipe else { continue };
            // Should the reader fail to start, the pipe closes and the
            // process sees EPIPE on that stream; it is not left blocked.
            if let Err(e) = log.attach(pipe, name) {
                logging::emit(
                    Level::Error,
                    "workload_log_reader_failed",
                    &[
                        ("pid", json!(pid)),
                        ("stream", json!(name)),
                        ("error", json!(e.to_string())),
                    ],
                );
            }
        }
        Ok(child)
    }
}

/// Either pipe of a child, as one `Read` type for [`LogPipeline::attach`].
enum PipeEnd {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl std::io::Read for PipeEnd {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            PipeEnd::Out(pipe) => pipe.read(buf),
            PipeEnd::Err(pipe) => pipe.read(buf),
        }
    }
}

/// A snapshot of one supervised process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionStatus {
    /// The pid of the current run, while one runs.
    pub current_pid: Option<u32>,
    /// Exit code of the last run that ended (`-1` when killed by a signal).
    pub last_exit: Option<i32>,
    /// Restarts so far.
    pub restart_count: u32,
    /// Waiting out the backoff before the next restart.
    pub restarting: bool,
    /// The supervisor gave up (policy, retry limit, or a deliberate stop):
    /// nothing will run again.
    pub finished: bool,
    /// Stopped on purpose; never restarted.
    pub stopped: bool,
}

impl SupervisionStatus {
    pub fn running(&self) -> bool {
        self.current_pid.is_some()
    }
}

#[derive(Debug)]
struct Shared {
    status: Mutex<SupervisionStatus>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SupervisionStatus> {
        // The status is plain data, valid whatever a panicking holder did.
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One supervised workload process. Its [`Supervision::id`] (the first
/// run's pid) is the handle the host keeps across restarts.
#[derive(Debug, Clone)]
pub struct Supervision {
    id: u32,
    shared: Arc<Shared>,
}

impl Supervision {
    /// Start `spec` and supervise it. Fails when the first run cannot start.
    pub fn start(
        spec: ProcessSpec,
        policy: SpawnRestartPolicy,
        backoff: RestartBackoff,
        log: LogPipeline,
    ) -> Result<Self, SuperviseError> {
        if spec.cmd.is_empty() {
            return Err(SuperviseError::EmptyCommand);
        }
        let child = spec.spawn(&log)?;
        let id = child.id();
        let shared = Arc::new(Shared {
            status: Mutex::new(SupervisionStatus {
                current_pid: Some(id),
                last_exit: None,
                restart_count: 0,
                restarting: false,
                finished: false,
                stopped: false,
            }),
            wake: Condvar::new(),
        });
        let program = spec.program().to_string();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("workload-supervisor-{id}"))
            .spawn(move || supervise(id, child, spec, policy, backoff, log, &thread_shared));
        if let Err(source) = spawned {
            // The child is owned by the closure that never ran and is gone
            // with it; kill it so it is not left running unsupervised.
            // SAFETY: plain kill(2) on a pid this function just started.
            unsafe { libc::kill(id as i32, libc::SIGKILL) };
            return Err(SuperviseError::Thread { program, source });
        }
        Ok(Self { id, shared })
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn status(&self) -> SupervisionStatus {
        *self.shared.lock()
    }

    pub fn is_finished(&self) -> bool {
        self.shared.lock().finished
    }

    /// Whether `pid` names this supervision: its handle or its current run.
    pub fn matches(&self, pid: u32) -> bool {
        self.id == pid || self.shared.lock().current_pid == Some(pid)
    }

    /// A deliberate stop: whatever ends the current run (the VM shutting
    /// down, a signal), it is not restarted, and a pending restart is
    /// cancelled at once.
    pub fn mark_stopped(&self) {
        let mut status = self.shared.lock();
        status.stopped = true;
        drop(status);
        self.shared.wake.notify_all();
    }
}

fn wait_exit_code(child: &mut Child) -> i32 {
    match child.wait() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

fn supervise(
    id: u32,
    first: Child,
    spec: ProcessSpec,
    policy: SpawnRestartPolicy,
    mut backoff: RestartBackoff,
    log: LogPipeline,
    shared: &Shared,
) {
    let mut child = Some(first);
    let mut started = Instant::now();
    loop {
        let (exit_code, uptime, pid) = match child.take() {
            Some(mut run) => {
                let pid = run.id();
                (wait_exit_code(&mut run), started.elapsed(), Some(pid))
            }
            None => (SPAWN_FAILED_EXIT_CODE, Duration::ZERO, None),
        };
        let mut status = shared.lock();
        status.current_pid = None;
        status.last_exit = Some(exit_code);
        if !should_restart(policy, exit_code, status.restart_count, status.stopped) {
            status.restarting = false;
            status.finished = true;
            let restarts = status.restart_count;
            let stopped = status.stopped;
            drop(status);
            logging::emit(
                Level::Info,
                "workload_exited",
                &[
                    ("id", json!(id)),
                    ("pid", json!(pid)),
                    ("exit_code", json!(exit_code)),
                    ("restart_count", json!(restarts)),
                    ("stopped", json!(stopped)),
                    ("policy", json!(policy)),
                ],
            );
            break;
        }
        let delay = backoff.next_delay(uptime);
        status.restarting = true;
        logging::emit(
            Level::Warn,
            "workload_restarting",
            &[
                ("id", json!(id)),
                ("pid", json!(pid)),
                ("exit_code", json!(exit_code)),
                ("uptime_ms", json!(uptime.as_millis() as u64)),
                ("restart_count", json!(status.restart_count)),
                ("restart_in_ms", json!(delay.as_millis() as u64)),
            ],
        );
        let deadline = Instant::now() + delay;
        while !status.stopped {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            status = shared
                .wake
                .wait_timeout(status, remaining)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
        if status.stopped {
            status.restarting = false;
            status.finished = true;
            break;
        }
        status.restart_count = status.restart_count.saturating_add(1);
        drop(status);
        match spec.spawn(&log) {
            Ok(run) => {
                started = Instant::now();
                let mut status = shared.lock();
                status.current_pid = Some(run.id());
                status.restarting = false;
                child = Some(run);
            }
            Err(error) => {
                logging::emit(
                    Level::Error,
                    "workload_restart_failed",
                    &[("id", json!(id)), ("error", json!(error.to_string()))],
                );
            }
        }
    }
    // The writer flushes what is queued and exits once the last reader
    // (a grandchild may still hold a pipe) reaches end of file.
    drop(log);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload_log::{LogLimits, RotatingLog};
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "temps-vm-agent-supervisor-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn fast_backoff() -> RestartBackoff {
        RestartBackoff::new(
            Duration::from_millis(10),
            Duration::from_millis(40),
            Duration::from_secs(10),
        )
    }

    fn shell(script: &str) -> ProcessSpec {
        ProcessSpec {
            cmd: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: HashMap::new(),
            cwd: "/".into(),
            uid: None,
            gid: None,
        }
    }

    fn pipeline(path: &std::path::Path) -> LogPipeline {
        let log = RotatingLog::open(path, LogLimits::default()).expect("open log");
        LogPipeline::start(log, |_| {}).expect("start pipeline")
    }

    fn wait_until(
        supervision: &Supervision,
        what: &str,
        done: impl Fn(&SupervisionStatus) -> bool,
    ) -> SupervisionStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = supervision.status();
            if done(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_log(path: &std::path::Path, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "log never showed {needle:?}: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_never_policy_runs_once_and_reports_the_exit_code() {
        let dir = temp_dir("never");
        let log = dir.join("out.log");
        let supervision = Supervision::start(
            shell("echo started; echo oops >&2; sleep 0.2; exit 7"),
            SpawnRestartPolicy::Never,
            fast_backoff(),
            pipeline(&log),
        )
        .expect("start");
        assert!(supervision.status().running());
        assert!(supervision.matches(supervision.id()));
        let status = wait_until(&supervision, "the exit", |s| s.finished);
        assert_eq!(status.last_exit, Some(7));
        assert_eq!(status.restart_count, 0);
        assert!(!status.running());
        let text = wait_for_log(&log, "oops");
        assert!(
            text.contains("started"),
            "stdout and stderr both land: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_always_policy_restarts_a_crashing_process_with_backoff() {
        let dir = temp_dir("always");
        let log = dir.join("out.log");
        let supervision = Supervision::start(
            shell("echo run; exit 3"),
            SpawnRestartPolicy::Always,
            fast_backoff(),
            pipeline(&log),
        )
        .expect("start");
        let status = wait_until(&supervision, "three restarts", |s| s.restart_count >= 3);
        assert!(!status.finished);
        assert_eq!(status.last_exit, Some(3));
        supervision.mark_stopped();
        let stopped = wait_until(&supervision, "the stop", |s| s.finished);
        assert!(stopped.stopped);
        let settled = stopped.restart_count;
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            supervision.status().restart_count,
            settled,
            "a stopped process is never restarted"
        );
        let text = wait_for_log(&log, "run");
        assert!(text.matches("run").count() >= 3, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn on_failure_stops_restarting_at_a_clean_exit_and_at_its_retry_limit() {
        let dir = temp_dir("on-failure");
        let marker = dir.join("ran-once");
        // Fails the first time, succeeds the second.
        let script = format!(
            "if [ -e {m} ]; then exit 0; else touch {m}; exit 1; fi",
            m = marker.display()
        );
        let supervision = Supervision::start(
            shell(&script),
            SpawnRestartPolicy::OnFailure { max_retries: None },
            fast_backoff(),
            pipeline(&dir.join("a.log")),
        )
        .expect("start");
        let status = wait_until(&supervision, "the clean exit", |s| s.finished);
        assert_eq!((status.restart_count, status.last_exit), (1, Some(0)));

        let limited = Supervision::start(
            shell("exit 5"),
            SpawnRestartPolicy::OnFailure {
                max_retries: Some(2),
            },
            fast_backoff(),
            pipeline(&dir.join("b.log")),
        )
        .expect("start");
        let status = wait_until(&limited, "the retry limit", |s| s.finished);
        assert_eq!((status.restart_count, status.last_exit), (2, Some(5)));
        assert!(!status.stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stop_during_the_backoff_cancels_the_pending_restart() {
        let dir = temp_dir("stop-in-backoff");
        let supervision = Supervision::start(
            shell("exit 1"),
            SpawnRestartPolicy::UnlessStopped,
            RestartBackoff::new(
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ),
            pipeline(&dir.join("out.log")),
        )
        .expect("start");
        wait_until(&supervision, "the backoff", |s| s.restarting);
        supervision.mark_stopped();
        let status = wait_until(&supervision, "the cancelled restart", |s| s.finished);
        assert_eq!(status.restart_count, 0);
        assert!(!status.restarting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_that_cannot_start_is_retried_as_a_failure() {
        let dir = temp_dir("vanish");
        let program = dir.join("app.sh");
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let supervision = Supervision::start(
            ProcessSpec {
                cmd: vec![program.display().to_string()],
                env: HashMap::new(),
                cwd: "/".into(),
                uid: None,
                gid: None,
            },
            SpawnRestartPolicy::OnFailure {
                max_retries: Some(3),
            },
            fast_backoff(),
            pipeline(&dir.join("out.log")),
        )
        .expect("start");
        std::fs::remove_file(&program).unwrap();
        let status = wait_until(&supervision, "the retry limit", |s| s.finished);
        assert_eq!(status.restart_count, 3);
        assert!(status.last_exit.is_some_and(|code| code != 0), "{status:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_that_does_not_exist_fails_the_start_naming_it() {
        let dir = temp_dir("missing");
        let error = Supervision::start(
            ProcessSpec {
                cmd: vec!["/nonexistent/app-server".into()],
                env: HashMap::new(),
                cwd: "/".into(),
                uid: None,
                gid: None,
            },
            SpawnRestartPolicy::Always,
            fast_backoff(),
            pipeline(&dir.join("out.log")),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("/nonexistent/app-server"),
            "{error}"
        );
        let empty = Supervision::start(
            ProcessSpec {
                cmd: Vec::new(),
                env: HashMap::new(),
                cwd: "/".into(),
                uid: None,
                gid: None,
            },
            SpawnRestartPolicy::Never,
            fast_backoff(),
            pipeline(&dir.join("out2.log")),
        )
        .unwrap_err();
        assert!(matches!(empty, SuperviseError::EmptyCommand));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_flooding_its_output_never_blocks_on_the_log() {
        // 16 MiB of output into a 64 KiB x 2 log: the process must finish
        // promptly and the files must stay bounded.
        let dir = temp_dir("flood");
        let path = dir.join("out.log");
        let log = RotatingLog::open(
            &path,
            LogLimits {
                max_bytes: 64 * 1024,
                max_files: 2,
            },
        )
        .expect("open log");
        let supervision = Supervision::start(
            shell("yes 0123456789abcdef0123456789abcdef | head -c 16777216; echo done"),
            SpawnRestartPolicy::Never,
            fast_backoff(),
            LogPipeline::start(log, |_| {}).expect("pipeline"),
        )
        .expect("start");
        let status = wait_until(&supervision, "the flood to end", |s| s.finished);
        assert_eq!(status.last_exit, Some(0));
        std::thread::sleep(Duration::from_millis(200));
        for index in 0..4 {
            let file = crate::workload_log::rotated_path(&path, index);
            if let Ok(meta) = std::fs::metadata(&file) {
                assert!(index < 2, "{} must not exist", file.display());
                assert!(
                    meta.len() <= 64 * 1024,
                    "{} is {} bytes",
                    file.display(),
                    meta.len()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
