//! Wire protocol between the host-side Firecracker sandbox provider and the
//! in-guest `temps-vm-agent` (ADR-029 §5).
//!
//! Transport: one RPC per vsock connection. The host connects through
//! Firecracker's hybrid vsock Unix socket (`CONNECT <port>\n` handshake),
//! then sends a single length-prefixed request and reads a single
//! length-prefixed response. Framing is `u32` big-endian byte length
//! followed by a JSON payload — same shape as the pty-agent protocol so a
//! later unification is mechanical.
//!
//! Binary file contents travel hex-encoded. That doubles the on-wire size,
//! but keeps the protocol JSON-debuggable and dependency-free; vsock
//! throughput makes this a non-issue at sandbox file sizes. Exec output is
//! lossy UTF-8 by design — it feeds `SandboxExecResult { stdout: String }`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vsock port the agent listens on for exec/fs RPCs.
pub const AGENT_PORT: u32 = 52;

/// Frames larger than this are rejected — the peer is untrusted.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Default working directory for sandbox commands; created by the agent at
/// boot when missing. Matches the Docker sandboxes' work dir so the public
/// API's `work_dir` field means the same thing on both backends.
pub const WORK_DIR: &str = "/workspace";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Exec {
        cmd: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        user: Option<u32>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    WriteFile {
        path: String,
        data_hex: String,
        mode: u32,
    },
    ReadFile {
        path: String,
    },
    Mkdir {
        path: String,
        mode: u32,
    },
    Kill {
        pattern: String,
        signal: i32,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong,
    Exec {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    File {
        data_hex: String,
    },
    Err {
        message: String,
    },
}

/// Read one length-prefixed JSON frame.
pub fn read_frame<R: std::io::Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {} out of bounds", len),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write one length-prefixed JSON frame.
pub fn write_frame<W: std::io::Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let req = Request::Exec {
            cmd: vec!["echo".into(), "hi".into()],
            env: HashMap::new(),
            cwd: Some("/work".into()),
            user: None,
            timeout_secs: Some(5),
        };
        let json = serde_json::to_vec(&req).unwrap();
        let mut buf = Vec::new();
        write_frame(&mut buf, &json).unwrap();
        let got = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(got, json);
        let parsed: Request = serde_json::from_slice(&got).unwrap();
        assert!(matches!(parsed, Request::Exec { .. }));
    }

    #[test]
    fn oversized_frame_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
        assert!(read_frame(&mut &buf[..]).is_err());
    }

    #[test]
    fn zero_frame_rejected() {
        let buf = 0u32.to_be_bytes();
        assert!(read_frame(&mut &buf[..]).is_err());
    }
}
