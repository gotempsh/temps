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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteBackend {
    pub address: String,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
}

/// One internal-zone route. `host` is the lower-cased FQDN the proxy
/// matches `Host:` against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub host: String,
    pub backends: Vec<RouteBackend>,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
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
    public_certificates: RwLock<Option<(String, HashMap<String, PublicIngressCertBundle>)>>,
    public_tls_private_key: RwLock<Option<String>>,
    public_tls_keys: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
    public_ingress_expires_at: AtomicI64,
    snapshot_path: PathBuf,
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
            public_certificates: RwLock::new(None),
            public_tls_private_key: RwLock::new(None),
            public_tls_keys: ArcSwap::from_pointee(HashMap::new()),
            public_ingress_expires_at: AtomicI64::new(0),
            snapshot_path,
        }
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
    pub fn apply_public_snapshot(&self, snapshot: PublicIngressSnapshot) {
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
        *self.public_ingress.write() = PublicIngressSnapshot {
            enabled: snap.public_ingress.enabled && valid_expiry,
            ..snap.public_ingress
        };
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
        self.public_routes.store(Arc::new(
            snapshot
                .routes
                .iter()
                .map(|route| {
                    (
                        route.host.trim_end_matches('.').to_ascii_lowercase(),
                        route.clone(),
                    )
                })
                .collect(),
        ));
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
