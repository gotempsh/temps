// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Worker-side mirror of the CP's internal-zone route table.
//!
//! Holds the current `*.temps.local` host → backends map in memory and
//! persists each successful apply to disk so a restarted agent serves
//! stale-but-correct data before the first sync round completes. The
//! [internal proxy](crate::internal_proxy) reads from this store on
//! every request. Wire model and update protocol are in
//! [`temps_routes::route_sync`].
//!
//! ## Atomic snapshots
//!
//! Replace-the-whole-map semantics. Internal routes retain a brief read lock;
//! public ingress routes and prepared TLS keys use immutable `ArcSwap` maps so
//! request and handshake lookups never lock or observe a partially built map.
//! Applies build each replacement off-path and publish it atomically.
//!
//! ## Disk snapshot
//!
//! Written best-effort to `<snapshot_dir>/routes.json` after every
//! successful apply. Disk write failure is logged and ignored — the
//! agent keeps serving from memory. On restart, [`load_from_disk`] is
//! the cold-start path; if the file doesn't exist or is unparsable,
//! the store is empty and the proxy returns 503 until the first sync
//! round finishes.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// One backend reachable for a host. `address` is the dial-as-is form
/// produced on the CP — overlay IP for same-node containers, underlay
/// IP + published port for cross-node, etc. The agent does not parse
/// or rewrite it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteBackend {
    pub address: String,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
}

/// One internal-zone route. `host` is the lower-cased FQDN the proxy
/// matches `Host:` against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteEntry {
    pub host: String,
    pub backends: Vec<RouteBackend>,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    /// The signed-in gate in front of this address (ADR 065 D8). Only on
    /// routes in `gated_routes`, here and in `routes.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<temps_core::gate::GatePolicy>,
    /// The environment's Shield settings (ADR 066, D10). The lookup index
    /// keeps them in [`crate::shield::WorkerShield`] instead, so a request
    /// does not clone them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shield: Option<temps_core::worker_shield::ShieldRoutePolicy>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublicIngressSnapshot {
    pub enabled: bool,
    pub routes: Vec<RouteEntry>,
    #[serde(default)]
    pub certificates: Option<PublicIngressCertificates>,
    #[serde(default)]
    pub unsupported_route_count: usize,
    #[serde(default)]
    pub unsupported_reasons: Vec<String>,
    /// Where to bind the public listeners (ADR 058 D7); `None` when this
    /// node takes no direct traffic or the control plane predates it.
    #[serde(default)]
    pub listen: Option<PublicListen>,
    /// Addresses only the private listener serves (ADR 064 D5). Never in
    /// `routes`: the direct listeners and the tunnel origin answer them 404.
    #[serde(default)]
    pub private_routes: Vec<PrivateRouteEntry>,
    /// What this worker serves through tailscaled (ADR 065 D4). Never in
    /// `routes` or `private_routes`: only the address's own Serve socket
    /// answers it.
    #[serde(default)]
    pub serve_entries: Vec<ServeEntry>,
    /// `tailnet_users` private addresses (ADR 065 D6). Never in
    /// `private_routes`, here or in `routes.json`: an agent rolled back to a
    /// version without identity checks reads that file and ignores this
    /// field, so it serves none of them to the whole range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_private_routes: Vec<PrivateRouteEntry>,
    /// The routes of `tailnet_users` Serve entries (ADR 065 D6). Their entry
    /// in `serve_entries` keeps no route, so an older agent's socket answers
    /// 404 for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_serve_routes: Vec<IdentityServeRoute>,
    /// Signed-in addresses (ADR 065 D7), each with its gate. Never in
    /// `routes`, here or in `routes.json`: an agent rolled back to a version
    /// without the gate reads that file and ignores this field, so it serves
    /// none of them, open, for the rest of its lease.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gated_routes: Vec<RouteEntry>,
    /// Routes whose environment acts on verdicts (ADR 066, D10), each with
    /// its Shield settings. Never in `routes` or `gated_routes`, here or in
    /// `routes.json`: an agent rolled back to a version without Shield
    /// reads that file and ignores this field, so it serves none of them
    /// unprotected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shield_routes: Vec<RouteEntry>,
    /// The current Shield snapshot revision, when the control plane runs
    /// Shield on workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shield: Option<temps_core::worker_shield::ShieldRef>,
}

/// Where a `tailnet_users` Serve socket sends requests, and who it admits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityServeRoute {
    pub address_id: String,
    pub route: RouteEntry,
    /// An empty list admits nobody.
    pub principals: Vec<temps_core::tailnet::TailnetPrincipal>,
}

impl PublicIngressSnapshot {
    /// ADR 065 D7: move every signed-in route out of `routes`, whatever put
    /// it there, and drop any route in `gated_routes` that names no gate
    /// (it would be served open).
    fn separate_gated(&mut self) {
        let (gated, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.routes)
            .into_iter()
            .partition(|route| route.gate.is_some());
        self.routes = open;
        self.gated_routes.extend(gated);
        self.gated_routes.retain(|route| {
            if route.gate.is_none() {
                warn!(host = %route.host, "dropping a signed-in route that names no gate");
            }
            route.gate.is_some()
        });
    }

