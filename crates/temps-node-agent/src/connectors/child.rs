// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connector child processes that die with the agent.
//!
//! The agent unit runs with `KillMode=process` so a restart keeps sandbox VMs
//! and containers alive, which also means systemd no longer kills anything
//! else in the unit's cgroup. The agent has no SIGTERM handler, so on
//! `systemctl restart` it exits without running any `Drop`, and
//! `kill_on_drop` never fires either: a cloudflared started by the previous
//! agent kept running next to the one the new agent started ("Unit process
//! ... remains running after unit stopped").
//!
//! On Linux every connector child therefore asks the kernel for
//! `PR_SET_PDEATHSIG(SIGTERM)`, which holds however the agent ends (SIGTERM,
//! SIGKILL after `TimeoutStopSec`, a panic, the OOM killer). SIGTERM, not
//! SIGKILL, so cloudflared still drains within its `--grace-period`.
//!
//! The kernel ties that signal to the *thread* that forked the child, not
//! the process. A tokio worker can retire while the agent runs: any
//! `block_in_place` (several provider crates linked into the agent use it)
//! hands the worker's core to a new thread, and the old one exits after its
//! idle keep-alive. Forking from such a thread would kill a healthy tunnel
//! seconds later. So children are forked from one dedicated thread that
//! lives for the whole process.

use std::io;

use tokio::process::{Child, Command};

/// Start `command` so the child gets SIGTERM when the agent process ends.
///
/// Elsewhere than Linux there is no parent-death signal, and this is a plain
/// spawn.
pub async fn spawn_tied_to_agent(command: Command) -> io::Result<Child> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn(command).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut command = command;
        command.spawn()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::sync::OnceLock;

    use tokio::process::{Child, Command};
    use tokio::runtime::Handle;
    use tokio::sync::{mpsc, oneshot};

    /// Connector starts are rare (a handful per sync); a full queue makes the
    /// caller wait, it never drops a start.
    const QUEUE: usize = 16;

    struct Request {
        command: Command,
        /// The caller's runtime: the child's exit is reaped by its process
        /// driver.
        runtime: Handle,
        reply: oneshot::Sender<io::Result<Child>>,
    }

    /// The spawner thread's queue, or why the thread could not start. The
    /// sender lives in a static, so the thread never sees its queue close and
    /// never exits.
    static SPAWNER: OnceLock<Result<mpsc::Sender<Request>, String>> = OnceLock::new();

    fn spawner() -> io::Result<&'static mpsc::Sender<Request>> {
        SPAWNER
            .get_or_init(|| {
                let (tx, mut rx) = mpsc::channel::<Request>(QUEUE);
                std::thread::Builder::new()
                    .name("temps-connector-spawner".into())
                    .spawn(move || {
                        while let Some(Request {
                            mut command,
                            runtime,
                            reply,
                        }) = rx.blocking_recv()
                        {
                            let _runtime = runtime.enter();
                            let _ = reply.send(command.spawn());
                        }
                    })
                    .map(|_| tx)
                    .map_err(|e| format!("could not start the connector spawner thread: {e}"))
            })
            .as_ref()
            .map_err(|reason| io::Error::other(reason.clone()))
    }

    pub(super) async fn spawn(mut command: Command) -> io::Result<Child> {
        die_with_agent(command.as_std_mut());
        let (reply, child) = oneshot::channel();
        spawner()?
            .send(Request {
                command,
                runtime: Handle::current(),
                reply,
            })
            .await
            .map_err(|_| io::Error::other("the connector spawner thread has stopped"))?;
        child
            .await
            .map_err(|_| io::Error::other("the connector spawner thread dropped the request"))?
    }

    /// Have the kernel send SIGTERM to the child when the thread that forks
    /// it exits; see the module docs for why that must be a thread that
    /// lives as long as the agent.
    pub(super) fn die_with_agent(command: &mut std::process::Command) {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: the hook runs in the forked child before exec and only
        // makes async-signal-safe calls (prctl, getppid); it does not
        // allocate.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGTERM as libc::c_ulong,
                    0,
                    0,
                    0,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                // The agent died between fork and prctl: the signal will never
                // come, so do not exec an orphan.
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn sleeper() -> std::process::Command {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        command
    }

    /// Polls `child` until it exits or `within` passes.
    fn exited_within(child: &mut std::process::Child, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_tied_child_gets_sigterm_when_its_forking_thread_ends() {
        // The parent-death signal follows the forking thread, so a thread
        // that exits stands in for the agent process ending.
        let mut child = std::thread::spawn(|| {
            let mut command = sleeper();
            linux::die_with_agent(&mut command);
            command.spawn().expect("spawn sleep")
        })
        .join()
        .expect("spawning thread");

        assert!(
            exited_within(&mut child, Duration::from_secs(5)),
            "the child outlived its parent"
        );
        let status = child.wait().expect("wait");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
    }

    #[test]
    fn an_untied_child_outlives_its_forking_thread() {
        // The control for the test above: without the hook the same child
        // keeps running, so the signal there came from the hook.
        let mut child = std::thread::spawn(|| sleeper().spawn().expect("spawn sleep"))
            .join()
            .expect("spawning thread");

        assert!(!exited_within(&mut child, Duration::from_millis(500)));
        child.kill().expect("kill");
        let _ = child.wait();
    }

    #[test]
    fn a_connector_outlives_the_thread_and_runtime_that_asked_for_it() {
        // The regression a naive pre_exec on a tokio worker would cause: the
        // requesting thread (and its runtime) go away while the agent lives
        // on, and the connector must keep running.
        let mut child = std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async {
                spawn_tied_to_agent(Command::from(sleeper()))
                    .await
                    .expect("spawn sleep")
            })
        })
        .join()
        .expect("requesting thread");

        std::thread::sleep(Duration::from_millis(500));
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "the connector died with the thread that requested it"
        );
        let pid = child.id().expect("pid") as libc::pid_t;
        // SAFETY: a plain signal to our own child.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}
