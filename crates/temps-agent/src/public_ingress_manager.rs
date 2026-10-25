// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct worker ingress (ADR 058 D7).
//!
//! The worker decides what its control plane may do, in `agent.json`:
//! `off` binds nothing, `fixed` binds where the worker says, `delegated` binds
//! where the route snapshot says and follows it as it changes. The setting is
//! re-read every few seconds, so `temps agent ingress …` applies without a
//! restart; `--public-ingress-address` on the command line pins this run to
//! `fixed`. The agent reports its mode, the host addresses it could bind and
//! what it bound, so the console can show them.
//!
//! A worker enrolled before worker ingress has no key for certificate
//! bundles: one is generated here and registered with the node token.

use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use temps_core::worker_ingress::{IngressMode, ListenerReport, LocalIngress, ReportedInterface};
use tokio::sync::{watch, Notify};
use tracing::{debug, info, warn};

use crate::public_ingress::{self, PublicIngressConfig, PublicIngressHandle};
use crate::route_store::{PublicListen, SharedRouteStore};

const REPORT_INTERVAL: Duration = Duration::from_secs(60);
const RETRY_INTERVAL: Duration = Duration::from_secs(15);
/// How often `agent.json` is re-read for a changed ingress setting.
const RELOAD_INTERVAL: Duration = Duration::from_secs(5);

static LISTEN: OnceLock<watch::Sender<Option<PublicListen>>> = OnceLock::new();
/// The worker's ingress mode in force, for the other ways in (connectors).
/// Unset until [`start`].
static MODE: OnceLock<watch::Sender<IngressMode>> = OnceLock::new();

/// What this worker allows right now, or `None` before [`start`].
pub fn current_mode() -> Option<IngressMode> {
    MODE.get().map(|sender| *sender.borrow())
}

/// Wakes when the worker's ingress mode changes.
pub fn watch_mode() -> Option<watch::Receiver<IngressMode>> {
    MODE.get().map(|sender| sender.subscribe())
}

fn publish_mode(mode: IngressMode) {
    if let Some(sender) = MODE.get() {
        sender.send_if_modified(|current| {
            let changed = *current != mode;
            *current = mode;
            changed
        });
    }
}

/// Hand the latest listen settings from the control plane to the manager.
/// A no-op before [`start`], so the route sync client can call it always.
pub fn apply_listen(listen: Option<PublicListen>) {
    if let Some(sender) = LISTEN.get() {
        sender.send_if_modified(|current| {
            if *current == listen {
                false
            } else {
                *current = listen;
                true
            }
        });
    }
}

pub struct ManagerConfig {
    /// Key saved by `temps join`, if this worker was enrolled with one.
    pub private_key_b64: Option<String>,
    /// Where a self-generated key is kept.
    pub data_dir: PathBuf,
    pub control_plane_url: String,
    pub control_plane_ca: Option<crate::ControlPlaneCa>,
    pub node_id: i32,
    pub node_token: String,
    /// The setting in force at startup.
    pub ingress: LocalIngress,
    /// Started with `--public-ingress-address`: keep `ingress`, ignore
    /// `agent.json`.
    pub pinned: bool,
    /// `agent.json`, re-read for `temps agent ingress` changes.
    pub config_path: PathBuf,
}

pub fn start(
    config: ManagerConfig,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
    initial: Option<PublicListen>,
) {
    let _ = MODE.set(watch::channel(config.ingress.mode).0);
    let (sender, receiver) = watch::channel(initial);
    if LISTEN.set(sender).is_err() {
        warn!("public ingress manager already started");
        return;
    }
    tokio::spawn(run(config, store, shutdown, receiver));
}

struct Bound {
    http: SocketAddr,
    https: SocketAddr,
    handle: PublicIngressHandle,
}

