// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeps the worker's Shield snapshot and keys current (ADR 066, D10).
//!
//! The route snapshot names the control plane's newest Shield revision
//! ([`ShieldRef`]). This task fetches
//! `GET /api/internal/nodes/{id}/shield/snapshot` at startup and whenever
//! that revision differs from the loaded one, decodes it off the request
//! threads, swaps it in, and keeps the response in the data dir so a
//! restart without the control plane starts with the same lists and keys.
//! A 404 is a control plane without Shield on workers: nothing is fetched
//! again until the route snapshot names a revision.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use temps_core::worker_shield::{ShieldRef, ShieldSnapshotResponse};
use temps_shield::{Index, ShieldKeys};
use thiserror::Error;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use super::uploader::{EventUploader, UploaderSettings};
use super::{decode_snapshot, open_keys, ShieldLoadError, WorkerShield};

/// The largest snapshot response read, from the network or from disk.
pub const MAX_SNAPSHOT_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Where and as whom the worker reaches the control plane.
#[derive(Clone)]
pub struct ShieldSyncConfig {
    /// Base URL, no trailing slash.
    pub control_plane_url: String,
    pub node_id: i32,
    pub node_token: String,
    /// Built with the control plane's trust ([`crate::with_control_plane_trust`]).
    pub http: reqwest::Client,
    /// The node's public-ingress private key (base64), which the keys are
    /// sealed to. Without it the lists load but no challenge is issued.
    pub private_key_b64: Option<String>,
    pub uploader: UploaderSettings,
}

