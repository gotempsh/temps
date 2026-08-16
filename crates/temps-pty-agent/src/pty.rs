//! PTY spawning + ioctl wrappers. Runs the child under a pseudo-terminal
//! whose master end we own (read/write + TIOCSWINSZ for resize). The slave
//! end becomes the child's stdin/stdout/stderr/controlling-tty.
//!
//! We deliberately avoid `portable-pty` and other async-pty crates: they're
//! great for embedders but drag in extra threads and buffering we'd have to
//! work around. Our needs here are narrow — openpty, fork/exec into bash,
//! async I/O on the master fd — so the ~50 lines below is cheaper than the
//! dependency.
//!
//! The master fd is wrapped in `tokio::fs::File` via `from_std`, which gives
//! us `AsyncRead`/`AsyncWrite` on an arbitrary Unix fd by delegating the
//! actual read/write to tokio's blocking-op threadpool. That's the same
//! strategy tokio itself uses for stdin/stdout in `tokio::io::stdin`.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

use nix::libc;
use nix::pty::{openpty, Winsize};
use tokio::fs::File;

/// Everything we need to talk to a spawned PTY child.
pub struct Pty {
    /// Master end as an async-capable handle. Reads return the child's
    /// stdout/stderr; writes forward to the child's stdin.
    pub master: File,
    /// Raw master fd kept alongside for ioctls (TIOCSWINSZ on resize).
    pub master_fd: i32,
    /// OS pid of the spawned child.
    pub pid: i32,
}

/// Spawn `/bin/sh -c {cmd}` under a fresh PTY. The child gets `env` merged
/// over the agent's own environment, cwd set to `cwd`, and its controlling
/// tty set to the PTY slave.
pub fn spawn_pty(
    cmd: &str,
    cwd: &str,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
) -> io::Result<Pty> {
    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Leave the pty at its kernel defaults — cooked mode with ECHO and
    // ICANON on, exactly what `openpty` hands back and what every other
    // terminal multiplexer (ssh, script, tmux) starts from.
    //
    // This used to call `cfmakeraw()` here, on the theory that raw mode was
    // needed for keystrokes to reach the child before Enter. That is the
    // child's decision, not ours: readline (bash) and full-screen TUIs
    // (claude, vim) each call `tcsetattr` themselves when they want
    // char-at-a-time input. Forcing raw up front only cleared ECHO for
    // everything that *doesn't* renegotiate — which is bash, whose readline
    // then assumed the driver was echoing. The result was a shell where
    // commands ran but the user could not see a single character they
    // typed; `stty -a` inside the session reported `-echo -icanon`.
    //
    // Terminals do not manage the line discipline for their children. We
    // move bytes; the program on the far end owns its termios.
    let result =
        openpty(Some(&winsize), None).map_err(|e| io::Error::other(format!("openpty: {e}")))?;

    let master_fd: OwnedFd = result.master;
    let slave_fd: OwnedFd = result.slave;

    // Use Command with pre_exec to wire up the slave fd as controlling tty.
    // Must run `setsid` + `ioctl(TIOCSCTTY)` so the child's session owns the
    // tty — otherwise it won't receive SIGWINCH on resize and TUIs like
    // claude's won't repaint.
    let slave_raw = slave_fd.as_raw_fd();
    let master_raw = master_fd.as_raw_fd();

    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(cmd).current_dir(cwd);
    // env_clear + env-pair merge keeps us from leaking the agent's own env
    // (HOME=/root, PATH=tiny) into the user's shell.
    command.env_clear();
    for (k, v) in env {
        command.env(k, v);
    }

    // SAFETY: pre_exec runs in the forked child after fork() and before
    // execve. Only async-signal-safe syscalls are allowed; the libc calls
    // below satisfy that. We must not allocate, lock, or touch Rust's
    // runtime state here.
    unsafe {
        command.pre_exec(move || {
            // New session → become process group leader without a
            // controlling tty yet.
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            // Acquire the slave as the controlling tty.
            if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            // Wire fds 0/1/2 to the slave.
            if libc::dup2(slave_raw, 0) < 0
                || libc::dup2(slave_raw, 1) < 0
                || libc::dup2(slave_raw, 2) < 0
            {
                return Err(io::Error::last_os_error());
            }
            // Close any lingering fds (slave original + master) so they
            // don't leak into the child. After dup2 the three std fds are
            // independent handles to the slave pty so closing slave_raw is
            // safe. master_raw isn't used in the child at all.
            if slave_raw > 2 {
                libc::close(slave_raw);
            }
            libc::close(master_raw);
            Ok(())
        });
    }

    let child = command.spawn()?;
    let pid = child.id() as i32;
    // We don't want Rust dropping the Child — the agent itself tracks the
    // pid and will reap via waitpid(). `forget` leaks only the Child handle
    // struct, not the process.
    std::mem::forget(child);

    // Slave fd stays open in the child; in the parent we close it.
    drop(slave_fd);

    // Convert the master OwnedFd into a tokio File for async I/O. We keep
    // the raw fd separately for ioctl calls (TIOCSWINSZ).
    let std_file = unsafe { std::fs::File::from_raw_fd(master_fd.as_raw_fd()) };
    // The OwnedFd would close the master on drop, but std::fs::File now
    // owns the fd too — forget the OwnedFd to avoid a double close.
    std::mem::forget(master_fd);
    let tokio_file = File::from_std(std_file);

    Ok(Pty {
        master: tokio_file,
        master_fd: master_raw,
        pid,
    })
}