    /// ADR 066 D10: move every route that acts on verdicts out of `routes`
    /// and `gated_routes`, whatever put it there, and drop any route in
    /// `shield_routes` that carries no Shield settings (it would be served
    /// unprotected).
    fn separate_shielded(&mut self) {
        let enforces = |route: &RouteEntry| {
            route
                .shield
                .as_ref()
                .is_some_and(|policy| crate::shield::policy_mode(policy).enforces())
        };
        let (shielded, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.routes)
            .into_iter()
            .partition(enforces);
        self.routes = open;
        self.shield_routes.extend(shielded);
        let (shielded, gated): (Vec<_>, Vec<_>) = std::mem::take(&mut self.gated_routes)
            .into_iter()
            .partition(enforces);
        self.gated_routes = gated;
        self.shield_routes.extend(shielded);
        self.shield_routes.retain(|route| {
            if route.shield.is_none() {
                warn!(host = %route.host, "dropping a Shield route that carries no Shield settings");
            }
            route.shield.is_some()
        });
    }

    /// ADR 065 D6: move every `tailnet_users` route out of the fields an
    /// older agent reads, whatever put it there, and make sure none of the
    /// moved ones is left without a list (that would be the whole range).
    fn separate_tailnet_users(&mut self) {
        let (restricted, open): (Vec<_>, Vec<_>) = std::mem::take(&mut self.private_routes)
            .into_iter()
            .partition(|route| route.principals.is_some());
        self.private_routes = open;
        self.identity_private_routes.extend(restricted);
        for route in &mut self.identity_private_routes {
            route.principals.get_or_insert_with(Vec::new);
        }
        for entry in &mut self.serve_entries {
            let Some(principals) = entry.principals.take() else {
                continue;
            };
            if let Some(route) = entry.route.take() {
                self.identity_serve_routes.push(IdentityServeRoute {
                    address_id: entry.address_id.clone(),
                    route,
                    principals,
                });
            }
        }
    }
}

/// One Serve entry the control plane declared for this worker (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServeEntry {
    /// The `tailnet_serve_entries` row: what the agent's report names.
    pub entry_id: i32,
    /// The address, and so its socket.
    pub address_id: String,
    /// The tailnet name visitors open.
    pub hostname: String,
    /// `tailscale_serve` or `tailscale_service`.
    pub via: String,
    /// The `svc:` name, for a Service.
    #[serde(default)]
    pub service: Option<String>,
    /// No run needs it any more: take it off, then report.
    #[serde(default)]
    pub removing: bool,
    /// The Serve config `Etag` the agent's last write produced, as recorded.
    #[serde(default)]
    pub applied_etag: Option<String>,
    /// Where requests go. `None` while it is held back or being removed:
    /// the socket then answers 404.
    #[serde(default)]
    pub route: Option<RouteEntry>,
    /// A Service's addresses as Tailscale assigned them (D4): the only
    /// answers the inside check accepts for its name.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// For `tailnet_users` (ADR 065 D6): who the socket admits. `None` is
    /// the whole tailnet; an empty list admits nobody. Set only in the
    /// lookup index, from `identity_serve_routes`: in a snapshot, and so in
    /// `routes.json`, it is always moved out with the route.
    #[serde(default)]
    pub principals: Option<Vec<temps_core::tailnet::TailnetPrincipal>>,
}