async fn run(
    config: ManagerConfig,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
    mut listen: watch::Receiver<Option<PublicListen>>,
) {
    let client = match crate::with_control_plane_trust(
        reqwest::Client::builder().timeout(Duration::from_secs(15)),
        config.control_plane_ca.clone(),
    )
    .build()
    {
        Ok(client) => client,
        Err(error) => {
            public_ingress::record_failure(format!("control-plane client: {error}"));
            return;
        }
    };
    let base = config.control_plane_url.trim_end_matches('/').to_string();
    let mut key: Option<String> = None;
    let mut key_registered = false;
    let mut bound: Option<Bound> = None;
    let mut last_reported: Option<ListenerReport> = None;
    let mut last_report_at = tokio::time::Instant::now() - REPORT_INTERVAL;
    let mut local = config.ingress.clone();
    let mut retry_at = tokio::time::Instant::now();
    // The last failed bind, so it is retried every RETRY_INTERVAL rather
    // than on every pass.
    let mut failed: Option<((SocketAddr, SocketAddr), String, tokio::time::Instant)> = None;

    loop {
        if !config.pinned {
            if let Some(saved) = read_local_ingress(&config.config_path) {
                if saved != local {
                    info!(
                        mode = saved.mode.as_str(),
                        "ingress setting changed on this worker"
                    );
                    local = saved;
                }
            }
        }
        publish_mode(local.mode);

        if local.mode != IngressMode::Off
            && (key.is_none() || !key_registered)
            && tokio::time::Instant::now() >= retry_at
        {
            retry_at = tokio::time::Instant::now() + RETRY_INTERVAL;
            match ensure_key(&config, &client, &base, key.is_some()).await {
                Ok((private, registered)) => {
                    key = Some(private);
                    key_registered = registered;
                }
                Err(error) => warn!(%error, "public ingress key unavailable"),
            }
        }

        let desired = listen.borrow_and_update().clone();
        let target = resolve(&local, desired.as_ref());
        let mut error = None;
        match target {
            Ok(None) => {
                if let Some(current) = bound.take() {
                    info!(http = %current.http, https = %current.https, "public worker ingress turned off; releasing ports");
                    current.handle.stop();
                }
            }
            Ok(Some((http, https))) => {
                let unchanged = bound
                    .as_ref()
                    .is_some_and(|b| b.http == http && b.https == https && b.handle.is_ready());
                let backing_off = failed.as_ref().filter(|(target, _, at)| {
                    *target == (http, https) && tokio::time::Instant::now() < *at
                });
                if let (false, Some((_, message, _))) = (unchanged, backing_off) {
                    error = Some(message.clone());
                } else if !unchanged {
                    if let Some(current) = bound.take() {
                        info!(from = %current.http, to = %http, "public worker ingress moving");
                        current.handle.stop();
                        // Let the aborted accept loops drop their sockets.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    match key.clone() {
                        None => error = Some(
                            "no ingress key yet: waiting to register one with the control plane"
                                .to_string(),
                        ),
                        Some(private_key_b64) => {
                            let ingress = PublicIngressConfig {
                                http_address: http,
                                https_address: https,
                                private_key_b64,
                                control_plane_url: config.control_plane_url.clone(),
                                control_plane_ca: config.control_plane_ca.clone(),
                                node_id: config.node_id,
                                node_token: config.node_token.clone(),
                            };
                            match public_ingress::spawn(ingress, store.clone(), shutdown.clone())
                                .await
                            {
                                Ok(handle) => {
                                    failed = None;
                                    info!(http = %handle.http_address(), https = %handle.https_address(), mode = local.mode.as_str(), "public worker ingress started");
                                    bound = Some(Bound {
                                        http: handle.http_address(),
                                        https: handle.https_address(),
                                        handle,
                                    });
                                }
                                Err(e) => {
                                    let message = explain_bind_error(&e.to_string(), http, https);
                                    warn!(error = %message, "public worker ingress failed to start");
                                    public_ingress::record_failure(message.clone());
                                    failed = Some((
                                        (http, https),
                                        message.clone(),
                                        tokio::time::Instant::now() + RETRY_INTERVAL,
                                    ));
                                    error = Some(message);
                                }
                            }
                        }
                    }
                }
            }
            Err(message) => {
                if let Some(current) = bound.take() {
                    current.handle.stop();
                }
                public_ingress::record_failure(message.clone());
                error = Some(message);
            }
        }

        let report = ListenerReport {
            interfaces: host_interfaces(),
            http: bound.as_ref().map(|b| b.http.to_string()),
            https: bound.as_ref().map(|b| b.https.to_string()),
            mode: Some(local.mode),
            error: error.clone(),
        };
        if last_reported.as_ref() != Some(&report) || last_report_at.elapsed() >= REPORT_INTERVAL {
            match send_report(&client, &base, &config, &report).await {
                Ok(()) => {
                    last_reported = Some(report);
                    last_report_at = tokio::time::Instant::now();
                }
                Err(e) => debug!(error = %e, "could not report public ingress listener"),
            }
        }

        tokio::select! {
            _ = shutdown.notified() => {
                if let Some(current) = bound.take() {
                    current.handle.stop();
                }
                return;
            }
            changed = listen.changed() => if changed.is_err() { return },
            _ = tokio::time::sleep(RELOAD_INTERVAL) => {}
        }
    }
}

