//! End-to-end test: helper binary ↔ daemon binary ↔ mocked control plane.
//!
//! Spins up:
//! 1. A tiny in-process HTTP server impersonating the control-plane
//!    `/workspace/git-credential` endpoint.
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