/// A private address and what bounds it (ADR 064 D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivateRouteEntry {
    pub route: RouteEntry,
    /// What the control plane calls it; the inside check maps it back here.
    pub address_id: String,
    /// The network a visitor's TCP peer address must be in.
    pub range: String,
    /// The listener address DNS points at.
    pub listener: String,
    /// For `tailnet_users` (ADR 065 D6): who the listener admits, by whois
    /// of the TCP peer. `None` is the whole range; an empty list admits
    /// nobody.
    #[serde(default)]
    pub principals: Option<Vec<temps_core::tailnet::TailnetPrincipal>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicListen {
    /// Local address to bind; `None` means the default-route interface.
    pub address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicIngressCertificates {
    pub ephemeral_public_key: String,
    pub bundles: Vec<PublicIngressCertBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicIngressCertBundle {
    pub domain: String,
    pub ciphertext: String,
    pub nonce: String,
    pub fingerprint: String,
}

/// On-disk snapshot. Versioned via the wrapping struct so a future
/// schema change (e.g. adding affinity hints) can use `serde`'s
/// `default` rather than a breaking parse failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskSnapshot {
    pub generation: u64,
    pub routes: Vec<RouteEntry>,
    #[serde(default)]
    pub public_ingress: PublicIngressSnapshot,
    #[serde(default)]
    pub public_ingress_expires_at: i64,
}

pub struct RouteStore {
    inner: RwLock<HashMap<String, RouteEntry>>,
    container_projects: RwLock<HashMap<String, HashSet<i32>>>,
    generation: RwLock<u64>,
    public_ingress: RwLock<PublicIngressSnapshot>,
    public_enabled: AtomicBool,
    public_routes: ArcSwap<HashMap<String, RouteEntry>>,
    private_routes: ArcSwap<HashMap<String, PrivateRouteEntry>>,
    /// Serve entries with a route, by address id (ADR 065 D3).
    serve_routes: ArcSwap<HashMap<String, ServeEntry>>,
    public_certificates: RwLock<Option<(String, HashMap<String, PublicIngressCertBundle>)>>,
    public_tls_private_key: RwLock<Option<String>>,
    public_tls_keys: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
    public_ingress_expires_at: AtomicI64,
    snapshot_path: PathBuf,
    /// Shield on this worker's public ingress (ADR 066, D10, I5): the
    /// loaded snapshot, the challenge gate and each route's settings.
    shield: Arc<crate::shield::WorkerShield>,
}

impl RouteStore {
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            container_projects: RwLock::new(HashMap::new()),
            generation: RwLock::new(0),
            public_ingress: RwLock::new(PublicIngressSnapshot::default()),
            public_enabled: AtomicBool::new(false),
            public_routes: ArcSwap::from_pointee(HashMap::new()),
            private_routes: ArcSwap::from_pointee(HashMap::new()),
            serve_routes: ArcSwap::from_pointee(HashMap::new()),
            public_certificates: RwLock::new(None),
            public_tls_private_key: RwLock::new(None),
            public_tls_keys: ArcSwap::from_pointee(HashMap::new()),
            public_ingress_expires_at: AtomicI64::new(0),
            shield: Arc::new(crate::shield::WorkerShield::new(
                snapshot_path.with_file_name(crate::shield::SNAPSHOT_FILE),
            )),
            snapshot_path,
        }
    }

    /// Shield on this worker's public ingress, which reads its routes from
    /// this store.
    pub fn shield(&self) -> &Arc<crate::shield::WorkerShield> {
        &self.shield
    }

    /// Replace the in-memory store with the given snapshot, persist
    /// to disk best-effort. Returns the new generation.
    pub fn apply_snapshot(&self, generation: u64, routes: Vec<RouteEntry>) -> u64 {
        let mut map = HashMap::with_capacity(routes.len());
        let container_projects = container_project_index(&routes);
        for r in &routes {
            map.insert(r.host.to_ascii_lowercase(), r.clone());
        }
        *self.inner.write() = map;
        *self.container_projects.write() = container_projects;
        *self.generation.write() = generation;

        // Best-effort disk persistence. We tolerate any error here —
        // the in-memory store is already updated and the proxy serves
        // from there.
        let snap = DiskSnapshot {
            generation,
            routes,
            public_ingress: self.public_ingress.read().clone(),
            public_ingress_expires_at: self.public_ingress_expires_at.load(Ordering::Acquire),
        };
        if let Err(e) = self.persist(&snap) {
            warn!(
                error = %e,
                path = %self.snapshot_path.display(),
                "failed to persist route snapshot"
            );
        }

        info!(
            generation,
            entries = self.inner.read().len(),
            "applied route snapshot"
        );
        generation
    }

    /// Apply the authenticated public-ingress portion of the control-plane
    /// snapshot. The encrypted certificate bundles are safe to cache on disk;
    /// decrypted private keys are never stored here.
    pub fn apply_public_snapshot(&self, mut snapshot: PublicIngressSnapshot) {
        snapshot.separate_gated();
        snapshot.separate_tailnet_users();
        snapshot.separate_shielded();
        if !snapshot.enabled {
            self.public_enabled.store(false, Ordering::Release);
        }
        self.rebuild_public_indexes(&snapshot);
        self.rebuild_public_tls_keys(&snapshot);
        self.public_enabled
            .store(snapshot.enabled, Ordering::Release);
        *self.public_ingress.write() = snapshot;
        self.public_ingress_expires_at.store(
            chrono::Utc::now().timestamp().saturating_add(300),
            Ordering::Release,
        );
        self.persist_current();
    }

    pub fn public_ingress_snapshot(&self) -> PublicIngressSnapshot {
        let mut snapshot = self.public_ingress.read().clone();
        if !self.public_ingress_authorized() {
            snapshot.enabled = false;
        }
        snapshot
    }

    pub(crate) fn public_ingress_runtime_status(&self) -> (bool, bool, usize) {
        (
            self.public_enabled.load(Ordering::Acquire),
            self.public_ingress_authorized(),
            self.public_tls_keys.load().len(),
        )
    }

    pub fn lookup_public(&self, host: &str) -> Option<RouteEntry> {
        if !self.public_enabled.load(Ordering::Acquire) || !self.public_ingress_authorized() {
            return None;
        }
        self.public_routes
            .load()
            .get(&host.to_ascii_lowercase())
            .cloned()
    }

    /// A private address the private listener serves (ADR 064 D5), under
    /// the same authorization lease as public routes.
    pub fn lookup_private(&self, host: &str) -> Option<PrivateRouteEntry> {
        if !self.public_enabled.load(Ordering::Acquire) || !self.public_ingress_authorized() {
            return None;
        }
        self.private_routes
            .load()
            .get(host.trim_end_matches('.').to_ascii_lowercase().as_str())
            .cloned()
    }

    /// The address a Serve socket serves (ADR 065 D3), by its id, under the
    /// same authorization lease as public routes. Only an entry the control
    /// plane still wants served, with a route.
    pub fn lookup_serve(&self, address_id: &str) -> Option<ServeEntry> {
        if !self.public_enabled.load(Ordering::Acquire) || !self.public_ingress_authorized() {
            return None;
        }
        self.serve_routes.load().get(address_id).cloned()
    }

    /// Every declared Serve entry, being removed or not, for the Serve
    /// manager. Not gated on the lease: a control plane out of reach is no
    /// reason to change tailscaled's config, and the sockets refuse on their
    /// own once the lease lapses.
    pub fn serve_entries(&self) -> Vec<ServeEntry> {
        let mut entries = self.public_ingress.read().serve_entries.clone();
        entries.sort_by(|a, b| a.address_id.cmp(&b.address_id));
        entries
    }

    /// The private addresses currently served, for the inside checks.
    pub fn private_routes(&self) -> Vec<PrivateRouteEntry> {
        if !self.public_enabled.load(Ordering::Acquire) || !self.public_ingress_authorized() {
            return Vec::new();
        }
        let mut routes: Vec<PrivateRouteEntry> =
            self.private_routes.load().values().cloned().collect();
        routes.sort_by(|a, b| a.address_id.cmp(&b.address_id));
        routes
    }

    pub fn lookup_public_certificate(
        &self,
        host: &str,
    ) -> Option<(String, PublicIngressCertBundle)> {
        if !self.public_ingress_authorized() || !self.public_enabled.load(Ordering::Acquire) {
            return None;
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let certificates = self.public_certificates.read();
        let (ephemeral_key, bundles) = certificates.as_ref()?;
        let bundle = bundles.get(&host).or_else(|| {
            host.find('.')
                .and_then(|dot| bundles.get(&format!("*.{}", &host[dot + 1..])))
        })?;
        Some((ephemeral_key.clone(), bundle.clone()))
    }

    /// Install the enrollment key used to decrypt certificate bundles and
    /// eagerly prepare the current snapshot. TLS handshakes only clone an
    /// already validated signing key and never decrypt or parse PEM data.
    pub fn configure_public_tls(&self, private_key_b64: String) {
        *self.public_tls_private_key.write() = Some(private_key_b64);
        self.rebuild_public_tls_keys(&self.public_ingress.read());
    }

    pub fn lookup_public_tls_key(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        if !self.public_ingress_authorized() || !self.public_enabled.load(Ordering::Acquire) {
            return None;
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let keys = self.public_tls_keys.load();
        keys.get(&host)
            .or_else(|| {
                host.find('.')
                    .and_then(|dot| keys.get(&format!("*.{}", &host[dot + 1..])))
            })
            .cloned()
    }

    fn persist_current(&self) {
        let snapshot = DiskSnapshot {
            generation: *self.generation.read(),
            routes: self.inner.read().values().cloned().collect(),
            public_ingress: self.public_ingress.read().clone(),
            public_ingress_expires_at: self.public_ingress_expires_at.load(Ordering::Acquire),
        };
        if let Err(error) = self.persist(&snapshot) {
            warn!(error = %error, path = %self.snapshot_path.display(), "failed to persist route snapshot");
        }
    }

    /// Look up a host. Returns the cloned entry on hit. Case-insensitive.
    pub fn lookup(&self, host: &str) -> Option<RouteEntry> {
        let key = host.to_ascii_lowercase();
        self.inner.read().get(&key).cloned()
    }

    /// Return whether a Docker container belongs to `project_id` according to
    /// the control-plane route snapshot. Container IDs are trusted workload
    /// identity; backend addresses are destinations and may be hostnames or
    /// shared node IPs, so they must never be used as caller identity.
    pub fn container_id_has_project(&self, container_id: &str, project_id: i32) -> bool {
        self.container_projects
            .read()
            .get(container_id)
            .is_some_and(|projects| projects.contains(&project_id))
    }

    pub fn current_generation(&self) -> u64 {
        *self.generation.read()
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Cold-start path. Reads `<snapshot_dir>/routes.json` if present.
    /// Silent no-op on missing/unparsable file — the store stays
    /// empty and the proxy returns 503 until the first sync round.
    pub fn load_from_disk(&self) {
        let data = match std::fs::read_to_string(&self.snapshot_path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %self.snapshot_path.display(),
                    "failed to read route snapshot from disk"
                );
                return;
            }
        };
        let snap: DiskSnapshot = match serde_json::from_str(&data) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %self.snapshot_path.display(),
                    "route snapshot on disk is unparsable; starting empty"
                );
                return;
            }
        };
        let mut map = HashMap::with_capacity(snap.routes.len());
        let container_projects = container_project_index(&snap.routes);
        for r in &snap.routes {
            map.insert(r.host.to_ascii_lowercase(), r.clone());
        }
        *self.inner.write() = map;
        *self.container_projects.write() = container_projects;
        *self.generation.write() = snap.generation;
        let now = chrono::Utc::now().timestamp();
        let valid_expiry = snap.public_ingress_expires_at > now
            && snap.public_ingress_expires_at <= now.saturating_add(300);
        let mut public_ingress = PublicIngressSnapshot {
            enabled: snap.public_ingress.enabled && valid_expiry,
            ..snap.public_ingress
        };
        public_ingress.separate_gated();
        public_ingress.separate_tailnet_users();
        public_ingress.separate_shielded();
        *self.public_ingress.write() = public_ingress;
        self.public_enabled
            .store(self.public_ingress.read().enabled, Ordering::Release);
        self.rebuild_public_indexes(&self.public_ingress.read());
        self.rebuild_public_tls_keys(&self.public_ingress.read());
        self.public_ingress_expires_at.store(
            if valid_expiry {
                snap.public_ingress_expires_at
            } else {
                0
            },
            Ordering::Release,
        );
        debug!(
            generation = snap.generation,
            entries = self.inner.read().len(),
            path = %self.snapshot_path.display(),
            "loaded route snapshot from disk"
        );
    }

    fn persist(&self, snap: &DiskSnapshot) -> std::io::Result<()> {
        if let Some(parent) = self.snapshot_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file then atomic-rename so a crashed process
        // doesn't leave a half-written snapshot that confuses the next
        // boot.
        let tmp = self.snapshot_path.with_extension("json.tmp");
        let json = serde_json::to_string(snap)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.snapshot_path)?;
        Ok(())
    }

    fn public_ingress_authorized(&self) -> bool {
        let expiry = self.public_ingress_expires_at.load(Ordering::Acquire);
        expiry > 0 && chrono::Utc::now().timestamp() <= expiry
    }

    fn rebuild_public_indexes(&self, snapshot: &PublicIngressSnapshot) {
        let mut serve_routes: HashMap<String, ServeEntry> = snapshot
            .serve_entries
            .iter()
            .filter(|entry| !entry.removing && entry.route.is_some())
            .map(|entry| (entry.address_id.clone(), entry.clone()))
            .collect();
        for identity in &snapshot.identity_serve_routes {
            let declared = snapshot
                .serve_entries
                .iter()
                .find(|entry| entry.address_id == identity.address_id && !entry.removing);
            if let Some(entry) = declared {
                serve_routes.insert(
                    identity.address_id.clone(),
                    ServeEntry {
                        route: Some(identity.route.clone()),
                        principals: Some(identity.principals.clone()),
                        ..entry.clone()
                    },
                );
            }
        }
        self.serve_routes.store(Arc::new(serve_routes));
        self.private_routes.store(Arc::new(
            snapshot
                .private_routes
                .iter()
                .chain(&snapshot.identity_private_routes)
                .map(|private| {
                    (
                        private
                            .route
                            .host
                            .trim_end_matches('.')
                            .to_ascii_lowercase(),
                        private.clone(),
                    )
                })
                .collect(),
        ));
        // A signed-in route comes after an open one, and a Shield route
        // last, so the stricter one wins for the same host. Shield settings
        // go to the Shield index, not in the entry a request clones.
        let mut policies = HashMap::new();
        let mut public_routes = HashMap::new();
        for route in snapshot
            .routes
            .iter()
            .chain(&snapshot.gated_routes)
            .chain(&snapshot.shield_routes)
        {
            let host = route.host.trim_end_matches('.').to_ascii_lowercase();
            match &route.shield {
                Some(policy) => {
                    policies.insert(host.clone(), crate::shield::env_policy(policy));
                }
                None => {
                    policies.remove(&host);
                }
            }
            public_routes.insert(
                host,
                RouteEntry {
                    shield: None,
                    ..route.clone()
                },
            );
        }
        // Settings before routes: a route that just gained Shield is never
        // found without them.
        self.shield.set_routes(policies, snapshot.shield);
        self.public_routes.store(Arc::new(public_routes));
        *self.public_certificates.write() = snapshot.certificates.as_ref().map(|certificates| {
            (
                certificates.ephemeral_public_key.clone(),
                certificates
                    .bundles
                    .iter()
                    .map(|bundle| {
                        (
                            bundle.domain.trim_end_matches('.').to_ascii_lowercase(),
                            bundle.clone(),
                        )
                    })
                    .collect(),
            )
        });
    }

    fn rebuild_public_tls_keys(&self, snapshot: &PublicIngressSnapshot) {
        let Some(private_key_b64) = self.public_tls_private_key.read().clone() else {
            self.public_tls_keys.store(Arc::new(HashMap::new()));
            return;
        };
        let Some(certificates) = snapshot.certificates.as_ref() else {
            self.public_tls_keys.store(Arc::new(HashMap::new()));
            return;
        };
        let mut prepared = HashMap::with_capacity(certificates.bundles.len());
        for bundle in &certificates.bundles {
            let encrypted = temps_core::ecies::EncryptedBundle {
                ciphertext: bundle.ciphertext.clone(),
                nonce: bundle.nonce.clone(),
            };
            let key = decrypt_certified_key(
                &private_key_b64,
                &certificates.ephemeral_public_key,
                bundle,
                &encrypted,
            );
            match key {
                Ok(key) => {
                    prepared.insert(
                        bundle.domain.trim_end_matches('.').to_ascii_lowercase(),
                        Arc::new(key),
                    );
                }
                Err(error) => {
                    warn!(domain = %bundle.domain, %error, "rejected public ingress certificate bundle")
                }
            }
        }
        self.public_tls_keys.store(Arc::new(prepared));
    }
}

