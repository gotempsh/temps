// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A small root service that runs each Firecracker VMM through the jailer.
//!
//! Without it the server starts `firecracker` as itself, so a VMM escape
//! lands as the server user, which can read the database credentials and the
//! encryption key. With it, the server asks this service (over a Unix socket
//! only the server user can open) to launch a VM by name. The service then:
//!
//! 1. re-validates the VM's config, which the server wrote and so is not
//!    trusted ([`plan`]),
//! 2. builds an empty chroot holding only that VM's kernel, disk and
//!    rewritten config,
//! 3. gives the TAP device to a per-VM uid, and
//! 4. runs `jailer`, which drops the VMM to that uid inside the chroot.
//!
//! The VMM then sees none of the server's files and cannot signal the
//! server. [`server`] is the privileged half; [`LauncherClient`] is what the
//! provider uses. The server falls back to its direct launch when no
//! launcher is configured (see `temps firecracker setup --jailer`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub mod plan;
pub mod server;

/// Largest request or response frame, so a confused peer cannot make either
/// side allocate without bound.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024;

/// How long the server waits for the launcher to answer one request. A
/// launch includes preparing a chroot and starting the jailer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// What the launcher is configured with, written by
/// `temps firecracker setup --jailer` to a root-owned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherConfig {
    /// `<data dir>/firecracker`, where `vms/<name>` hold the server's VM state.
    pub fc_root: PathBuf,
    /// Where the jailer builds its chroots. Must be on the same filesystem as
    /// `fc_root` (VM disks are hard-linked into it, never copied) and should
    /// be root-owned with root-owned ancestors, since the launcher deletes
    /// and populates it as root.
    pub jail_base: PathBuf,
    pub firecracker_bin: PathBuf,
    pub jailer_bin: PathBuf,
    /// Directory holding the pinned `vmlinux-*` guest kernel.
    pub kernel_dir: PathBuf,
    /// The only identity allowed to talk to the launcher.
    pub server_uid: u32,
    pub server_gid: u32,
    /// Per-VM identities are `uid_base + slot` (and the same gid).
    pub uid_base: u32,
    pub uid_count: u32,
    /// The bridge TAP devices attach to.
    pub bridge: String,
    pub socket: PathBuf,
}

impl LauncherConfig {
    pub fn vms_dir(&self) -> PathBuf {
        self.fc_root.join("vms")
    }
}

/// Where the jailer builds its chroots unless configured otherwise: under the
/// data directory, so a VM disk can be hard-linked into them.
pub fn default_jail_base(fc_root: &Path) -> PathBuf {
    fc_root.join("jail")
}

/// The jailer accepts ids of letters, digits and hyphens, at most 64. A VM
/// name may hold more (underscores, dots), so the id is the name made safe
/// plus a hash of the original, which keeps two names that sanitize alike
/// apart.
pub fn jail_id(vm: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in vm.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let safe: String = vm
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(47)
        .collect();
    format!("{safe}-{:016x}", hash)
}

/// The directory the VMM sees as `/` (the jailer's `<base>/<exec>/<id>/root`).
pub fn jail_root(jail_base: &Path, vm: &str) -> PathBuf {
    jail_base.join("firecracker").join(jail_id(vm)).join("root")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LauncherRequest {
    Ping,
    /// Start the VM `vm` from `vms/<vm>`.
    Launch {
        vm: String,
    },
    /// Kill the VM's VMM process, if it runs.
    Kill {
        vm: String,
    },
    /// Kill it and delete its jail; give its TAP back to the server user.
    Remove {
        vm: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum LauncherResponse {
    Pong,
    Launched { pid: u32 },
    Killed { was_running: bool },
    Removed,
    Err { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("launcher at '{socket}': {operation}: {reason}")]
    Io {
        socket: PathBuf,
        operation: &'static str,
        reason: String,
    },
    #[error("launcher at '{socket}' timed out after {timeout_secs}s on {operation}")]
    Timeout {
        socket: PathBuf,
        operation: &'static str,
        timeout_secs: u64,
    },
    #[error("launcher at '{socket}' refused {operation} of VM '{vm}': {message}")]
    Refused {
        socket: PathBuf,
        operation: &'static str,
        vm: String,
        message: String,
    },
    #[error("launcher at '{socket}' sent a frame of {len} bytes (limit {MAX_FRAME_BYTES})")]
    FrameTooLarge { socket: PathBuf, len: u32 },
    #[error("launcher at '{socket}' sent an unexpected answer to {operation}: {answer}")]
    Unexpected {
        socket: PathBuf,
        operation: &'static str,
        answer: String,
    },
}

/// What the provider uses to ask the launcher for things.
#[derive(Debug, Clone)]
pub struct LauncherClient {
    socket: PathBuf,
}

impl LauncherClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn call(
        &self,
        operation: &'static str,
        request: &LauncherRequest,
    ) -> Result<LauncherResponse, LauncherError> {
        let io = |reason: String| LauncherError::Io {
            socket: self.socket.clone(),
            operation,
            reason,
        };
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket)
                .await
                .map_err(|e| io(format!("connect: {e}")))?;
            write_frame(&mut stream, request)
                .await
                .map_err(|e| io(format!("send: {e}")))?;
            match read_frame::<LauncherResponse>(&mut stream).await {
                Ok(response) => Ok(response),
                Err(FrameError::TooLarge(len)) => Err(LauncherError::FrameTooLarge {
                    socket: self.socket.clone(),
                    len,
                }),
                Err(FrameError::Io(e)) => Err(io(format!("receive: {e}"))),
            }
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, exchange).await {
            Ok(result) => result,
            Err(_) => Err(LauncherError::Timeout {
                socket: self.socket.clone(),
                operation,
                timeout_secs: REQUEST_TIMEOUT.as_secs(),
            }),
        }
    }

    fn refused(&self, operation: &'static str, vm: &str, message: String) -> LauncherError {
        LauncherError::Refused {
            socket: self.socket.clone(),
            operation,
            vm: vm.to_string(),
            message,
        }
    }

    fn unexpected(&self, operation: &'static str, answer: LauncherResponse) -> LauncherError {
        LauncherError::Unexpected {
            socket: self.socket.clone(),
            operation,
            answer: format!("{answer:?}"),
        }
    }

    pub async fn ping(&self) -> Result<(), LauncherError> {
        match self.call("ping", &LauncherRequest::Ping).await? {
            LauncherResponse::Pong => Ok(()),
            other => Err(self.unexpected("ping", other)),
        }
    }

    /// Start the VM; returns the VMM's pid.
    pub async fn launch(&self, vm: &str) -> Result<u32, LauncherError> {
        match self
            .call("launch", &LauncherRequest::Launch { vm: vm.to_string() })
            .await?
        {
            LauncherResponse::Launched { pid } => Ok(pid),
            LauncherResponse::Err { message } => Err(self.refused("launch", vm, message)),
            other => Err(self.unexpected("launch", other)),
        }
    }

    /// Kill the VMM; whether one was running.
    pub async fn kill(&self, vm: &str) -> Result<bool, LauncherError> {
        match self
            .call("kill", &LauncherRequest::Kill { vm: vm.to_string() })
            .await?
        {
            LauncherResponse::Killed { was_running } => Ok(was_running),
            LauncherResponse::Err { message } => Err(self.refused("kill", vm, message)),
            other => Err(self.unexpected("kill", other)),
        }
    }

    pub async fn remove(&self, vm: &str) -> Result<(), LauncherError> {
        match self
            .call("remove", &LauncherRequest::Remove { vm: vm.to_string() })
            .await?
        {
            LauncherResponse::Removed => Ok(()),
            LauncherResponse::Err { message } => Err(self.refused("remove", vm, message)),
            other => Err(self.unexpected("remove", other)),
        }
    }
}

