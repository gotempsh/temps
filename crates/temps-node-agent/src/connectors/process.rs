// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The process driver: a pinned binary as a supervised child process.
//!
//! Covers cloudflared and every connector that is "a program with a token"
//! (frpc, newt, rathole...). The binary is downloaded once per sha256 and
//! verified before it is ever executed; secrets reach it as 0600 files, never
//! on the command line.
//!
//! The control plane names the binary, but the agent decides whether to run
//! it: only artifacts in [`TRUSTED_ARTIFACTS`], compiled into this agent, are
//! ever downloaded or executed. A compromised or impersonated control plane
//! can choose among them, not hand the host an arbitrary program.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info};

use super::wire::{Artifact, DriverChoice, Readiness};
use super::{
    ConnectorDriver, ConnectorHealth, DriverKind, EdgeHandoff, ResolvedSpec, RunningConnector,
};

/// How long a connector may take to report ready before it counts as degraded.
const STARTUP_GRACE: Duration = Duration::from_secs(45);

/// A connector build this agent agrees to run.
struct TrustedArtifact {
    os: &'static str,
    arch: &'static str,
    url: &'static str,
    /// Of the download: the archive itself when `archive_entry` is set.
    sha256: &'static str,
    /// The file to run from inside a gzipped tarball download.
    archive_entry: Option<&'static str>,
}

/// Every connector binary this agent will run: the control plane's pins
/// (`temps-ingress/src/cloudflare.rs`), duplicated here because the agent
/// does not depend on control-plane crates. A test keeps the two in step;
/// moving the control plane to a new release means shipping an agent that
/// trusts it.
const TRUSTED_ARTIFACTS: &[TrustedArtifact] = &[
    TrustedArtifact {
        os: "linux",
        arch: "x86_64",
        url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-linux-amd64",
        sha256: "3bfc539f45d2620c71debb7a86be0efa737ed5ccbad9eb85c787116900e5c586",
        archive_entry: None,
    },
    TrustedArtifact {
        os: "linux",
        arch: "aarch64",
        url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-linux-arm64",
        sha256: "2c65dfabf616c40efc7545d107d7a57ad5e21377e2964b7e88af19a238074a46",
        archive_entry: None,
    },
    // cloudflared publishes macOS builds only as `.tgz` archives.
    TrustedArtifact {
        os: "macos",
        arch: "x86_64",
        url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-darwin-amd64.tgz",
        sha256: "3b0dbaf378337e9d7fb6cd235a76d1d6a7dee434c2d8edf9281164fb6a3837ab",
        archive_entry: Some("cloudflared"),
    },
    TrustedArtifact {
        os: "macos",
        arch: "aarch64",
        url: "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/cloudflared-darwin-arm64.tgz",
        sha256: "647615df99d49cd179ea5e13ef760b9bbfb33630f6ae62f87e33dfadda60adcc",
        archive_entry: Some("cloudflared"),
    },
];

/// Refuse any artifact not pinned in [`TRUSTED_ARTIFACTS`], matched on
/// platform, exact URL, checksum and archive entry together.
fn ensure_trusted(artifact: &Artifact) -> Result<(), String> {
    let sha256 = artifact.sha256.to_ascii_lowercase();
    let trusted = TRUSTED_ARTIFACTS.iter().any(|t| {
        t.os == artifact.os
            && t.arch == artifact.arch
            && t.url == artifact.url
            && t.sha256 == sha256
            && t.archive_entry == artifact.archive_entry.as_deref()
    });
    if trusted {
        Ok(())
    } else {
        Err(format!(
            "This agent does not trust connector artifact {} (sha256 {sha256}); upgrade the agent \
             (`temps upgrade` on the node) to one that ships with it",
            artifact.url
        ))
    }
}

/// A secret's file name: letters, digits, `_` and `-` only, so it can never
/// name a path outside the secrets directory.
fn valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub struct ProcessDriver {
    http: reqwest::Client,
}

