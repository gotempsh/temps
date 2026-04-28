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
//! Replace-the-whole-map semantics. Each apply allocates a new
//! `HashMap`, populates it, then swaps it into the `Arc<RwLock<…>>`.
//! Lookups take a tiny read lock and clone the matched entry; they
//! never block writers in practice (apply happens once per CP
//! generation bump, lookups happen per request).
//!
//! ## Disk snapshot
//!
//! Written best-effort to `<snapshot_dir>/routes.json` after every
//! successful apply. Disk write failure is logged and ignored — the
//! agent keeps serving from memory. On restart, [`load_from_disk`] is
//! the cold-start path; if the file doesn't exist or is unparsable,
//! the store is empty and the proxy returns 503 until the first sync
//! round finishes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
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

/// On-disk snapshot. Versioned via the wrapping struct so a future
/// schema change (e.g. adding affinity hints) can use `serde`'s
/// `default` rather than a breaking parse failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskSnapshot {
    pub generation: u64,
    pub routes: Vec<RouteEntry>,
}

pub struct RouteStore {
    inner: RwLock<HashMap<String, RouteEntry>>,
    generation: RwLock<u64>,
    snapshot_path: PathBuf,
}

impl RouteStore {
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            generation: RwLock::new(0),
            snapshot_path,
        }
    }

    /// Replace the in-memory store with the given snapshot, persist
    /// to disk best-effort. Returns the new generation.
    pub fn apply_snapshot(&self, generation: u64, routes: Vec<RouteEntry>) -> u64 {
        let mut map = HashMap::with_capacity(routes.len());
        for r in &routes {
            map.insert(r.host.to_ascii_lowercase(), r.clone());
        }
        *self.inner.write() = map;
        *self.generation.write() = generation;

        // Best-effort disk persistence. We tolerate any error here —
        // the in-memory store is already updated and the proxy serves
        // from there.
        let snap = DiskSnapshot { generation, routes };
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

    /// Look up a host. Returns the cloned entry on hit. Case-insensitive.
    pub fn lookup(&self, host: &str) -> Option<RouteEntry> {
        let key = host.to_ascii_lowercase();
        self.inner.read().get(&key).cloned()
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
        for r in &snap.routes {
            map.insert(r.host.to_ascii_lowercase(), r.clone());
        }
        *self.inner.write() = map;
        *self.generation.write() = snap.generation;
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
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.snapshot_path)?;
        Ok(())
    }
}

pub type SharedRouteStore = Arc<RouteStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(host: &str, addr: &str) -> RouteEntry {
        RouteEntry {
            host: host.into(),
            backends: vec![RouteBackend {
                address: addr.into(),
                container_id: None,
                container_name: None,
            }],
            deployment_id: Some(1),
            project_id: Some(1),
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
    fn missing_disk_is_silent() {
        let dir = TempDir::new().unwrap();
        let store = RouteStore::new(dir.path().join("nope.json"));
        store.load_from_disk();
        assert_eq!(store.current_generation(), 0);
        assert!(store.is_empty());
    }
}