/// Forward a window-size change to the PTY. Safe to call concurrently with
/// reads/writes on the master fd.
pub fn resize_pty(master_fd: i32, cols: u16, rows: u16) -> io::Result<()> {
    let ws = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: master_fd is the raw fd from openpty(); TIOCSWINSZ expects a
    // pointer to Winsize, which is exactly our layout.
    let rc = unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ as _, &ws as *const _) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Fire-and-forget kill: SIGTERM with a 2 s grace period followed by SIGKILL.
/// Runs on its own blocking task so the caller can keep streaming to other
/// subscribers without waiting for the wait().
pub async fn kill_tree(pid: i32) {
    use tokio::time::{sleep, Duration};
    let _ = tokio::task::spawn_blocking(move || unsafe {
        // Negative pid = send to the process group (the PTY session), so we
        // catch any child processes the program forked.
        libc::kill(-pid, libc::SIGTERM);
    })
    .await;
    sleep(Duration::from_secs(2)).await;
    let _ = tokio::task::spawn_blocking(move || unsafe {
        libc::kill(-pid, libc::SIGKILL);
    })
    .await;
}

/// Non-blocking wait for a specific child. Returns `Some((code, signal))`
/// if the child has exited, `None` if it's still running. Called from the
/// agent's reaper tick.
pub fn try_reap(pid: i32) -> Option<(Option<i32>, Option<i32>)> {
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid is safe to call on any pid; WNOHANG means it returns
    // immediately if the child hasn't exited yet.
    let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if rc <= 0 {
        return None;
    }
    if libc::WIFEXITED(status) {
        Some((Some(libc::WEXITSTATUS(status)), None))
    } else if libc::WIFSIGNALED(status) {
        Some((None, Some(libc::WTERMSIG(status))))
    } else {
        // Stopped/continued — treat as still running.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn spawn_echo_and_read_output() {
        let mut pty = spawn_pty(
            "printf hello; exit 0",
            "/tmp",
            &[("PATH".into(), "/usr/bin:/bin".into())],
            80,
            24,
        )
        .expect("spawn");
        let mut buf = [0u8; 64];
        // Give the child a moment to run + write its output.
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), pty.master.read(&mut buf))
            .await
            .expect("pty read timed out")
            .expect("pty read failed");
        assert!(n > 0, "expected some bytes from child");
        // PTY often CR-LF translates, so match loosely.
        let s = String::from_utf8_lossy(&buf[..n]);
        assert!(s.contains("hello"), "output did not contain 'hello': {s:?}");
    }

    /// The pty must start with ECHO and ICANON on.
    ///
    /// This is the regression guard for a shell in which commands ran but
    /// the user could not see a single character they typed: the pty was
    /// put into raw mode at creation, clearing ECHO, and bash's readline
    /// assumed the driver was echoing. Anything that wants raw mode — vim,
    /// claude, readline itself — sets it per-program; a terminal must not
    /// pre-empt that decision for the whole session.
    #[tokio::test]
    async fn pty_starts_with_echo_and_canonical_mode_enabled() {
        use nix::sys::termios::{self, LocalFlags};

        let pty = spawn_pty(
            "sleep 5",
            "/tmp",
            &[("PATH".into(), "/usr/bin:/bin".into())],
            80,
            24,
        )
        .expect("spawn");

        let attrs = termios::tcgetattr(&pty.master).expect("read pty termios");
        assert!(
            attrs.local_flags.contains(LocalFlags::ECHO),
            "pty must start with ECHO enabled — without it the user types blind"
        );
        assert!(
            attrs.local_flags.contains(LocalFlags::ICANON),
            "pty must start in canonical mode; programs opt into raw themselves"
        );
        kill_tree(pty.pid).await;
    }

    #[tokio::test]
    async fn resize_does_not_error() {
        let pty = spawn_pty(
            "sleep 5",
            "/tmp",
            &[("PATH".into(), "/usr/bin:/bin".into())],
            80,
            24,
        )
        .expect("spawn");
        resize_pty(pty.master_fd, 120, 40).expect("resize should succeed");
        kill_tree(pty.pid).await;
    }
}