impl ProcessDriver {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
        }
    }

    fn artifact_for(choice: &DriverChoice) -> Option<&Artifact> {
        let DriverChoice::Process(spec) = choice else {
            return None;
        };
        spec.artifacts
            .iter()
            .find(|a| a.os == std::env::consts::OS && a.arch == std::env::consts::ARCH)
    }

    /// The verified binary for `artifact`, downloading it on first use. An
    /// archive is verified the same way, then its entry is extracted afresh on
    /// every start, so what runs always comes from a checked download.
    async fn binary(&self, artifact: &Artifact, bin_dir: &Path) -> Result<PathBuf, String> {
        ensure_trusted(artifact)?;
        let download = self.download(artifact, bin_dir).await?;
        match artifact.archive_entry.as_deref() {
            None => Ok(download),
            Some(entry) => {
                let target =
                    bin_dir.join(format!("{}-{entry}", artifact.sha256.to_ascii_lowercase()));
                let entry = entry.to_string();
                let url = artifact.url.clone();
                tokio::task::spawn_blocking(move || {
                    extract_entry(&download, &entry, &target, &url).map(|()| target)
                })
                .await
                .map_err(|e| {
                    format!("extracting the connector from {} failed: {e}", artifact.url)
                })?
            }
        }
    }

    /// The file at `artifact.url`, verified against its pinned checksum and
    /// cached by it, downloading it on first use.
    async fn download(&self, artifact: &Artifact, bin_dir: &Path) -> Result<PathBuf, String> {
        let expected = artifact.sha256.to_ascii_lowercase();
        let path = bin_dir.join(&expected);
        if sha256_of_file(&path).await.ok().as_deref() == Some(expected.as_str()) {
            return Ok(path);
        }
        tokio::fs::create_dir_all(bin_dir)
            .await
            .map_err(|e| format!("could not create {}: {e}", bin_dir.display()))?;
        info!(url = %artifact.url, "downloading connector binary");
        let response = self
            .http
            .get(&artifact.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| {
                format!(
                    "could not download {}: {e}. The node needs outbound HTTPS to fetch the connector once.",
                    artifact.url
                )
            })?;
        if let Some(length) = response.content_length() {
            if length > MAX_CONNECTOR_BYTES {
                return Err(too_large(&artifact.url));
            }
        }
        // Streamed to disk and hashed as it arrives: a large body (an error
        // page served with 200, say) must not be buffered in memory first.
        let tmp = unique_part(&path);
        let actual = match download_to(response, &tmp, &artifact.url).await {
            Ok(actual) => actual,
            Err(error) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(error);
            }
        };
        if actual != expected {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(format!(
                "{} has sha256 {actual}, expected {expected}; refusing to run it",
                artifact.url
            ));
        }
        // An archive is only read, never run.
        let mode = if artifact.archive_entry.is_some() {
            0o644
        } else {
            0o755
        };
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|e| format!("could not set the mode of {}: {e}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| format!("could not install {}: {e}", path.display()))?;
        Ok(path)
    }
}

/// A temporary path beside `path` that no other write uses. Two connectors
/// starting at once with the same artifact each download and extract into
/// their own file, so neither truncates the other's, and only a complete,
/// checked file is renamed over `path`.
fn unique_part(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{}.{n}.part", std::process::id()));
    path.with_file_name(name)
}

/// Larger than any connector binary we pin (cloudflared is about 40 MiB);
/// a download past it is refused before its checksum can be checked.
const MAX_CONNECTOR_BYTES: u64 = 256 * 1024 * 1024;

fn too_large(url: &str) -> String {
    format!(
        "{url} is larger than {} MiB, which no connector binary is; refusing to download it. \
         Check that the node reaches the release host directly and not through a proxy that \
         rewrites the response.",
        MAX_CONNECTOR_BYTES / (1024 * 1024)
    )
}

