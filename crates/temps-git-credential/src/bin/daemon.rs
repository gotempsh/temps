//! `temps-git-credential-daemon` — long-running per-sandbox process that
//! holds the workspace's deployment token and mints scoped git
//! credentials on demand.
//!
//! Trust model: runs as `temps-git` (uid 1001), distinct from the user
//! shell's `temps` (uid 1000). User code on uid 1000 cannot read the
//! deployment token (held in the daemon's memory + a 0600 env file
//! owned by uid 1001), cannot ptrace the daemon, and cannot read its
//! `/proc/<pid>/environ`. The only thing user code can do is connect to
//! the IPC socket and ask for credentials — and the daemon enforces
//! per-project, per-repo authorization on every request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use temps_git_credential::ipc::{IpcRequest, IpcResponse};
use temps_git_credential::{Operation, DEFAULT_DAEMON_ENV_PATH, DEFAULT_SOCKET_PATH};

/// Settings the daemon needs to talk to the control plane. Loaded from
/// a 0600 env-style file at startup.
#[derive(Debug)]
struct DaemonConfig {
    api_url: String,
    api_token: String,
}

impl DaemonConfig {
    fn load_from(path: &Path) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let mut url = None;
        let mut token = None;
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("TEMPS_API_URL=") {
                url = Some(rest.trim_matches('"').trim_matches('\'').to_string());
            } else if let Some(rest) = line.strip_prefix("TEMPS_API_TOKEN=") {
                token = Some(rest.trim_matches('"').trim_matches('\'').to_string());
            }
        }
        Ok(Self {
            api_url: url.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing TEMPS_API_URL in daemon env file",
                )
            })?,
            api_token: token.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing TEMPS_API_TOKEN in daemon env file",
                )
            })?,
        })
    }
}

#[derive(Debug, Serialize)]
struct MintRequestBody {
    host: String,
    owner: String,
    repo: String,
    operation: String,
}

#[derive(Debug, Deserialize)]
struct MintResponseBody {
    username: String,
    password: String,
    #[allow(dead_code)]
    expires_at: Option<String>,
}

/// Per-(host,owner,repo,op) ephemeral cache. Holds a credential for at
/// most a few seconds so back-to-back retries by git don't pound the
/// control plane. Crucially short — we WANT every operation to get a
/// fresh token from the security side; the cache is purely a
/// performance optimization for the same operation retrying within the
/// same shell command.
const CACHE_TTL_SECS: u64 = 30;

#[derive(Clone)]
struct CacheEntry {
    username: String,
    password: String,
    expires_at: std::time::Instant,
}

type Cache = Arc<Mutex<HashMap<(String, String, String, Operation), CacheEntry>>>;

#[tokio::main]
async fn main() {
    init_tracing();

    let config_path = std::env::var("TEMPS_GIT_CREDENTIAL_DAEMON_ENV")
        .unwrap_or_else(|_| DEFAULT_DAEMON_ENV_PATH.to_string());
    let config = match DaemonConfig::load_from(Path::new(&config_path)) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            error!("Failed to load daemon config from {}: {}", config_path, e);
            std::process::exit(1);
        }
    };

    let socket_path = PathBuf::from(
        std::env::var("TEMPS_GIT_CREDENTIAL_SOCKET")
            .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string()),
    );

    // Best-effort cleanup of any stale socket from a previous run.
    let _ = std::fs::remove_file(&socket_path);

    if let Some(parent) = socket_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            error!("Failed to create socket parent {}: {}", parent.display(), e);
            std::process::exit(1);
        }
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            error!(
                "Failed to bind credential socket at {}: {}",
                socket_path.display(),
                e
            );
            std::process::exit(1);
        }
    };

    // Set permissive group-read perms (0660). The Dockerfile chowns
    // the parent dir to `temps-git:git-users` so this picks up the
    // right owner; mode change is the part we control here.
    if let Err(e) = std::fs::set_permissions(
        &socket_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o660),
    ) {
        warn!(
            "Failed to set permissions on credential socket {}: {}",
            socket_path.display(),
            e
        );
    }

    info!(
        "temps-git-credential-daemon listening on {}",
        socket_path.display()
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client must build with default config");

    let cache: Cache = Arc::new(Mutex::new(HashMap::new()));

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let cfg = config.clone();
                let http = http.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, cfg, http, cache).await {
                        warn!("connection handler error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("accept error on credential socket: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn handle_connection(
    mut stream: UnixStream,
    config: Arc<DaemonConfig>,
    http: reqwest::Client,
    cache: Cache,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(512);
    stream.read_to_end(&mut buf).await?;
    let line = std::str::from_utf8(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .trim();

    let request: IpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            warn!("malformed IPC request: {}", e);
            let resp = IpcResponse::Refused {
                reason: format!("malformed request: {}", e),
            };
            write_response(&mut stream, &resp).await?;
            return Ok(());
        }
    };

    let response = match request {
        IpcRequest::Get {
            host,
            owner,
            repo,
            operation,
        } => handle_get(&host, &owner, &repo, operation, &config, &http, &cache).await,
        IpcRequest::Store => IpcResponse::Ok,
        IpcRequest::Erase { host, owner, repo } => {
            // Drop every cached entry for this repo regardless of op.
            let mut cache = cache.lock().await;
            cache.retain(|(h, o, r, _), _| !(h == &host && o == &owner && r == &repo));
            IpcResponse::Ok
        }
    };

    write_response(&mut stream, &response).await?;
    Ok(())
}

async fn write_response(stream: &mut UnixStream, resp: &IpcResponse) -> std::io::Result<()> {
    let body = serde_json::to_string(resp).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("serialize response: {e}"),
        )
    })?;
    stream.write_all(body.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.shutdown().await?;
    Ok(())
}