/// Where to bind under `local`, given what the control plane asks for.
fn resolve(
    local: &LocalIngress,
    desired: Option<&PublicListen>,
) -> Result<Option<(SocketAddr, SocketAddr)>, String> {
    let (address, http_port, https_port) = match local.mode {
        IngressMode::Off => return Ok(None),
        IngressMode::Fixed => (local.listen, local.http_port, local.https_port),
        IngressMode::Delegated => {
            // Off in the console, or a control plane that predates modes.
            let Some(desired) = desired else {
                return Ok(None);
            };
            let address = match desired.address.as_deref() {
                Some(text) => {
                    let ip = text.parse::<IpAddr>().map_err(|_| {
                        format!(
                            "listen address '{text}' from the control plane is not an IP address"
                        )
                    })?;
                    check_delegated_address(ip, &own_addresses())?;
                    Some(ip)
                }
                None => None,
            };
            (address, desired.http_port, desired.https_port)
        }
    };
    let address = match address {
        Some(ip) => ip,
        None => default_route_address().ok_or_else(|| {
            "listen address is automatic, but this worker has no IPv4 default route to take it from; choose an address".to_string()
        })?,
    };
    Ok(Some((
        SocketAddr::new(address, http_port),
        SocketAddr::new(address, https_port),
    )))
}

/// Interface name prefixes that are Temps' own or container networking: the
/// WireGuard mesh (`temps-wg0`), the overlay, Docker and CNI bridges. Never a
/// public listener, and never reported as a candidate.
const NON_PUBLIC_INTERFACES: &[&str] = &[
    "docker", "br-", "veth", "temps", "cni", "virbr", "flannel", "lo",
];

/// Every address on this host with its interface name.
fn own_addresses() -> Vec<(String, IpAddr)> {
    if_addrs::get_if_addrs()
        .map(|interfaces| {
            interfaces
                .into_iter()
                .map(|i| (i.name.clone(), i.ip()))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the control plane may make this worker listen on `ip` in
/// `delegated` mode. Delegation lets it pick among the worker's public-facing
/// addresses, nothing more: not every address at once, not loopback, and not
/// the mesh, overlay or a container network, which would expose services the
/// worker only shares with the cluster.
fn check_delegated_address(ip: IpAddr, own: &[(String, IpAddr)]) -> Result<(), String> {
    let refuse = |why: String| {
        Err(format!(
            "refused listen address {ip} from the control plane: {why}. Choose one of this worker's own addresses in the console, or set `temps agent ingress fixed` on the worker."
        ))
    };
    if ip.is_unspecified() {
        return refuse(
            "it would listen on every address of this worker, including the private mesh".into(),
        );
    }
    if ip.is_loopback() {
        return refuse("it is a loopback address".into());
    }
    match own.iter().find(|(_, address)| *address == ip) {
        None => refuse("it is not an address of this worker".into()),
        Some((name, _)) if NON_PUBLIC_INTERFACES.iter().any(|p| name.starts_with(p)) => refuse(
            format!("it belongs to {name}, a private mesh, overlay or container network"),
        ),
        Some(_) => Ok(()),
    }
}

/// The ingress setting saved in `agent.json`, or `None` when it can't be
/// read (keep the current one).
fn read_local_ingress(path: &std::path::Path) -> Option<LocalIngress> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<crate::AgentConfig>(&text) {
        Ok(config) => Some(config.local_ingress()),
        Err(error) => {
            debug!(%error, path = %path.display(), "agent config unreadable; keeping the current ingress setting");
            None
        }
    }
}

fn explain_bind_error(error: &str, http: SocketAddr, https: SocketAddr) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("address already in use") {
        format!("another program already listens on {http} or {https} (a web server such as nginx, Caddy or Traefik?). Stop it, or choose other local ports in the console and forward 80 and 443 to them.")
    } else if lower.contains("cannot assign requested address")
        || lower.contains("address not available")
    {
        format!("{} is not an address of this machine. Behind NAT, listen on the local interface address and enter the router's public IP as the public address.", http.ip())
    } else if lower.contains("permission denied") {
        format!("the agent may not bind ports {} and {}: ports below 1024 need root or CAP_NET_BIND_SERVICE. Run the agent service as root, or choose ports above 1024 and forward 80 and 443 to them.", http.port(), https.port())
    } else {
        error.to_string()
    }
}

/// The source address the kernel picks for the default IPv4 route. Connecting
/// a UDP socket sends nothing; it only resolves the route.
fn default_route_address() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// Host addresses a public listener could bind: Temps' own devices, container
/// bridges, loopback and link-local left out.
fn host_interfaces() -> Vec<ReportedInterface> {
    const SKIP: &[&str] = NON_PUBLIC_INTERFACES;
    let default = default_route_address();
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<ReportedInterface> = interfaces
        .into_iter()
        .filter(|i| !i.is_loopback() && !SKIP.iter().any(|p| i.name.starts_with(p)))
        .filter(|i| match i.ip() {
            IpAddr::V4(v4) => !v4.is_link_local(),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
        })
        .map(|i| ReportedInterface {
            default_route: Some(i.ip()) == default,
            address: i.ip().to_string(),
            name: i.name,
        })
        .collect();
    out.sort_by(|a, b| {
        b.default_route
            .cmp(&a.default_route)
            .then(a.name.cmp(&b.name))
    });
    out.dedup();
    out
}

/// The private key to bind with, and whether the control plane has its
/// public half. A key from `temps join` was registered at enrollment.
async fn ensure_key(
    config: &ManagerConfig,
    client: &reqwest::Client,
    base: &str,
    have_key: bool,
) -> Result<(String, bool), String> {
    if let Some(key) = &config.private_key_b64 {
        return Ok((key.clone(), true));
    }
    let path = config.data_dir.join("public-ingress.key");
    let (private, public) = match std::fs::read_to_string(&path) {
        Ok(saved) => {
            let private = saved.trim().to_string();
            let public = public_half(&private)?;
            (private, public)
        }
        Err(_) if !have_key => {
            let (private, public) = temps_core::worker_ingress::generate_ingress_key()
                .map_err(|e| format!("generate ingress key: {e}"))?;
            write_private(&path, &private)?;
            info!(path = %path.display(), "generated an ingress key for certificate bundles");
            (private, public)
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let url = format!(
        "{base}/api/internal/nodes/{}/public-ingress/key",
        config.node_id
    );
    let registered = match client
        .post(&url)
        .bearer_auth(&config.node_token)
        .json(&serde_json::json!({ "public_key": public }))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => true,
        Ok(response) => {
            let status = response.status();
            let reason = response.text().await.unwrap_or_default();
            let reason = reason.chars().take(500).collect::<String>();
            warn!(%status, %reason, "control plane did not accept the ingress key; HTTPS certificates cannot reach this worker until it does");
            public_ingress::record_failure(format!(
                "the control plane did not accept this worker's ingress key ({status}): {reason}"
            ));
            false
        }
        Err(e) => {
            debug!(error = %e, "could not register the ingress key yet");
            false
        }
    };
    Ok((private, registered))
}

fn public_half(private_b64: &str) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes: [u8; 32] = STANDARD
        .decode(private_b64)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("saved ingress key is not a base64 X25519 key")?;
    let secret = x25519_dalek::StaticSecret::from(bytes);
    Ok(STANDARD.encode(x25519_dalek::PublicKey::from(&secret).as_bytes()))
}

fn write_private(path: &std::path::Path, private: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("key.part");
    std::fs::write(&tmp, private).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("save {}: {e}", path.display()))
}