/// Write `response` to `tmp` in chunks, returning the body's hex SHA-256.
async fn download_to(
    mut response: reqwest::Response,
    tmp: &Path,
    url: &str,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("download of {url} failed: {e}"))?
    {
        written += chunk.len() as u64;
        if written > MAX_CONNECTOR_BYTES {
            return Err(too_large(url));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    Ok(hex::encode(hasher.finalize()))
}

/// Install the regular file `entry` of the gzipped tarball `archive` at
/// `target`, executable. Only that exact name at the archive's root is taken
/// (`./` aside): no other path, link or directory, and nothing larger than
/// [`MAX_CONNECTOR_BYTES`]. Written beside `target` and renamed over it, so a
/// partial write never replaces a working binary.
fn extract_entry(archive: &Path, entry: &str, target: &Path, url: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_secret_name(entry) {
        return Err(format!("{url}: refusing archive entry name '{entry}'"));
    }
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("could not open {}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = tar
        .entries()
        .map_err(|e| format!("{url} is not a readable .tar.gz: {e}"))?;
    for item in entries {
        let mut item = item.map_err(|e| format!("{url} is not a readable .tar.gz: {e}"))?;
        let path = item
            .path()
            .map_err(|e| format!("{url} has an unreadable entry name: {e}"))?;
        let name = path.strip_prefix(".").unwrap_or(&path);
        if name != Path::new(entry) {
            continue;
        }
        if !item.header().entry_type().is_file() {
            return Err(format!(
                "{url}: '{entry}' is not a regular file in the archive; refusing to run it"
            ));
        }
        if item.size() > MAX_CONNECTOR_BYTES {
            return Err(too_large(url));
        }
        let tmp = unique_part(target);
        let written = (|| -> std::io::Result<()> {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o755)
                .open(&tmp)?;
            let mut limited = (&mut item).take(MAX_CONNECTOR_BYTES);
            std::io::copy(&mut limited, &mut out)?;
            out.flush()?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, target)
        })();
        return written.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!(
                "could not extract '{entry}' from {url} to {}: {e}",
                target.display()
            )
        });
    }
    Err(format!(
        "{url} has no '{entry}' at its root; refusing to guess which file to run"
    ))
}

/// Hex SHA-256 of a file, read in chunks.
async fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

impl Default for ProcessDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Fill `{edge_addr}`, `{metrics_addr}` and `{secret_file:NAME}`.
fn render(
    template: &str,
    edge: SocketAddr,
    metrics: SocketAddr,
    secret_dir: &Path,
) -> Result<String, String> {
    let mut out = template
        .replace("{edge_addr}", &edge.to_string())
        .replace("{metrics_addr}", &metrics.to_string());
    while let Some(start) = out.find("{secret_file:") {
        let end = out[start..]
            .find('}')
            .map(|i| start + i)
            .ok_or_else(|| format!("unterminated placeholder in '{template}'"))?;
        let name = &out[start + "{secret_file:".len()..end];
        if !valid_secret_name(name) {
            return Err(format!("invalid secret name '{name}'"));
        }
        let path = secret_dir.join(name).display().to_string();
        out.replace_range(start..=end, &path);
    }
    Ok(out)
}

/// Write a secret readable by its owner only, never briefly wider: created
/// 0600, and an existing file is narrowed before it is overwritten.
async fn write_secret(path: &Path, value: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    file.write_all(value).await?;
    file.flush().await
}

fn free_loopback_port() -> Result<SocketAddr, String> {
    StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("no free loopback port for connector metrics: {e}"))
}

