// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end tests: bind the agent on a scratch socket, connect as a
//! client, drive the full protocol. These catch wiring bugs that the
//! unit tests don't: handshake ordering, broadcast fan-out, replay
//! semantics across attach.
//!
//! Tests must run serially. `spawn_pty` performs a fork()+exec(), which is
//! unsafe when other threads in the parent are mid-operation (tokio's
//! driver, file-descriptor state, etc.). Concurrent fork()s across tests
//! produce sporadic EOFs on the PTY master. The serialization lock below
//! is cheap and makes the suite deterministic.
//!
//! The std Mutex is deliberate: we need the guard to hold across the
//! entire async test body so no two fork()s overlap. A tokio Mutex would
//! also work but adds runtime overhead for no benefit here.
#![allow(clippy::await_holding_lock)]

use std::sync::Mutex;
use std::time::Duration;

static SERIAL: Mutex<()> = Mutex::new(());

use temps_pty_agent::protocol::{
    encode_resize, read_frame, write_frame, write_json_frame, OpenRequest, OpenedResponse,
    OP_INPUT, OP_KILL, OP_OPEN, OP_OPENED, OP_OUTPUT, OP_PING, OP_PONG, OP_RESIZE,
};
use tokio::net::UnixStream;

fn scratch_socket_path() -> String {
    let dir = std::env::temp_dir();
    let fname = format!(
        "temps-pty-agent-test-{}-{}.sock",
        std::process::id(),
        // nanosecond tick so concurrent tests in the same process don't
        // clobber each other's sockets.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    dir.join(fname).to_string_lossy().to_string()
}

/// Minimal env that works on any dev host — avoids sandbox-specific HOME
/// and PATH that don't exist on a macOS/Linux workstation.
fn test_env() -> Vec<(String, String)> {
    vec![
        ("TERM".into(), "xterm-256color".into()),
        (
            "PATH".into(),
            "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
        ),
        (
            "HOME".into(),
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
        ),
    ]
}

async fn start_agent() -> String {
    let path = scratch_socket_path();
    let p = path.clone();
    tokio::spawn(async move {
        let _ = temps_pty_agent::server::run(&p).await;
    });
    // Wait for bind.
    for _ in 0..50 {
        if std::path::Path::new(&path).exists() {
            return path;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("agent socket never appeared at {path}");
}

#[tokio::test]
async fn ping_pong_roundtrip() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = start_agent().await;
    let mut s = UnixStream::connect(&sock).await.unwrap();
    write_frame(&mut s, OP_PING, b"").await.unwrap();
    let (ty, _) = read_frame(&mut s).await.unwrap().unwrap();
    assert_eq!(ty, OP_PONG);
}

#[tokio::test]
async fn open_and_read_output() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = start_agent().await;
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let req = OpenRequest {
        tab_id: "t1".into(),
        kind: "shell".into(),
        cmd: "printf READY; sleep 3".into(),
        cols: 80,
        rows: 24,
        replay_bytes: 0,
        label: None,
        cwd: Some("/tmp".into()),
        env: Some(test_env()),
    };
    write_json_frame(&mut s, OP_OPEN, &req).await.unwrap();

    // First frame should be OPENED, then OUTPUT.
    let (ty, payload) = read_frame(&mut s).await.unwrap().unwrap();
    if ty != OP_OPENED {
        panic!(
            "expected OPENED (0x82), got 0x{ty:02x}; payload={}",
            String::from_utf8_lossy(&payload)
        );
    }
    let opened: OpenedResponse = serde_json::from_slice(&payload).unwrap();
    assert!(!opened.existed);
    assert!(opened.pid > 0);

    // Expect to see "READY" in the first OUTPUT chunk(s).
    let mut seen = Vec::new();
    for _ in 0..10 {
        let framed = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut s))
            .await
            .expect("output timeout")
            .unwrap();
        let Some((ty, p)) = framed else { break };
        if ty == OP_OUTPUT {
            seen.extend_from_slice(&p);
            if String::from_utf8_lossy(&seen).contains("READY") {
                break;
            }
        }
    }
    let combined = String::from_utf8_lossy(&seen);
    assert!(
        combined.contains("READY"),
        "did not see READY; got {combined:?}"
    );

    // Kill so `sleep 3` doesn't keep the test alive.
    write_frame(&mut s, OP_KILL, b"").await.unwrap();
}

#[tokio::test]
async fn resize_does_not_disconnect() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = start_agent().await;
    let mut s = UnixStream::connect(&sock).await.unwrap();
    let req = OpenRequest {
        tab_id: "resize-tab".into(),
        kind: "shell".into(),
        cmd: "sleep 2".into(),
        cols: 80,
        rows: 24,
        replay_bytes: 0,
        label: None,
        cwd: Some("/tmp".into()),
        env: Some(test_env()),
    };
    write_json_frame(&mut s, OP_OPEN, &req).await.unwrap();
    // Drain the OPENED frame.
    let _ = read_frame(&mut s).await.unwrap().unwrap();
    // Send resize.
    let ws = encode_resize(120, 40);
    write_frame(&mut s, OP_RESIZE, &ws).await.unwrap();
    // Ping — if resize crashed the handler the ping would fail.
    write_frame(&mut s, OP_PING, b"").await.unwrap();
    let (ty, _) = read_frame(&mut s).await.unwrap().unwrap();
    assert_eq!(ty, OP_PONG);
    write_frame(&mut s, OP_KILL, b"").await.unwrap();
}

#[tokio::test]
async fn input_reaches_child() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = start_agent().await;
    let mut s = UnixStream::connect(&sock).await.unwrap();
    // Use a shell that reads a line then echoes it with a marker. This is
    // more reliable than bare `cat` under a PTY where termios line-discipline
    // echo and cooked-mode buffering can re-order bytes.
    let req = OpenRequest {
        tab_id: "cat-tab".into(),
        kind: "shell".into(),
        cmd: "read line; printf 'GOT[%s]\\n' \"$line\"; sleep 1".into(),
        cols: 80,
        rows: 24,
        replay_bytes: 0,
        label: None,
        cwd: Some("/tmp".into()),
        env: Some(test_env()),
    };
    write_json_frame(&mut s, OP_OPEN, &req).await.unwrap();
    let _ = read_frame(&mut s).await.unwrap().unwrap(); // OPENED

    // Give the shell a moment to reach `read`.
    tokio::time::sleep(Duration::from_millis(200)).await;
    write_frame(&mut s, OP_INPUT, b"ping-abc\n").await.unwrap();
    let mut got = Vec::new();
    for _ in 0..30 {
        let framed = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut s))
            .await
            .expect("read timeout")
            .unwrap();
        let Some((ty, p)) = framed else { break };
        if ty == OP_OUTPUT {
            got.extend_from_slice(&p);
            if String::from_utf8_lossy(&got).contains("GOT[ping-abc]") {
                break;
            }
        }
    }
    assert!(
        String::from_utf8_lossy(&got).contains("GOT[ping-abc]"),
        "did not see marker; got: {:?}",
        String::from_utf8_lossy(&got)
    );
    write_frame(&mut s, OP_KILL, b"").await.unwrap();
}
