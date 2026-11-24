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

use std::path::{Path, PathBuf};
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
    /// Where the public-ingress manager keeps the key it generates when
    /// `private_key_b64` is unset; read on every fetch, so a key
    /// provisioned after startup is picked up.
    pub key_file: Option<PathBuf>,
    pub uploader: UploaderSettings,
}

impl ShieldSyncConfig {
    /// The key to open the sealed keys with: the configured one, else the
    /// one the ingress manager saved.
    fn private_key(&self) -> Option<String> {
        self.private_key_b64.clone().or_else(|| {
            let saved = std::fs::read_to_string(self.key_file.as_ref()?).ok()?;
            let saved = saved.trim();
            (!saved.is_empty()).then(|| saved.to_string())
        })
    }
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
    #[error(
        "the control plane has nothing newer than revision {loaded}, but the routes name {wanted}"
    )]
    Behind { loaded: u64, wanted: u64 },
    #[error("Shield lists are loaded, but challenges cannot be issued: {0}")]
    Keys(ShieldLoadError),
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
    match load_from_disk(&shield, config.private_key().as_deref()).await {
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
        // The routes name a revision this control plane says is not newer:
        // another control plane stored it and this one has not seen it
        // yet. Retry rather than wait for the next route change.
        StatusCode::NOT_MODIFIED => {
            if let Some(wanted) = wanted.filter(|wanted| wanted.revision > loaded) {
                return Err(ShieldFetchError::Behind {
                    loaded,
                    wanted: wanted.revision,
                });
            }
            return Ok(Fetched::Unchanged);
        }
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
    let keys = install(shield, Arc::clone(&snapshot), config.private_key()).await?;
    let path = shield.path().to_path_buf();
    match tokio::task::spawn_blocking(move || persist_keeping_keys(&path, &snapshot)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(%error, "the Shield snapshot is loaded but not kept on disk"),
        Err(error) => warn!(%error, "the Shield snapshot is loaded but not kept on disk"),
    }
    // Retried with backoff: the ingress key may only be provisioned later,
    // and a route change need not follow when it is.
    keys.map_err(ShieldFetchError::Keys)?;
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

/// As [`persist`], keeping the sealed keys already on disk when `response`
/// carries none: a response without keys says nothing about the ones the
/// worker has, and a restart must still find them.
fn persist_keeping_keys(
    path: &Path,
    response: &ShieldSnapshotResponse,
) -> Result<(), ShieldPersistError> {
    if response.keys.is_some() {
        return persist(path, response);
    }
    let kept = read_capped(path)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<ShieldSnapshotResponse>(&bytes).ok())
        .and_then(|saved| saved.keys);
    match kept {
        Some(keys) => persist(
            path,
            &ShieldSnapshotResponse {
                keys: Some(keys),
                ..response.clone()
            },
        ),
        None => persist(path, response),
    }
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

#[cfg(test)]
mod tests {

    use tempfile::TempDir;
    use temps_core::worker_shield::{ShieldRef, ShieldSnapshotResponse};

    use super::*;
    use crate::shield::tests::*;
    use crate::shield::{env_policy, guard, WorkerShield};
    use axum::http::StatusCode;

    fn sync_config(url: &str, private_key: Option<String>) -> ShieldSyncConfig {
        ShieldSyncConfig {
            control_plane_url: url.to_string(),
            node_id: 7,
            node_token: "node-token".to_string(),
            http: reqwest::Client::new(),
            private_key_b64: private_key,
            key_file: None,
            uploader: UploaderSettings::default(),
        }
    }

    /// A worker with a `selective` route and nothing loaded.
    fn routed_shield(dir: &TempDir) -> WorkerShield {
        let shield = WorkerShield::new(dir.path().join(crate::shield::SNAPSHOT_FILE));
        shield.set_routes(
            [(HOST.to_string(), env_policy(&policy("selective", None)))]
                .into_iter()
                .collect(),
            Some(ShieldRef { revision: 5 }),
        );
        shield
    }

    async fn blocks_listed(shield: &WorkerShield) -> bool {
        guard(shield, HOST, get_page("/"), visitor(listed()))
            .await
            .is_err_and(|response| response.status() == StatusCode::FORBIDDEN)
    }

    #[tokio::test]
    async fn a_fetched_snapshot_is_swapped_in_kept_on_disk_and_loaded_offline() {
        let (private_key, public_key) = keypair();
        let (plane, url) = control_plane(stored_response(5, &public_key)).await;
        let config = sync_config(&url, Some(private_key.clone()));
        let dir = TempDir::new().unwrap();
        let shield = routed_shield(&dir);
        assert!(
            !blocks_listed(&shield).await,
            "the built-in lists do not list it"
        );

        let wanted = Some(ShieldRef { revision: 5 });
        assert_eq!(
            fetch_once(&config, &shield, wanted).await.unwrap(),
            Fetched::Installed(5)
        );
        assert_eq!(shield.revision(), 5);
        assert!(shield.has_engine());
        assert!(blocks_listed(&shield).await);
        // With no engine yet, the worker asks for everything (revision 0).
        assert_eq!(*plane.asked.lock().unwrap(), vec![0]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join(crate::shield::SNAPSHOT_FILE);
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // The revision the routes name is loaded: nothing to ask.
        assert_eq!(
            fetch_once(&config, &shield, wanted).await.unwrap(),
            Fetched::Unchanged
        );
        assert_eq!(plane.asked.lock().unwrap().len(), 1);
        // A newer one named, but this control plane has nothing newer yet: a
        // 304 that leaves the worker behind is retried, not taken as current.
        assert!(matches!(
            fetch_once(&config, &shield, Some(ShieldRef { revision: 6 })).await,
            Err(ShieldFetchError::Behind {
                loaded: 5,
                wanted: 6
            })
        ));
        assert_eq!(*plane.asked.lock().unwrap(), vec![0, 5]);

        // Restarted without the control plane: the same lists and keys.
        let restarted = routed_shield(&dir);
        assert_eq!(
            load_from_disk(&restarted, Some(&private_key))
                .await
                .unwrap(),
            Some(5)
        );
        assert!(restarted.has_engine());
        assert!(blocks_listed(&restarted).await);

        // Nothing on disk is nothing loaded.
        let empty = TempDir::new().unwrap();
        let fresh = routed_shield(&empty);
        assert_eq!(
            load_from_disk(&fresh, Some(&private_key)).await.unwrap(),
            None
        );
        assert!(!fresh.has_engine());
    }

    #[tokio::test]
    async fn an_old_control_plane_or_a_failing_one_leaves_what_is_loaded() {
        let (private_key, public_key) = keypair();
        let (plane, url) = control_plane(stored_response(5, &public_key)).await;
        let config = sync_config(&url, Some(private_key));
        let dir = TempDir::new().unwrap();
        let shield = routed_shield(&dir);

        *plane.answer.lock().unwrap() = Answer::NotFound;
        assert_eq!(
            fetch_once(&config, &shield, None).await.unwrap(),
            Fetched::Unsupported
        );
        *plane.answer.lock().unwrap() = Answer::Failing;
        assert!(fetch_once(&config, &shield, None).await.is_err());
        assert_eq!(shield.revision(), 0);
        assert!(!shield.has_engine());
        assert!(!dir.path().join(crate::shield::SNAPSHOT_FILE).exists());
    }

    #[tokio::test]
    async fn lists_load_even_when_the_keys_are_for_another_node() {
        let (_, public_key) = keypair();
        let (other_private, _) = keypair();
        let (_plane, url) = control_plane(stored_response(5, &public_key)).await;
        let dir = TempDir::new().unwrap();
        let shield = routed_shield(&dir);
        // The keys failing is an error, so the sync retries, but the lists
        // are in use already.
        assert!(matches!(
            fetch_once(&sync_config(&url, Some(other_private)), &shield, None).await,
            Err(ShieldFetchError::Keys(_))
        ));
        assert_eq!(shield.revision(), 5);
        assert!(!shield.has_engine());
        assert!(blocks_listed(&shield).await);
    }

    #[tokio::test]
    async fn a_key_provisioned_after_startup_opens_the_keys() {
        let (private_key, public_key) = keypair();
        let (_plane, url) = control_plane(stored_response(5, &public_key)).await;
        let dir = TempDir::new().unwrap();
        let key_file = dir.path().join("public-ingress.key");
        let config = ShieldSyncConfig {
            key_file: Some(key_file.clone()),
            ..sync_config(&url, None)
        };
        let shield = routed_shield(&dir);
        assert!(matches!(
            fetch_once(&config, &shield, None).await,
            Err(ShieldFetchError::Keys(_))
        ));
        assert!(!shield.has_engine());
        // The ingress manager saves the key it registered.
        std::fs::write(&key_file, format!("{private_key}\n")).unwrap();
        assert_eq!(
            fetch_once(&config, &shield, None).await.unwrap(),
            Fetched::Installed(5)
        );
        assert!(shield.has_engine());
    }

    #[tokio::test]
    async fn a_response_without_keys_keeps_the_ones_on_disk() {
        let (private_key, public_key) = keypair();
        let dir = TempDir::new().unwrap();
        let (_plane, url) = control_plane(stored_response(5, &public_key)).await;
        let shield = routed_shield(&dir);
        fetch_once(&sync_config(&url, Some(private_key.clone())), &shield, None)
            .await
            .unwrap();

        // A later restart fetches from a control plane that sends no keys.
        let keyless = ShieldSnapshotResponse {
            keys: None,
            ..stored_response(6, &public_key)
        };
        let (_plane, url) = control_plane(keyless).await;
        let restarted = routed_shield(&dir);
        assert!(fetch_once(
            &sync_config(&url, Some(private_key.clone())),
            &restarted,
            None
        )
        .await
        .is_err());
        assert_eq!(restarted.revision(), 6);

        // The keys from the first fetch are still on disk for the next start.
        let again = routed_shield(&dir);
        assert_eq!(
            load_from_disk(&again, Some(&private_key)).await.unwrap(),
            Some(6)
        );
        assert!(again.has_engine());
    }
}