#[async_trait]
impl ConnectorDriver for ProcessDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Process
    }

    fn accepts(&self, choice: &DriverChoice) -> bool {
        matches!(choice, DriverChoice::Process(_))
    }

    fn available(&self, choice: &DriverChoice) -> Result<(), String> {
        match Self::artifact_for(choice) {
            Some(artifact) => ensure_trusted(artifact),
            None => Err(format!(
                "no build for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )),
        }
    }

    async fn start(
        &self,
        spec: &ResolvedSpec,
        edge: &EdgeHandoff,
    ) -> Result<Box<dyn RunningConnector>, String> {
        let DriverChoice::Process(process) = &spec.choice else {
            return Err("not a process spec".into());
        };
        let EdgeHandoff::Loopback {
            addr: edge_addr, ..
        } = edge
        else {
            return Err("the process driver forwards to a loopback listener".into());
        };
        // Checked before anything touches the disk: a name like `../x` or
        // `/etc/cron.d/x` would otherwise write outside the secrets directory.
        if let Some(bad) = spec.secrets.keys().find(|name| !valid_secret_name(name)) {
            return Err(format!(
                "refusing connector spec: secret name '{bad}' may only use letters, digits, '_' and '-'"
            ));
        }
        let artifact = Self::artifact_for(&spec.choice).ok_or("no artifact for this platform")?;
        let binary = self.binary(artifact, &spec.bin_dir).await?;

        // Secrets as 0600 files in a 0700 directory.
        let secret_dir = spec.work_dir.join("secrets");
        tokio::fs::create_dir_all(&secret_dir)
            .await
            .map_err(|e| format!("could not create {}: {e}", secret_dir.display()))?;
        for dir in [&spec.work_dir, &secret_dir] {
            tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|e| format!("could not lock down {}: {e}", dir.display()))?;
        }
        for (name, value) in &spec.secrets {
            write_secret(&secret_dir.join(name), value.as_bytes())
                .await
                .map_err(|e| format!("could not write secret {name}: {e}"))?;
        }

        let metrics = free_loopback_port()?;
        let args = process
            .args
            .iter()
            .map(|a| render(a, *edge_addr, metrics, &secret_dir))
            .collect::<Result<Vec<_>, _>>()?;
        let mut command = Command::new(&binary);
        command
            .args(&args)
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", &spec.work_dir)
            .current_dir(&spec.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &process.env {
            command.env(key, render(value, *edge_addr, metrics, &secret_dir)?);
        }
        // Tied to the agent: the unit's `KillMode=process` leaves the agent's
        // children running when it stops, so the kernel must end them.
        let mut child = super::child::spawn_tied_to_agent(command)
            .await
            .map_err(|e| format!("could not start {}: {e}", binary.display()))?;

        let log_tail = Arc::new(Mutex::new(VecDeque::with_capacity(20)));
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let tail = Arc::clone(&log_tail);
            let connector_id = spec.connector_id;
            tokio::spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!(connector_id, "{line}");
                    let mut tail = tail.lock();
                    if tail.len() == 20 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }

        let readiness = match &process.readiness {
            Readiness::HttpReady { url } => Readiness::HttpReady {
                url: render(url, *edge_addr, metrics, &secret_dir)?,
            },
            Readiness::TcpConnect { address } => Readiness::TcpConnect {
                address: render(address, *edge_addr, metrics, &secret_dir)?,
            },
            Readiness::Running => Readiness::Running,
        };
        Ok(Box::new(RunningProcess {
            child,
            readiness,
            started: Instant::now(),
            log_tail,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .no_proxy()
                .build()
                .unwrap_or_default(),
        }))
    }
}

struct RunningProcess {
    child: Child,
    readiness: Readiness,
    started: Instant,
    log_tail: Arc<Mutex<VecDeque<String>>>,
    http: reqwest::Client,
}

impl RunningProcess {
    /// The most recent log line that looks like an error, else the last one.
    fn last_log(&self) -> Option<String> {
        let tail = self.log_tail.lock();
        tail.iter()
            .rev()
            .find(|l| l.contains("ERR") || l.to_ascii_lowercase().contains("error"))
            .or_else(|| tail.back())
            .cloned()
    }

