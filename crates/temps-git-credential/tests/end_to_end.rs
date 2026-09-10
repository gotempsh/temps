// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end test: helper binary ↔ daemon binary ↔ mocked control plane.
//!
//! Spins up:
//! 1. A tiny in-process HTTP server impersonating the control-plane
//!    `/api/workspace/git-credential` endpoint.
//! 2. The real `temps-git-credential-daemon` binary, pointed at that
//!    server via `TEMPS_GIT_CREDENTIAL_DAEMON_ENV` and listening on a
//!    temp-dir Unix socket.
//! 3. The real `temps-git-credential-helper` binary, fed the standard
//!    git credential protocol on stdin and pointed at the same socket.
//!
//! Verifies the helper outputs the username/password the mock server
//! returned, that the daemon refuses cross-host requests, and that
//! refused mints surface a clear stderr message.
//!
//! Skips gracefully if the workspace binaries haven't been built yet
//! (CI typically builds them first; locally `cargo test` does too via
//! `--bin` deps, but a clean checkout might not).

use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener as AsyncTcpListener;

/// Tiny mock control-plane server. Returns whatever JSON we ask it to.
async fn mock_server(
    port: u16,
    response_status: u16,
    response_body: serde_json::Value,
    requests: Arc<AtomicU64>,
) {
    let listener = AsyncTcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .expect("bind mock server");
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(p) => p,
            Err(_) => return,
        };
        requests.fetch_add(1, Ordering::SeqCst);
        let body = response_body.to_string();
        let status_text = match response_status {
            200 => "OK",
            403 => "Forbidden",
            502 => "Bad Gateway",
            _ => "Whatever",
        };
        let payload = format!(
            "HTTP/1.1 {status} {status_text}\r\n\
             content-type: application/json\r\n\
             content-length: {len}\r\n\
             connection: close\r\n\r\n\
             {body}",
            status = response_status,
            len = body.len(),
        );
        // Drain request before responding; otherwise reqwest may
        // sometimes treat the early close as connection-reset.
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_millis(200), sock.read(&mut buf)).await;
        use tokio::io::AsyncWriteExt;
        let _ = sock.write_all(payload.as_bytes()).await;
        let _ = sock.shutdown().await;
    }
}

fn pick_free_port() -> u16 {
    // Bind sync to grab the port, then drop so the daemon's reqwest can
    // reach it. There's a tiny race window but for a test it's fine.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn workspace_target_dir() -> std::path::PathBuf {
    // Tests run inside the workspace target/debug/deps/, so up two parents.
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps
    p.pop(); // debug or release
    p
}

fn binary_path(name: &str) -> std::path::PathBuf {
    workspace_target_dir().join(name)
}

/// Skip the test if the binaries aren't built (e.g. running an isolated
/// `cargo test --test end_to_end` without first `cargo build`).
fn binaries_or_skip() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let helper = binary_path("temps-git-credential-helper");
    let daemon = binary_path("temps-git-credential-daemon");
    if !helper.exists() || !daemon.exists() {
        eprintln!(
            "skipping: required binaries not built ({} or {} missing)",
            helper.display(),
            daemon.display()
        );
        return None;
    }
    Some((helper, daemon))
}