async fn send_report(
    client: &reqwest::Client,
    base: &str,
    config: &ManagerConfig,
    report: &ListenerReport,
) -> Result<(), String> {
    let url = format!(
        "{base}/api/internal/nodes/{}/public-ingress/report",
        config.node_id
    );
    let response = client
        .post(&url)
        .bearer_auth(&config.node_token)
        .json(report)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // A control plane that predates console-managed ingress.
        return Ok(());
    }
    response
        .error_for_status()
        .map(|_| ())
        .map_err(|e| format!("POST {url}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Vec<(String, IpAddr)> {
        vec![
            ("lo".into(), "127.0.0.1".parse().unwrap()),
            ("eth0".into(), "192.168.1.8".parse().unwrap()),
            ("temps-wg0".into(), "10.201.0.3".parse().unwrap()),
            ("docker0".into(), "172.17.0.1".parse().unwrap()),
        ]
    }

    #[test]
    fn delegated_bind_takes_only_a_public_facing_address_of_this_worker() {
        assert!(check_delegated_address("192.168.1.8".parse().unwrap(), &host()).is_ok());
        for (ip, why) in [
            ("0.0.0.0", "every address"),
            ("::", "every address"),
            ("127.0.0.1", "loopback"),
            ("10.201.0.3", "temps-wg0"),
            ("172.17.0.1", "docker0"),
            ("203.0.113.9", "not an address of this worker"),
        ] {
            let error = check_delegated_address(ip.parse().unwrap(), &host()).unwrap_err();
            assert!(error.contains(why), "{ip}: {error}");
        }
    }

    #[test]
    fn fixed_mode_keeps_the_workers_own_choice() {
        let local = LocalIngress {
            mode: IngressMode::Fixed,
            listen: Some("0.0.0.0".parse().unwrap()),
            http_port: 80,
            https_port: 443,
        };
        let (http, _) = resolve(&local, None).unwrap().unwrap();
        assert!(http.ip().is_unspecified());
    }
}