fn decrypt_certified_key(
    private_key_b64: &str,
    ephemeral_public_key: &str,
    bundle: &PublicIngressCertBundle,
    encrypted: &temps_core::ecies::EncryptedBundle,
) -> Result<CertifiedKey, String> {
    let plaintext =
        temps_core::ecies::decrypt_bundle(private_key_b64, ephemeral_public_key, encrypted)
            .map_err(|error| error.to_string())?;
    let payload = std::str::from_utf8(&plaintext).map_err(|error| error.to_string())?;
    let key_marker = [
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
    ]
    .iter()
    .filter_map(|marker| payload.find(marker))
    .min()
    .ok_or_else(|| "certificate payload contains no private key".to_string())?;
    // The control plane builds the encrypted payload as
    // `{certificate}\n{private_key}` and fingerprints the certificate string
    // exactly as stored. Remove only that one separator newline. Trimming the
    // slice also removed meaningful trailing PEM whitespace and made the
    // worker compute a different fingerprint from the control plane.
    let certificate_pem = certificate_pem_from_payload(payload, key_marker)?;
    if temps_core::ecies::cert_fingerprint(certificate_pem) != bundle.fingerprint {
        return Err("certificate fingerprint does not match snapshot".to_string());
    }
    let (certificates, private_key) = parse_public_certificate_pem(&plaintext)?;
    if !certificate_covers_domain(
        certificates
            .first()
            .ok_or_else(|| "certificate chain is empty".to_string())?,
        &bundle.domain,
    ) {
        return Err("certificate SAN does not cover snapshot domain".to_string());
    }
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&private_key)
        .map_err(|error| error.to_string())?;
    let certified = CertifiedKey::new(certificates, signing_key);
    certified.keys_match().map_err(|error| error.to_string())?;
    Ok(certified)
}