/// Happy path: helper asks daemon, daemon hits mock server, returns a
/// fake credential, helper writes it to stdout in git's format.
#[tokio::test(flavor = "multi_thread")]
async fn helper_daemon_mock_server_roundtrip() {
    let (helper, daemon) = match binaries_or_skip() {
        Some(p) => p,
        None => return,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("git.sock");
    let env_file = dir.path().join("daemon.env");
    let mock_port = pick_free_port();

    // Spin up the mock server.
    let mock_requests = Arc::new(AtomicU64::new(0));
    let mock_handle = tokio::spawn(mock_server(
        mock_port,
        200,
        json!({
            "username": "x-access-token",
            "password": "ghs_FAKE_e2e",
            "expires_at": "2099-01-01T00:00:00Z",
        }),
        mock_requests.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Provision the daemon's env file.
    std::fs::write(
        &env_file,
        format!(
            "TEMPS_API_URL=http://127.0.0.1:{}\nTEMPS_API_TOKEN=dt_test_token\n",
            mock_port
        ),
    )
    .expect("write env file");

    // Launch the daemon.
    let mut daemon_proc = Command::new(&daemon)
        .env("TEMPS_GIT_CREDENTIAL_DAEMON_ENV", &env_file)
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    // Wait for socket to appear.
    let mut waited = 0u32;
    while !socket_path.exists() && waited < 50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert!(socket_path.exists(), "daemon never created socket");

    // Run the helper as git would: pass `get` and feed stdin.
    let mut helper_proc = Command::new(&helper)
        .arg("get")
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");

    {
        let stdin = helper_proc.stdin.as_mut().expect("helper stdin");
        stdin
            .write_all(b"protocol=https\nhost=github.com\npath=acme/web\n\n")
            .expect("write helper stdin");
    }

    let helper_out = helper_proc.wait_with_output().expect("helper wait");
    assert!(
        helper_out.status.success(),
        "helper exited non-zero: stderr={:?}",
        String::from_utf8_lossy(&helper_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&helper_out.stdout);
    assert!(
        stdout.contains("username=x-access-token"),
        "helper stdout missing username, got: {stdout}"
    );
    assert!(
        stdout.contains("password=ghs_FAKE_e2e"),
        "helper stdout missing password, got: {stdout}"
    );

    // Mock server received exactly one mint request.
    assert_eq!(
        mock_requests.load(Ordering::SeqCst),
        1,
        "mock server should have received one mint request"
    );

    // Cleanup.
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
    mock_handle.abort();
}

/// Refusal path: control plane returns 403, daemon must surface as
/// `Refused`, helper writes a clear stderr line and exits 0 (so git
/// falls through to the next helper or prompts).
#[tokio::test(flavor = "multi_thread")]
async fn helper_surfaces_control_plane_refusal() {
    let (helper, daemon) = match binaries_or_skip() {
        Some(p) => p,
        None => return,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("git.sock");
    let env_file = dir.path().join("daemon.env");
    let mock_port = pick_free_port();

    let mock_requests = Arc::new(AtomicU64::new(0));
    let mock_handle = tokio::spawn(mock_server(
        mock_port,
        403,
        json!({
            "type": "about:blank",
            "title": "Cross-Project Credential Request Denied",
            "status": 403,
            "detail": "wrong project",
        }),
        mock_requests.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;

    std::fs::write(
        &env_file,
        format!(
            "TEMPS_API_URL=http://127.0.0.1:{}\nTEMPS_API_TOKEN=dt_test\n",
            mock_port
        ),
    )
    .unwrap();

    let mut daemon_proc = Command::new(&daemon)
        .env("TEMPS_GIT_CREDENTIAL_DAEMON_ENV", &env_file)
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut waited = 0u32;
    while !socket_path.exists() && waited < 50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert!(socket_path.exists());

    let mut helper_proc = Command::new(&helper)
        .arg("get")
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    helper_proc
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"protocol=https\nhost=github.com\npath=victim/repo\n\n")
        .unwrap();
    let out = helper_proc.wait_with_output().unwrap();

    // Helper exits 0 even on refusal — that's how git's protocol works
    // (no creds returned, git falls through). But the user should see
    // *why* on stderr.
    assert!(out.status.success(), "helper should exit 0 on refusal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("password="),
        "no password should leak on refusal, got stdout={stdout}"
    );
    assert!(
        stderr.contains("refused"),
        "stderr should explain refusal, got: {stderr}"
    );

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
    mock_handle.abort();
}

/// Path validation: a malformed git request (no `path=`) must produce a
/// helper-side error and never even hit the daemon socket.
#[tokio::test(flavor = "multi_thread")]
async fn helper_rejects_request_without_path() {
    let (helper, _daemon) = match binaries_or_skip() {
        Some(p) => p,
        None => return,
    };

    // No daemon needed — helper should fail at parse time.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("git.sock");

    let mut helper_proc = Command::new(&helper)
        .arg("get")
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    helper_proc
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"protocol=https\nhost=github.com\n\n")
        .unwrap();
    let out = helper_proc.wait_with_output().unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.is_empty(),
        "helper should output nothing on parse error"
    );
    assert!(
        stderr.contains("malformed") || stderr.contains("path"),
        "stderr should explain the missing path, got: {stderr}"
    );
}

/// Crash + relaunch path. Models what `message_executor`'s launch
/// script does when it detects a dead daemon: remove the stale socket,
/// relaunch the daemon, verify the new instance serves requests.
///
/// Why this matters: the original "git push fails inside sandbox" bug
/// happened because the launch script's idempotency check (`[ -S socket
/// ]`) would skip the relaunch if the socket file still existed —
/// which it does after a crash. This test pins the post-fix invariant:
/// after `rm -f socket && relaunch`, the daemon comes back healthy.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_can_be_relaunched_after_crash() {
    let (helper, daemon) = match binaries_or_skip() {
        Some(p) => p,
        None => return,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("git.sock");
    let env_file = dir.path().join("daemon.env");
    let mock_port = pick_free_port();

    let mock_requests = Arc::new(AtomicU64::new(0));
    let mock_handle = tokio::spawn(mock_server(
        mock_port,
        200,
        json!({
            "username": "x-access-token",
            "password": "ghs_RELAUNCH",
            "expires_at": "2099-01-01T00:00:00Z",
        }),
        mock_requests.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;

    std::fs::write(
        &env_file,
        format!(
            "TEMPS_API_URL=http://127.0.0.1:{}\nTEMPS_API_TOKEN=dt_relaunch\n",
            mock_port
        ),
    )
    .expect("write env file");

    // First daemon instance.
    let mut daemon_a = Command::new(&daemon)
        .env("TEMPS_GIT_CREDENTIAL_DAEMON_ENV", &env_file)
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon A");

    let mut waited = 0u32;
    while !socket_path.exists() && waited < 50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert!(socket_path.exists(), "daemon A never bound socket");

    // Sanity: helper works against daemon A.
    {
        let mut h = Command::new(&helper)
            .arg("get")
            .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn helper A");
        h.stdin
            .as_mut()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\npath=acme/web\n\n")
            .unwrap();
        let out = h.wait_with_output().unwrap();
        assert!(out.status.success(), "pre-crash helper should succeed");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ghs_RELAUNCH"),
            "pre-crash helper should return mock credential"
        );
    }

    // Simulate daemon crash with SIGKILL — bypasses any cleanup the
    // daemon would do on graceful shutdown, leaving the socket file on
    // disk. This is exactly the state that tripped the old idempotency
    // check (`[ -S socket ]` was true even though no daemon listened).
    let _ = daemon_a.kill();
    let _ = daemon_a.wait();

    // Stale socket invariant: file exists but connect() now fails
    // because no process is bound to it. This is the precondition the
    // launch-script fix explicitly cleans up before relaunching.
    assert!(
        socket_path.exists(),
        "SIGKILL should leave stale socket on disk — that is the bug \
         the relaunch path has to handle"
    );

    // Mirror the launch script's recovery: remove stale socket, then
    // relaunch the daemon. (The actual script in message_executor.rs
    // also checks `pgrep` first; we already KNOW the process is dead so
    // we go straight to the cleanup + relaunch.)
    std::fs::remove_file(&socket_path).expect("rm stale socket");

    let mut daemon_b = Command::new(&daemon)
        .env("TEMPS_GIT_CREDENTIAL_DAEMON_ENV", &env_file)
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon B");

    let mut waited = 0u32;
    while !socket_path.exists() && waited < 50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert!(
        socket_path.exists(),
        "daemon B never bound socket after relaunch"
    );

    // Verify the new daemon serves a fresh credential — same mock
    // server, but a separate request from helper B's perspective.
    let pre_count = mock_requests.load(Ordering::SeqCst);
    {
        let mut h = Command::new(&helper)
            .arg("get")
            .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn helper B");
        h.stdin
            .as_mut()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\npath=acme/web\n\n")
            .unwrap();
        let out = h.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "post-relaunch helper should succeed, stderr={:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ghs_RELAUNCH"),
            "post-relaunch helper should return mock credential"
        );
    }
    // The relaunched daemon has a fresh empty cache, so it MUST hit
    // the mock server — confirming we're talking to a live process and
    // not somehow reading stale state.
    assert!(
        mock_requests.load(Ordering::SeqCst) > pre_count,
        "post-relaunch helper should have triggered a new mint call"
    );

    let _ = daemon_b.kill();
    let _ = daemon_b.wait();
    mock_handle.abort();
}

/// SIGHUP must reload the env file in place rather than killing the
/// daemon. The previous behavior (default disposition = terminate)
/// caused a token-rotation refresh to take down the credential
/// pipeline until the next workspace open. The new behavior reads the
/// updated env file and serves credentials with the rotated token on
/// the next request.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_reloads_env_file_on_sighup() {
    let (helper, daemon) = match binaries_or_skip() {
        Some(p) => p,
        None => return,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("git.sock");
    let env_file = dir.path().join("daemon.env");

    // Two mock servers on different ports — the env file rewrite
    // points the daemon at the second one. If SIGHUP truly reloaded,
    // the post-HUP helper request lands on server B.
    let port_a = pick_free_port();
    let port_b = pick_free_port();
    let requests_a = Arc::new(AtomicU64::new(0));
    let requests_b = Arc::new(AtomicU64::new(0));

    let mock_a = tokio::spawn(mock_server(
        port_a,
        200,
        json!({
            "username": "x-access-token",
            "password": "ghs_FROM_A",
            "expires_at": "2099-01-01T00:00:00Z",
        }),
        requests_a.clone(),
    ));
    let mock_b = tokio::spawn(mock_server(
        port_b,
        200,
        json!({
            "username": "x-access-token",
            "password": "ghs_FROM_B",
            "expires_at": "2099-01-01T00:00:00Z",
        }),
        requests_b.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Start pointing at server A.
    std::fs::write(
        &env_file,
        format!(
            "TEMPS_API_URL=http://127.0.0.1:{}\nTEMPS_API_TOKEN=dt_v1\n",
            port_a
        ),
    )
    .unwrap();

    let mut daemon_proc = Command::new(&daemon)
        .env("TEMPS_GIT_CREDENTIAL_DAEMON_ENV", &env_file)
        .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");
    let daemon_pid = daemon_proc.id();

    let mut waited = 0u32;
    while !socket_path.exists() && waited < 50 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert!(socket_path.exists());

    // First request — must hit server A. Use a unique repo so the
    // daemon's per-(host,owner,repo,op) cache doesn't mask the second
    // request.
    {
        let mut h = Command::new(&helper)
            .arg("get")
            .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        h.stdin
            .as_mut()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\npath=acme/repo-a\n\n")
            .unwrap();
        let out = h.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ghs_FROM_A"),
            "pre-HUP request should hit server A"
        );
    }
    assert_eq!(
        requests_a.load(Ordering::SeqCst),
        1,
        "server A should have served exactly one request"
    );

    // Rewrite env file to point at server B, then SIGHUP.
    std::fs::write(
        &env_file,
        format!(
            "TEMPS_API_URL=http://127.0.0.1:{}\nTEMPS_API_TOKEN=dt_v2\n",
            port_b
        ),
    )
    .unwrap();

    // Use nix's re-exported libc — Tokio's process API doesn't expose
    // signals directly, and `nix` is already a workspace dependency so
    // we don't pull in a new crate. Signal 0 is the standard "is the
    // process alive?" probe (no signal delivered, just permission +
    // existence check).
    use nix::libc;
    unsafe {
        libc::kill(daemon_pid as libc::pid_t, libc::SIGHUP);
    }

    // Give the reloader a moment to swap the config.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The daemon process must still be alive — the whole point of the
    // SIGHUP handler is that it does NOT terminate.
    let still_alive = unsafe { libc::kill(daemon_pid as libc::pid_t, 0) } == 0;
    assert!(
        still_alive,
        "daemon should survive SIGHUP, but the process is gone"
    );

    // Second request, different repo to dodge the cache. Must now hit
    // server B because the reloader swapped the API URL.
    {
        let mut h = Command::new(&helper)
            .arg("get")
            .env("TEMPS_GIT_CREDENTIAL_SOCKET", &socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        h.stdin
            .as_mut()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\npath=acme/repo-b\n\n")
            .unwrap();
        let out = h.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ghs_FROM_B"),
            "post-HUP request should hit server B (env reload effective)"
        );
    }
    assert_eq!(
        requests_b.load(Ordering::SeqCst),
        1,
        "server B should have served exactly one request after SIGHUP"
    );

    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
    mock_a.abort();
    mock_b.abort();
}