#[derive(Debug, Error)]
pub enum ShieldFetchError {
    #[error("GET {url}: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GET {url} answered {status}")]
    Status { url: String, status: StatusCode },
    #[error("GET {url}: the response is larger than {limit} bytes")]
    TooLarge { url: String, limit: usize },
    #[error("GET {url}: the response is not a Shield snapshot: {reason}")]
    Malformed { url: String, reason: String },
    #[error(transparent)]
    Load(#[from] ShieldLoadError),
    #[error("decoding the Shield snapshot stopped: {0}")]
    Join(String),
}

#[derive(Debug, Error)]
pub enum ShieldPersistError {
    #[error("cannot write the Shield snapshot to {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot encode the Shield snapshot: {0}")]
    Encode(#[from] serde_json::Error),
}

/// What [`fetch_once`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// A new snapshot is loaded.
    Installed(u64),
    /// The loaded one is the newest.
    Unchanged,
    /// The control plane does not run Shield on workers.
    Unsupported,
}

/// Start the snapshot sync and the event uploader for `shield`.
pub fn start(
    config: ShieldSyncConfig,
    shield: Arc<WorkerShield>,
    shutdown: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    let uploader = EventUploader::spawn(
        config.http.clone(),
        format!(
            "{}/api/internal/nodes/{}/shield/events",
            config.control_plane_url, config.node_id
        ),
        config.node_token.clone(),
        config.uploader.clone(),
        Arc::clone(&shutdown),
    );
    shield.set_sink(uploader);
    tokio::spawn(run(config, shield, shutdown))
}

async fn run(config: ShieldSyncConfig, shield: Arc<WorkerShield>, shutdown: Arc<Notify>) {
    let mut changes = shield.subscribe();
    match load_from_disk(&shield, config.private_key_b64.as_deref()).await {
        Ok(Some(revision)) => info!(revision, "loaded the Shield snapshot from disk"),
        Ok(None) => {}
        Err(error) => warn!(%error, "ignoring the Shield snapshot on disk"),
    }
    let mut backoff = INITIAL_BACKOFF;
    loop {
        // Mark what this round fetches for as seen, so a change while it
        // runs starts another.
        let wanted = *changes.borrow_and_update();
        let wait = match fetch_once(&config, &shield, wanted).await {
            Ok(fetched) => {
                backoff = INITIAL_BACKOFF;
                match fetched {
                    Fetched::Installed(revision) => {
                        info!(revision, "loaded the Shield snapshot")
                    }
                    Fetched::Unchanged => debug!("the Shield snapshot is current"),
                    Fetched::Unsupported => {
                        debug!("the control plane does not run Shield on workers")
                    }
                }
                None
            }
            Err(error) => {
                warn!(%error, retry_in = ?backoff, "could not load the Shield snapshot");
                let wait = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                Some(wait)
            }
        };
        let retry = async {
            match wait {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = shutdown.notified() => return,
            _ = retry => {}
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// One fetch for `wanted`, the revision the route snapshot names.
pub async fn fetch_once(
    config: &ShieldSyncConfig,
    shield: &WorkerShield,
    wanted: Option<ShieldRef>,
) -> Result<Fetched, ShieldFetchError> {
    let loaded = shield.revision();
    if wanted.is_some_and(|wanted| wanted.revision == loaded) && shield.has_engine() {
        return Ok(Fetched::Unchanged);
    }
    // Without an engine the keys are wanted too, which revision 0 asks for.
    let revision = if shield.has_engine() { loaded } else { 0 };
    let url = format!(
        "{}/api/internal/nodes/{}/shield/snapshot?revision={revision}",
        config.control_plane_url, config.node_id
    );
    let request_error = |source| ShieldFetchError::Request {
        url: url.clone(),
        source,
    };
    let mut response = config
        .http
        .get(&url)
        .bearer_auth(&config.node_token)
        .header(
            temps_core::tailnet::AGENT_CAPABILITIES_HEADER,
            crate::route_sync_client::declared_capabilities(),
        )
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(request_error)?;
    match response.status() {
        StatusCode::NOT_MODIFIED => return Ok(Fetched::Unchanged),
        StatusCode::NOT_FOUND => return Ok(Fetched::Unsupported),
        status if !status.is_success() => {
            return Err(ShieldFetchError::Status { url, status });
        }
        _ => {}
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SNAPSHOT_RESPONSE_BYTES as u64)
    {
        return Err(ShieldFetchError::TooLarge {
            url,
            limit: MAX_SNAPSHOT_RESPONSE_BYTES,
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if body.len() + chunk.len() > MAX_SNAPSHOT_RESPONSE_BYTES {
            return Err(ShieldFetchError::TooLarge {
                url,
                limit: MAX_SNAPSHOT_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    let snapshot: ShieldSnapshotResponse =
        serde_json::from_slice(&body).map_err(|error| ShieldFetchError::Malformed {
            url: url.clone(),
            reason: error.to_string(),
        })?;
    let revision = snapshot.revision;
    let snapshot = Arc::new(snapshot);
    let keys = install(
        shield,
        Arc::clone(&snapshot),
        config.private_key_b64.clone(),
    )
    .await?;
    let path = shield.path().to_path_buf();
    match tokio::task::spawn_blocking(move || persist(&path, &snapshot)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(%error, "the Shield snapshot is loaded but not kept on disk"),
        Err(error) => warn!(%error, "the Shield snapshot is loaded but not kept on disk"),
    }
    if let Err(error) = keys {
        warn!(%error, "Shield lists are loaded, but challenges cannot be issued");
    }
    Ok(Fetched::Installed(revision))
}

/// The keys' result is separate: lists that load are used even when the
/// keys do not open.
type KeysResult = Result<(), ShieldLoadError>;

/// Decode `response` and swap it in. Decoding and indexing run on the
/// blocking pool; a failure keeps what was loaded before.
async fn install(
    shield: &WorkerShield,
    response: Arc<ShieldSnapshotResponse>,
    private_key_b64: Option<String>,
) -> Result<KeysResult, ShieldFetchError> {
    let open = !shield.has_engine();
    let (revision, index, keys) =
        tokio::task::spawn_blocking(move || prepare(&response, private_key_b64.as_deref(), open))
            .await
            .map_err(|error| ShieldFetchError::Join(error.to_string()))??;
    let (keys, result) = match keys {
        Ok(keys) => (keys, Ok(())),
        Err(error) => (None, Err(error)),
    };
    shield.install(revision, index, keys);
    Ok(result)
}

type Prepared = (u64, Index, Result<Option<ShieldKeys>, ShieldLoadError>);

fn prepare(
    response: &ShieldSnapshotResponse,
    private_key_b64: Option<&str>,
    open: bool,
) -> Result<Prepared, ShieldLoadError> {
    let index = match decode_snapshot(response)? {
        Some(snapshot) => Index::new(&snapshot),
        None => Index::baseline(),
    };
    let keys = match (open, private_key_b64, response.keys.as_ref()) {
        (false, _, _) => Ok(None),
        (true, Some(private_key), Some(sealed)) => open_keys(private_key, sealed).map(Some),
        (true, None, Some(_)) => Err(ShieldLoadError::Keys {
            reason: "this node has no public-ingress key".to_string(),
        }),
        (true, _, None) => Err(ShieldLoadError::Keys {
            reason: "the control plane sent none (it has no ingress key for this node)".to_string(),
        }),
    };
    Ok((response.revision, index, keys))
}

/// The snapshot kept by the last fetch, if any, swapped in.
pub async fn load_from_disk(
    shield: &WorkerShield,
    private_key_b64: Option<&str>,
) -> Result<Option<u64>, ShieldFetchError> {
    let path = shield.path().to_path_buf();
    let read = tokio::task::spawn_blocking(move || read_capped(&path))
        .await
        .map_err(|error| ShieldFetchError::Join(error.to_string()))?;
    let bytes = match read {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(reason) => {
            return Err(ShieldFetchError::Malformed {
                url: shield.path().display().to_string(),
                reason,
            })
        }
    };
    let response: ShieldSnapshotResponse =
        serde_json::from_slice(&bytes).map_err(|error| ShieldFetchError::Malformed {
            url: shield.path().display().to_string(),
            reason: error.to_string(),
        })?;
    let revision = response.revision;
    if let Err(error) = install(
        shield,
        Arc::new(response),
        private_key_b64.map(str::to_string),
    )
    .await?
    {
        warn!(%error, "Shield lists loaded from disk, but challenges cannot be issued yet");
    }
    Ok(Some(revision))
}

fn read_capped(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SNAPSHOT_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_SNAPSHOT_RESPONSE_BYTES {
        return Err(format!("larger than {MAX_SNAPSHOT_RESPONSE_BYTES} bytes"));
    }
    Ok(Some(bytes))
}

/// Write `response` to `path` through a temporary file in the same
/// directory, readable by this user only (the keys in it are sealed, but
/// the file is still nobody else's business).
fn persist(path: &Path, response: &ShieldSnapshotResponse) -> Result<(), ShieldPersistError> {
    use std::io::Write;
    let write_error = |source| ShieldPersistError::Write {
        path: path.display().to_string(),
        source,
    };
    let bytes = serde_json::to_vec(response)?;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory).map_err(write_error)?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(write_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(write_error)?;
    }
    file.write_all(&bytes).map_err(write_error)?;
    file.as_file().sync_all().map_err(write_error)?;
    file.persist(path)
        .map_err(|error| write_error(error.error))?;
    Ok(())
}