fn certificate_pem_from_payload(payload: &str, key_marker: usize) -> Result<&str, String> {
    payload[..key_marker]
        .strip_suffix('\n')
        .ok_or_else(|| "certificate payload separator is missing".to_string())
}

fn parse_public_certificate_pem(
    payload: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let mut reader = BufReader::new(payload);
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(payload);
    let private_key = rustls_pemfile::private_key(&mut reader)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "certificate payload contains no private key".to_string())?;
    if certificates.is_empty() {
        return Err("certificate payload contains no certificate".to_string());
    }
    Ok((certificates, private_key))
}

fn certificate_covers_domain(certificate: &CertificateDer<'_>, expected: &str) -> bool {
    use x509_parser::extensions::GeneralName;
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(certificate.as_ref()) else {
        return false;
    };
    let Ok(Some(san)) = parsed.subject_alternative_name() else {
        return false;
    };
    san.value.general_names.len() == 1
        && san.value.general_names.iter().all(|name| match name {
            GeneralName::DNSName(name) => {
                !name.starts_with("*.") && name.eq_ignore_ascii_case(expected)
            }
            _ => false,
        })
}

fn container_project_index(routes: &[RouteEntry]) -> HashMap<String, HashSet<i32>> {
    let mut index: HashMap<String, HashSet<i32>> = HashMap::new();
    for route in routes {
        if let Some(project_id) = route.project_id {
            for backend in &route.backends {
                if let Some(container_id) = backend.container_id.as_deref() {
                    index
                        .entry(container_id.to_string())
                        .or_default()
                        .insert(project_id);
                }
            }
        }
    }
    index
}