    fn not_ready(&self, reason: String) -> ConnectorHealth {
        if self.started.elapsed() < STARTUP_GRACE {
            ConnectorHealth::Starting
        } else {
            let detail = self
                .last_log()
                .map(|l| format!(" Last log: {l}"))
                .unwrap_or_default();
            ConnectorHealth::Degraded(format!("{reason}.{detail}"))
        }
    }
}

#[async_trait]
impl RunningConnector for RunningProcess {
    async fn health(&mut self) -> ConnectorHealth {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let detail = self
                    .last_log()
                    .map(|l| format!(" Last log: {l}"))
                    .unwrap_or_default();
                return ConnectorHealth::Exited(format!("exited with {status}.{detail}"));
            }
            Ok(None) => {}
            Err(error) => return ConnectorHealth::Exited(format!("lost the process: {error}")),
        }
        match &self.readiness {
            Readiness::Running => ConnectorHealth::Ready {
                connections: None,
                connector_ref: None,
            },
            Readiness::TcpConnect { address } => {
                match tokio::net::TcpStream::connect(address.as_str()).await {
                    Ok(_) => ConnectorHealth::Ready {
                        connections: None,
                        connector_ref: None,
                    },
                    Err(e) => self.not_ready(format!("{address} refuses connections ({e})")),
                }
            }
            Readiness::HttpReady { url } => match self.http.get(url).send().await {
                Ok(response) if response.status().is_success() => {
                    let body: serde_json::Value = response.json().await.unwrap_or_default();
                    ConnectorHealth::Ready {
                        connections: body
                            .get("readyConnections")
                            .and_then(|v| v.as_i64())
                            .map(|v| v as i32),
                        connector_ref: body
                            .get("connectorId")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    }
                }
                Ok(response) => self.not_ready(format!(
                    "not connected to the provider yet (readiness answered {})",
                    response.status()
                )),
                Err(e) => self.not_ready(format!("readiness check failed ({e})")),
            },
        }
    }

    async fn stop(mut self: Box<Self>, grace: Duration) {
        if let Some(pid) = self.child.id() {
            // SAFETY: plain signal to our own child.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(grace, self.child.wait()).await.is_ok() {
                return;
            }
        }
        let _ = self.child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloudflared(arch: &str, url: &str, sha256: &str) -> Artifact {
        Artifact {
            os: "linux".into(),
            arch: arch.into(),
            url: url.into(),
            sha256: sha256.into(),
            archive_entry: None,
        }
    }

    fn cloudflared_archive(arch: &str, url: &str, sha256: &str, entry: Option<&str>) -> Artifact {
        Artifact {
            os: "macos".into(),
            arch: arch.into(),
            url: url.into(),
            sha256: sha256.into(),
            archive_entry: entry.map(String::from),
        }
    }

    /// A gzipped tarball of `(path, contents, entry type)`.
    fn tgz(dir: &Path, entries: &[(&str, &[u8], tar::EntryType)]) -> PathBuf {
        let path = dir.join("archive.tgz");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::fast(),
        ));
        for (name, contents, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_path(name).unwrap();
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn an_archive_is_trusted_only_with_its_pinned_entry() {
        let pinned = TRUSTED_ARTIFACTS
            .iter()
            .find(|t| t.archive_entry.is_some())
            .expect("a pinned archive");
        let artifact = |entry| cloudflared_archive(pinned.arch, pinned.url, pinned.sha256, entry);
        let mut exact = artifact(pinned.archive_entry);
        exact.os = pinned.os.into();
        assert!(ensure_trusted(&exact).is_ok());
        for other in [None, Some("cloudflared-other")] {
            let mut wrong = artifact(other);
            wrong.os = pinned.os.into();
            assert!(ensure_trusted(&wrong).is_err(), "entry {other:?} trusted");
        }
        let mut on_linux = artifact(pinned.archive_entry);
        on_linux.os = "linux".into();
        assert!(ensure_trusted(&on_linux).is_err());
    }

    #[test]
    fn extracts_only_the_named_root_file_as_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tgz(
            dir.path(),
            &[
                ("README", b"docs", tar::EntryType::Regular),
                ("nested/cloudflared", b"wrong one", tar::EntryType::Regular),
                ("./cloudflared", b"the binary", tar::EntryType::Regular),
            ],
        );
        let target = dir.path().join("abc-cloudflared");
        std::fs::write(&target, b"old binary").unwrap();
        extract_entry(&archive, "cloudflared", &target, "https://x/a.tgz").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"the binary");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(part_files(dir.path()), Vec::<String>::new());
    }

    fn part_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect()
    }

    #[test]
    fn every_write_gets_its_own_temporary_file() {
        let target = Path::new("/bin-dir/abc-cloudflared");
        let (a, b) = (unique_part(target), unique_part(target));
        assert_ne!(a, b);
        for part in [&a, &b] {
            assert_eq!(part.parent(), target.parent());
            let name = part.file_name().unwrap().to_string_lossy();
            assert!(name.starts_with("abc-cloudflared."), "{name}");
            assert!(name.ends_with(".part"), "{name}");
        }
    }

    #[test]
    fn concurrent_extractions_of_one_artifact_each_install_a_complete_binary() {
        let dir = tempfile::tempdir().unwrap();
        // Large enough that the copies overlap.
        let binary = vec![7u8; 8 * 1024 * 1024];
        let archive = tgz(
            dir.path(),
            &[("cloudflared", binary.as_slice(), tar::EntryType::Regular)],
        );
        let target = dir.path().join("abc-cloudflared");
        let results: Vec<_> = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        extract_entry(&archive, "cloudflared", &target, "https://x/a.tgz")
                    })
                })
                .collect();
            starts.into_iter().map(|s| s.join().unwrap()).collect()
        });
        for result in results {
            assert_eq!(result, Ok(()));
        }
        assert_eq!(std::fs::read(&target).unwrap(), binary);
        assert_eq!(part_files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn refuses_a_missing_or_non_file_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("abc-cloudflared");

        let archive = tgz(
            dir.path(),
            &[("nested/cloudflared", b"x", tar::EntryType::Regular)],
        );
        let missing =
            extract_entry(&archive, "cloudflared", &target, "https://x/a.tgz").unwrap_err();
        assert!(
            missing.contains("has no 'cloudflared' at its root"),
            "{missing}"
        );

        let archive = tgz(
            dir.path(),
            &[("cloudflared", b"", tar::EntryType::Directory)],
        );
        let not_file =
            extract_entry(&archive, "cloudflared", &target, "https://x/a.tgz").unwrap_err();
        assert!(not_file.contains("is not a regular file"), "{not_file}");

        let bad_name =
            extract_entry(&archive, "../cloudflared", &target, "https://x/a.tgz").unwrap_err();
        assert!(
            bad_name.contains("refusing archive entry name"),
            "{bad_name}"
        );
        assert!(!target.exists());

        std::fs::write(dir.path().join("archive.tgz"), b"not gzip").unwrap();
        let corrupt = extract_entry(
            &dir.path().join("archive.tgz"),
            "cloudflared",
            &target,
            "https://x/a.tgz",
        )
        .unwrap_err();
        assert!(corrupt.contains("https://x/a.tgz"), "{corrupt}");
    }

    #[test]
    fn only_pinned_artifacts_are_trusted() {
        let pinned = &TRUSTED_ARTIFACTS[0];
        assert!(ensure_trusted(&cloudflared(pinned.arch, pinned.url, pinned.sha256)).is_ok());
        assert!(
            ensure_trusted(&cloudflared(
                pinned.arch,
                pinned.url,
                &pinned.sha256.to_ascii_uppercase()
            ))
            .is_ok(),
            "checksums compare case-insensitively"
        );

        let elsewhere = ensure_trusted(&cloudflared(
            pinned.arch,
            "https://evil.example/cloudflared",
            pinned.sha256,
        ))
        .unwrap_err();
        assert!(
            elsewhere.contains("does not trust connector artifact"),
            "{elsewhere}"
        );
        assert!(elsewhere.contains("upgrade the agent"), "{elsewhere}");
        let plain_http = pinned.url.replacen("https://", "http://", 1);
        assert!(ensure_trusted(&cloudflared(pinned.arch, &plain_http, pinned.sha256)).is_err());
        assert!(ensure_trusted(&cloudflared(pinned.arch, pinned.url, &"0".repeat(64))).is_err());
        let other_arch = if pinned.arch == "x86_64" {
            "aarch64"
        } else {
            "x86_64"
        };
        assert!(ensure_trusted(&cloudflared(other_arch, pinned.url, pinned.sha256)).is_err());
    }

    #[test]
    fn every_trusted_artifact_is_https() {
        for artifact in TRUSTED_ARTIFACTS {
            assert!(artifact.url.starts_with("https://"), "{}", artifact.url);
        }
    }

    /// The control plane hands out what it pins in `temps-ingress`; the agent
    /// must trust exactly those, or delegated workers refuse every tunnel.
    #[test]
    fn trusted_artifacts_match_the_control_plane_pins() {
        let source = include_str!("../../../temps-ingress/src/cloudflare.rs");
        let constant = |name: &str| -> String {
            let at = source
                .find(&format!("const {name}: &str ="))
                .unwrap_or_else(|| panic!("{name} missing"));
            let rest = &source[at..];
            let open = rest.find('"').unwrap() + 1;
            let close = open + rest[open..].find('"').unwrap();
            rest[open..close].to_string()
        };
        let version = constant("CLOUDFLARED_VERSION");
        let release =
            format!("https://github.com/cloudflare/cloudflared/releases/download/{version}");
        for (os, arch, file, sha, entry) in [
            (
                "linux",
                "x86_64",
                "cloudflared-linux-amd64",
                constant("CLOUDFLARED_AMD64_SHA256"),
                None,
            ),
            (
                "linux",
                "aarch64",
                "cloudflared-linux-arm64",
                constant("CLOUDFLARED_ARM64_SHA256"),
                None,
            ),
            (
                "macos",
                "x86_64",
                "cloudflared-darwin-amd64.tgz",
                constant("CLOUDFLARED_DARWIN_AMD64_SHA256"),
                Some("cloudflared"),
            ),
            (
                "macos",
                "aarch64",
                "cloudflared-darwin-arm64.tgz",
                constant("CLOUDFLARED_DARWIN_ARM64_SHA256"),
                Some("cloudflared"),
            ),
        ] {
            let url = format!("{release}/{file}");
            let mut artifact = cloudflared_archive(arch, &url, &sha, entry);
            artifact.os = os.into();
            assert!(
                ensure_trusted(&artifact).is_ok(),
                "the control plane pins {url} ({sha}) but this agent does not trust it"
            );
        }
        assert_eq!(
            TRUSTED_ARTIFACTS.len(),
            4,
            "every trusted artifact is checked against a control-plane pin above"
        );
    }

    #[test]
    fn secret_names_cannot_leave_the_secret_dir() {
        assert!(valid_secret_name("token"));
        assert!(valid_secret_name("TUNNEL_token-2"));
        for bad in [
            "",
            "../x",
            "/etc/cron.d/x",
            "a/b",
            "..",
            "a.b",
            "tok en",
            "x\0",
        ] {
            assert!(!valid_secret_name(bad), "{bad:?} accepted");
        }
    }

    #[tokio::test]
    async fn secrets_are_written_owner_only() {
        let dir = std::env::temp_dir().join(format!("temps-secret-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("token");
        tokio::fs::write(&path, b"old").await.unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .await
            .unwrap();
        write_secret(&path, b"new").await.unwrap();
        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"new");
        let fresh = dir.join("fresh");
        write_secret(&fresh, b"x").await.unwrap();
        assert_eq!(
            tokio::fs::metadata(&fresh)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