#[derive(Debug)]
pub(crate) enum FrameError {
    TooLarge(u32),
    Io(std::io::Error),
}

impl From<std::io::Error> for FrameError {
    fn from(e: std::io::Error) -> Self {
        FrameError::Io(e)
    }
}

/// One JSON value behind a big-endian `u32` length.
pub(crate) async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

pub(crate) async fn read_frame<T>(reader: &mut (impl AsyncReadExt + Unpin)) -> Result<T, FrameError>
where
    T: for<'de> Deserialize<'de>,
{
    let len = reader.read_u32().await?;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| FrameError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jail_ids_are_valid_for_the_jailer_and_distinguish_similar_names() {
        let a = jail_id("temps-sandbox-wl_app.1");
        let b = jail_id("temps-sandbox-wl-app-1");
        assert_ne!(a, b, "names that sanitize alike stay apart");
        for id in [&a, &b, &jail_id(&"x".repeat(200))] {
            assert!(id.len() <= 64, "{id}");
            assert!(
                id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{id}"
            );
        }
        assert_eq!(a, jail_id("temps-sandbox-wl_app.1"), "stable");
    }

    #[test]
    fn the_jail_root_is_where_the_jailer_puts_it() {
        let root = jail_root(Path::new("/data/firecracker/jail"), "vm1");
        let id = jail_id("vm1");
        assert_eq!(
            root,
            PathBuf::from(format!("/data/firecracker/jail/firecracker/{id}/root"))
        );
        assert_eq!(
            default_jail_base(Path::new("/data/firecracker")),
            PathBuf::from("/data/firecracker/jail")
        );
    }

    #[test]
    fn requests_and_answers_have_a_stable_wire_form() {
        assert_eq!(
            serde_json::to_string(&LauncherRequest::Launch { vm: "a".into() }).unwrap(),
            r#"{"op":"launch","vm":"a"}"#
        );
        assert_eq!(
            serde_json::to_string(&LauncherResponse::Launched { pid: 7 }).unwrap(),
            r#"{"result":"launched","pid":7}"#
        );
        let err: LauncherResponse =
            serde_json::from_str(r#"{"result":"err","message":"no"}"#).unwrap();
        assert_eq!(
            err,
            LauncherResponse::Err {
                message: "no".into()
            }
        );
    }

    #[tokio::test]
    async fn a_frame_round_trips_and_an_oversized_one_is_refused() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &LauncherRequest::Ping).await.unwrap();
        assert_eq!(
            read_frame::<LauncherRequest>(&mut b).await.unwrap(),
            LauncherRequest::Ping
        );
        a.write_all(&(MAX_FRAME_BYTES + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_frame::<LauncherRequest>(&mut b).await,
            Err(FrameError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn the_client_reports_an_unreachable_launcher_with_its_socket() {
        let client = LauncherClient::new(PathBuf::from("/nonexistent/launcher.sock"));
        let error = client.launch("vm1").await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("/nonexistent/launcher.sock"), "{message}");
        assert!(message.contains("launch"), "{message}");
    }
}