pub type SharedRouteStore = Arc<RouteStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(host: &str, addr: &str) -> RouteEntry {
        entry_with_project(host, addr, Some(1))
    }

    fn entry_with_project(host: &str, addr: &str, project_id: Option<i32>) -> RouteEntry {
        RouteEntry {
            host: host.into(),
            backends: vec![RouteBackend {
                address: addr.into(),
                container_id: None,
                container_name: None,
            }],
            deployment_id: Some(1),
            project_id,
            environment_id: Some(1),
            gate: None,
            shield: None,
        }
    }

    #[test]
    fn apply_and_lookup() {
        let dir = TempDir::new().unwrap();
        let store = RouteStore::new(dir.path().join("routes.json"));
        store.apply_snapshot(5, vec![entry("PROD.foo.temps.local", "10.0.0.1:80")]);
        assert_eq!(store.current_generation(), 5);
        assert!(store.lookup("prod.foo.temps.local").is_some());
        // Case-insensitive match.
        assert!(store.lookup("PROD.FOO.TEMPS.LOCAL").is_some());
        assert!(store.lookup("missing.temps.local").is_none());
    }

    #[test]
    fn snapshot_persists_and_reloads() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("routes.json");

        let s1 = RouteStore::new(path.clone());
        s1.apply_snapshot(7, vec![entry("a.temps.local", "10.0.0.1:80")]);

        let s2 = RouteStore::new(path);
        s2.load_from_disk();
        assert_eq!(s2.current_generation(), 7);
        assert!(s2.lookup("a.temps.local").is_some());
    }

    #[test]
    fn project_ids_are_indexed_by_trusted_container_id() {
        let dir = TempDir::new().unwrap();
        let store = RouteStore::new(dir.path().join("routes.json"));
        let mut alpha = entry_with_project("prod.alpha.temps.local", "alpha:3000", Some(10));
        alpha.backends[0].container_id = Some("container-alpha".to_string());
        let mut beta = entry_with_project("prod.beta.temps.local", "10.0.0.2:8080", Some(20));
        beta.backends[0].container_id = Some("container-beta".to_string());
        store.apply_snapshot(1, vec![alpha, beta]);

        assert!(store.container_id_has_project("container-alpha", 10));
        assert!(!store.container_id_has_project("container-alpha", 20));
        assert!(!store.container_id_has_project("unknown", 10));
    }

    /// ADR 065 D7: a signed-in route given in `routes` is moved out of it,
    /// so `routes.json` never holds one where an agent that predates the
    /// gate reads. That agent, reading the file after a rollback, finds no
    /// signed-in host; this one still serves it, with its gate.
    #[test]
    fn routes_json_holds_no_signed_in_route_where_an_older_agent_reads() {
        #[derive(serde::Deserialize)]
        struct OldDisk {
            public_ingress: OldPublicIngress,
        }
        #[derive(serde::Deserialize)]
        struct OldPublicIngress {
            routes: Vec<OldEntry>,
            #[serde(default)]
            private_routes: Vec<OldPrivate>,
        }
        #[derive(serde::Deserialize)]
        struct OldEntry {
            host: String,
        }
        #[derive(serde::Deserialize)]
        struct OldPrivate {
            route: OldEntry,
        }

        let gated = |host: &str| RouteEntry {
            gate: Some(temps_core::gate::GatePolicy {
                address_id: "custom:1".into(),
                gate: temps_core::gate::GateKind::Temps,
                temps: None,
                access: None,
            }),
            ..entry(host, "10.0.0.1:80")
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("routes.json");
        let store = RouteStore::new(path.clone());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            routes: vec![
                entry("open.example.test", "10.0.0.1:80"),
                gated("team.example.test"),
            ],
            gated_routes: vec![
                gated("access.example.test"),
                // Names no gate: never served from here.
                entry("nogate.example.test", "10.0.0.1:80"),
            ],
            ..PublicIngressSnapshot::default()
        });
        store.apply_snapshot(1, Vec::new());

        // This agent serves them, each behind its gate.
        assert!(store
            .lookup_public("team.example.test")
            .unwrap()
            .gate
            .is_some());
        assert!(store
            .lookup_public("access.example.test")
            .unwrap()
            .gate
            .is_some());
        assert!(store.lookup_public("nogate.example.test").is_none());
        assert!(store
            .lookup_public("open.example.test")
            .unwrap()
            .gate
            .is_none());

        // The agent that predates the gate, reading the same file, serves
        // none of them.
        let old: OldDisk = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hosts: Vec<&str> = old
            .public_ingress
            .routes
            .iter()
            .chain(old.public_ingress.private_routes.iter().map(|p| &p.route))
            .map(|r| r.host.as_str())
            .collect();
        assert_eq!(hosts, vec!["open.example.test"]);

        // And this agent, restarted on that file, still gates them.
        let reloaded = RouteStore::new(path);
        reloaded.load_from_disk();
        assert!(reloaded
            .lookup_public("team.example.test")
            .unwrap()
            .gate
            .is_some());
        assert!(reloaded.lookup_public("nogate.example.test").is_none());
    }

    /// ADR 066 D10: a route whose environment acts on verdicts is never in
    /// the fields an older agent reads, wherever the snapshot put it, and
    /// one in `shield_routes` without settings is not served. This agent
    /// serves each with its settings, after a restart too, and keeps the
    /// settings out of the entry a request clones.
    #[test]
    fn routes_json_holds_no_shield_route_where_an_older_agent_reads() {
        #[derive(serde::Deserialize)]
        struct OldDisk {
            public_ingress: OldPublicIngress,
        }
        #[derive(serde::Deserialize)]
        struct OldPublicIngress {
            routes: Vec<OldEntry>,
            #[serde(default)]
            gated_routes: Vec<OldEntry>,
        }
        #[derive(serde::Deserialize)]
        struct OldEntry {
            host: String,
        }

        let shielded = |host: &str, mode: &str| RouteEntry {
            shield: Some(temps_core::worker_shield::ShieldRoutePolicy {
                project_id: 1,
                environment_id: 2,
                mode: mode.to_string(),
                exempt_paths: Vec::new(),
                bypass_token_hashes: Vec::new(),
            }),
            ..entry(host, "10.0.0.1:80")
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("routes.json");
        let store = RouteStore::new(path.clone());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            routes: vec![
                entry("open.example.test", "10.0.0.1:80"),
                shielded("logged.example.test", "log"),
                // In the wrong field: moved, never served open.
                shielded("misplaced.example.test", "attack"),
                // A mode this agent does not know is taken as enforcing.
                shielded("future.example.test", "paranoid"),
            ],
            shield_routes: vec![
                shielded("selective.example.test", "selective"),
                entry("nosettings.example.test", "10.0.0.1:80"),
            ],
            shield: Some(temps_core::worker_shield::ShieldRef { revision: 9 }),
            ..PublicIngressSnapshot::default()
        });
        store.apply_snapshot(1, Vec::new());

        let check = |store: &RouteStore| {
            for (host, mode) in [
                ("logged.example.test", temps_shield::ShieldMode::Log),
                ("misplaced.example.test", temps_shield::ShieldMode::Attack),
                ("future.example.test", temps_shield::ShieldMode::Selective),
                (
                    "selective.example.test",
                    temps_shield::ShieldMode::Selective,
                ),
            ] {
                let route = store.lookup_public(host).expect(host);
                assert!(route.shield.is_none(), "{host} clones no settings");
                assert_eq!(store.shield().policy(host).expect(host).mode, mode);
            }
            assert!(store.lookup_public("open.example.test").is_some());
            assert!(store.shield().policy("open.example.test").is_none());
            assert!(store.lookup_public("nosettings.example.test").is_none());
        };
        check(&store);
        assert_eq!(
            *store.shield().subscribe().borrow(),
            Some(temps_core::worker_shield::ShieldRef { revision: 9 })
        );

        // The agent that predates Shield on workers, reading the same file,
        // serves only what it can serve as before.
        let old: OldDisk = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hosts: Vec<&str> = old
            .public_ingress
            .routes
            .iter()
            .chain(&old.public_ingress.gated_routes)
            .map(|r| r.host.as_str())
            .collect();
        assert_eq!(hosts, vec!["open.example.test", "logged.example.test"]);

        let reloaded = RouteStore::new(path);
        reloaded.load_from_disk();
        check(&reloaded);
    }

    /// ADR 065 D6: a `tailnet_users` route given in the fields every agent
    /// reads is moved out of them, so `routes.json` never holds one there.
    /// The 3b agent's own types, reading that file after a rollback, find
    /// no restricted route; this agent still serves it, with its list.
    #[test]
    fn routes_json_holds_no_restricted_route_where_an_older_agent_reads() {
        #[derive(serde::Deserialize)]
        struct OldDisk {
            public_ingress: OldPublicIngress,
        }
        #[derive(serde::Deserialize)]
        struct OldPublicIngress {
            #[serde(default)]
            private_routes: Vec<OldPrivateRoute>,
            #[serde(default)]
            serve_entries: Vec<OldServeEntry>,
        }
        #[derive(serde::Deserialize)]
        struct OldPrivateRoute {
            route: RouteEntry,
        }
        #[derive(serde::Deserialize)]
        struct OldServeEntry {
            address_id: String,
            #[serde(default)]
            route: Option<RouteEntry>,
        }

        let alice =
            vec![temps_core::tailnet::TailnetPrincipal::parse("alice@example.com").unwrap()];
        let private = |host: &str, id: &str, principals| PrivateRouteEntry {
            route: entry(host, "10.0.0.1:80"),
            address_id: id.to_string(),
            range: "100.64.0.0/10".to_string(),
            listener: "100.64.0.3".to_string(),
            principals,
        };
        let serve = |id: &str, host: &str, principals| ServeEntry {
            entry_id: 1,
            address_id: id.to_string(),
            hostname: host.to_string(),
            via: "tailscale_serve".to_string(),
            service: None,
            removing: false,
            applied_etag: None,
            route: Some(entry(host, "10.0.0.1:80")),
            addrs: Vec::new(),
            principals,
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("routes.json");
        let store = RouteStore::new(path.clone());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            private_routes: vec![
                private("range.example.test", "custom:1", None),
                private("users.example.test", "custom:2", Some(alice.clone())),
            ],
            serve_entries: vec![serve(
                "custom:3",
                "users.example.ts.net",
                Some(alice.clone()),
            )],
            identity_private_routes: vec![private("nolist.example.test", "custom:4", None)],
            ..PublicIngressSnapshot::default()
        });
        store.apply_snapshot(1, Vec::new());

        // This agent serves them, each with who it admits.
        assert_eq!(
            store
                .lookup_private("users.example.test")
                .unwrap()
                .principals,
            Some(alice.clone())
        );
        assert_eq!(
            store
                .lookup_private("nolist.example.test")
                .unwrap()
                .principals,
            Some(Vec::new()),
            "an identity route without a list admits nobody"
        );
        assert_eq!(
            store
                .lookup_private("range.example.test")
                .unwrap()
                .principals,
            None
        );
        let socket = store.lookup_serve("custom:3").expect("served here");
        assert_eq!(socket.principals, Some(alice.clone()));
        assert!(socket.route.is_some());

        // The 3b agent, reading the same file, serves none of them.
        let old: OldDisk = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hosts: Vec<String> = old
            .public_ingress
            .private_routes
            .iter()
            .map(|r| r.route.host.clone())
            .collect();
        assert_eq!(hosts, vec!["range.example.test".to_string()]);
        assert_eq!(old.public_ingress.serve_entries[0].address_id, "custom:3");
        assert!(old.public_ingress.serve_entries[0].route.is_none());

        // And this agent, restarted on that file, still restricts them.
        let reloaded = RouteStore::new(path);
        reloaded.load_from_disk();
        assert_eq!(
            reloaded
                .lookup_private("users.example.test")
                .unwrap()
                .principals,
            Some(alice.clone())
        );
        assert_eq!(
            reloaded.lookup_serve("custom:3").unwrap().principals,
            Some(alice)
        );
    }

    #[test]
    fn missing_disk_is_silent() {
        let dir = TempDir::new().unwrap();
        let store = RouteStore::new(dir.path().join("nope.json"));
        store.load_from_disk();
        assert_eq!(store.current_generation(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn public_indexes_normalize_hosts_and_reject_unprepared_certificates() {
        let dir = TempDir::new().unwrap();
        let store = RouteStore::new(dir.path().join("routes.json"));
        store.configure_public_tls("invalid enrollment key".to_string());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            routes: vec![entry("Example.COM.", "10.0.0.1:80")],
            certificates: Some(PublicIngressCertificates {
                ephemeral_public_key: "invalid ephemeral key".to_string(),
                bundles: vec![PublicIngressCertBundle {
                    domain: "EXAMPLE.COM.".to_string(),
                    ciphertext: "invalid ciphertext".to_string(),
                    nonce: "invalid nonce".to_string(),
                    fingerprint: "invalid fingerprint".to_string(),
                }],
            }),
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
            listen: None,
            private_routes: Vec::new(),
            gated_routes: Vec::new(),
            serve_entries: Vec::new(),
            identity_private_routes: Vec::new(),
            identity_serve_routes: Vec::new(),
            shield_routes: Vec::new(),
            shield: None,
        });

        assert!(store.lookup_public("example.com").is_some());
        assert!(store.lookup_public_tls_key("example.com").is_none());
    }

    #[test]
    fn certificate_bundle_preserves_stored_pem_trailing_newline_for_fingerprint() {
        const PRIVATE_KEY_B64: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
        const PUBLIC_KEY_B64: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let leaf = temps_core::node_pki::generate_node_keypair_csr(
            "app.example.test",
            &["app.example.test".to_string()],
        )
        .expect("generate test leaf CSR");
        let signed = temps_core::node_pki::sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &leaf.csr_pem,
            &["app.example.test".to_string()],
        )
        .expect("sign test leaf certificate");
        let certificate = format!("{}{}", signed.cert_pem, ca.cert_pem);
        let payload = format!("{certificate}\n{}", leaf.key_pem);
        let session = temps_core::ecies::EncryptionSession::new(PUBLIC_KEY_B64)
            .expect("create encryption session");
        let encrypted = session.encrypt(payload.as_bytes()).expect("encrypt bundle");
        let bundle = PublicIngressCertBundle {
            domain: "app.example.test".to_string(),
            ciphertext: encrypted.ciphertext.clone(),
            nonce: encrypted.nonce.clone(),
            fingerprint: temps_core::ecies::cert_fingerprint(&certificate),
        };

        let certified_key = decrypt_certified_key(
            PRIVATE_KEY_B64,
            session.ephemeral_public_key(),
            &bundle,
            &encrypted,
        );

        assert!(
            certified_key.is_ok(),
            "leaf and CA chain must decrypt and parse"
        );
    }

    #[test]
    fn certificate_bundle_rejects_missing_payload_separator() {
        const PRIVATE_KEY_B64: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
        const PUBLIC_KEY_B64: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let payload = format!("{}{}", ca.cert_pem.trim_end(), ca.key_pem);
        let session = temps_core::ecies::EncryptionSession::new(PUBLIC_KEY_B64)
            .expect("create encryption session");
        let encrypted = session.encrypt(payload.as_bytes()).expect("encrypt bundle");
        let bundle = PublicIngressCertBundle {
            domain: "app.example.test".to_string(),
            ciphertext: encrypted.ciphertext.clone(),
            nonce: encrypted.nonce.clone(),
            fingerprint: temps_core::ecies::cert_fingerprint(ca.cert_pem.trim_end()),
        };

        let error = decrypt_certified_key(
            PRIVATE_KEY_B64,
            session.ephemeral_public_key(),
            &bundle,
            &encrypted,
        )
        .expect_err("payload without framing separator must fail");

        assert_eq!(error, "certificate payload separator is missing");
    }
}