async fn handle_get(
    host: &str,
    owner: &str,
    repo: &str,
    operation: Operation,
    config: &DaemonConfig,
    http: &reqwest::Client,
    cache: &Cache,
) -> IpcResponse {
    let key = (
        host.to_string(),
        owner.to_string(),
        repo.to_string(),
        operation,
    );

    {
        let cache = cache.lock().await;
        if let Some(entry) = cache.get(&key) {
            if entry.expires_at > std::time::Instant::now() {
                debug!(
                    host = %host,
                    owner = %owner,
                    repo = %repo,
                    "serving credential from short-lived cache"
                );
                return IpcResponse::Credential {
                    username: entry.username.clone(),
                    password: entry.password.clone(),
                };
            }
        }
    }

    let url = format!(
        "{}/workspace/git-credential",
        config.api_url.trim_end_matches('/')
    );
    let body = MintRequestBody {
        host: host.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
        operation: match operation {
            Operation::Fetch => "fetch".to_string(),
            Operation::Push => "push".to_string(),
        },
    };

    let resp = match http
        .post(&url)
        .bearer_auth(&config.api_token)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("control-plane HTTP failed: {}", e);
            return IpcResponse::Refused {
                reason: format!("control plane unreachable: {}", e),
            };
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(%status, "control-plane refused mint: {}", body);
        return IpcResponse::Refused {
            reason: format!("control plane returned {}: {}", status, body),
        };
    }

    let parsed: MintResponseBody = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            warn!("control-plane response parse failed: {}", e);
            return IpcResponse::Refused {
                reason: format!("control plane response parse error: {}", e),
            };
        }
    };

    {
        let mut cache = cache.lock().await;
        cache.insert(
            key,
            CacheEntry {
                username: parsed.username.clone(),
                password: parsed.password.clone(),
                expires_at: std::time::Instant::now() + Duration::from_secs(CACHE_TTL_SECS),
            },
        );
    }

    info!(
        host = %host,
        owner = %owner,
        repo = %repo,
        ?operation,
        "minted credential via control plane"
    );

    IpcResponse::Credential {
        username: parsed.username,
        password: parsed.password,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Daemon must reject a config file missing TEMPS_API_TOKEN — without
    /// it, every mint call would 401, so failing fast at startup is the
    /// only sane behavior.
    #[test]
    fn config_load_requires_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.env");
        std::fs::write(&path, "TEMPS_API_URL=http://example.com\n").unwrap();
        let err = DaemonConfig::load_from(&path).unwrap_err();
        assert!(err.to_string().contains("TEMPS_API_TOKEN"));
    }

    /// Quoted values should be accepted — env files written by shells
    /// often use `KEY="value"` and we shouldn't reject that.
    #[test]
    fn config_load_strips_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.env");
        std::fs::write(
            &path,
            "TEMPS_API_URL=\"http://example.com\"\nTEMPS_API_TOKEN='dt_xyz'\n",
        )
        .unwrap();
        let cfg = DaemonConfig::load_from(&path).unwrap();
        assert_eq!(cfg.api_url, "http://example.com");
        assert_eq!(cfg.api_token, "dt_xyz");
    }
}
