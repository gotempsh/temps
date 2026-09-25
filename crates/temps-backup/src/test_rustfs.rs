// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared RustFS (S3-compatible server) fixture for this crate's docker-backed
//! tests. RustFS replaced MinIO here because MinIO withdrew its public images
//! (`quay.io/minio/*` and Docker Hub `minio/*` no longer pull).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Image repository and pinned tag, kept separate for bollard's
/// `CreateImageOptions` (`from_image` + `tag`) and testcontainers'
/// `GenericImage::new(name, tag)`.
pub(crate) const RUSTFS_IMAGE: &str = "rustfs/rustfs";
pub(crate) const RUSTFS_TAG: &str = "1.0.0-rc.5";
/// RustFS's documented default root credential, set explicitly so the test
/// does not depend on the image default.
pub(crate) const RUSTFS_ACCESS_KEY: &str = "rustfsadmin";
pub(crate) const RUSTFS_SECRET_KEY: &str = "rustfsadmin";
/// S3 API port inside the container (console is 9001).
pub(crate) const RUSTFS_S3_PORT: u16 = 9000;

/// Container env for a RustFS server. The image's default entrypoint/cmd
/// already serves `/data` (`RUSTFS_VOLUMES=/data`), so no cmd is needed.
pub(crate) fn rustfs_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("RUSTFS_ACCESS_KEY", RUSTFS_ACCESS_KEY),
        ("RUSTFS_SECRET_KEY", RUSTFS_SECRET_KEY),
    ]
}

/// A testcontainers request for a RustFS server with the fixture credentials.
pub(crate) fn rustfs_container_request(
) -> testcontainers::core::ContainerRequest<testcontainers::GenericImage> {
    use testcontainers::{GenericImage, ImageExt};
    rustfs_env().into_iter().fold(
        GenericImage::new(RUSTFS_IMAGE, RUSTFS_TAG).into(),
        |req: testcontainers::core::ContainerRequest<GenericImage>, (k, v)| req.with_env_var(k, v),
    )
}

/// Poll RustFS's unauthenticated `GET /health` on `127.0.0.1:<host_port>`
/// until it returns 200. RustFS prints nothing on stdout/stderr once it is
/// ready (its log level defaults to `warn` and goes to `/logs`), so a
/// log-message wait strategy has nothing to match; polling the health
/// endpoint replaces the fixed sleeps the MinIO fixture used.
pub(crate) async fn wait_for_rustfs_ready(host_port: u16) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut last_err = String::from("never attempted");
    while tokio::time::Instant::now() < deadline {
        match probe_health(host_port).await {
            Ok(true) => return Ok(()),
            Ok(false) => last_err = "health endpoint returned non-200".to_string(),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "RustFS on 127.0.0.1:{host_port} not healthy after 60s: {last_err}"
    ))
}

async fn probe_health(host_port: u16) -> std::io::Result<bool> {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", host_port)),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
    stream
        .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut buf = Vec::with_capacity(256);
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timed out"))??;
    let status_line = buf.split(|b| *b == b'\n').next().unwrap_or_default();
    let status_line = String::from_utf8_lossy(status_line);
    Ok(status_line.split_whitespace().nth(1) == Some("200"))
}
