//! In-memory zone state with on-disk snapshot persistence.
//!
//! The store is designed for one writer (the [`SyncClient`] task) and many
//! readers (the Hickory authority handlers, one per request). The reader
//! path is lock-free after a single `RwLock` read — we keep records in an
//! `Arc<Inner>` so the read guard releases as soon as the snapshot is
//! cloned by reference.
//!
//! ## Disk format
//!
//! `zone.json` is a single JSON object: `{ "generation": N, "records": [...] }`.
//! The file is written atomically via `<file>.tmp` + `rename(2)` so a crash
//! mid-write can never produce a half-file.
//!
//! ## Failure model
//!
//! - Disk write fails → log warning, keep serving from memory. The next
//!   successful write overwrites.
//! - Disk read fails on startup → log warning, start with empty zone.
//!   Sync will populate on first successful long-poll.
//! - Parse fails on startup → same as read fail. Old corrupt snapshots
//!   never block startup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot_compat::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::ResolverError;
use crate::record::ZoneRecord;

// We don't actually need parking_lot — std::sync::RwLock works fine for
// this contention pattern (write is rare, reads are short). Wrap the alias
// so call sites don't change if we ever want to swap.
mod parking_lot_compat {
    pub use std::sync::RwLock;
}

/// Wire shape of the on-disk snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    generation: i64,
    records: Vec<ZoneRecord>,
}

#[derive(Debug, Default)]
struct Inner {
    generation: i64,
    records: Vec<ZoneRecord>,
}

#[derive(Debug)]
pub struct ZoneStore {
    inner: RwLock<Arc<Inner>>,
    snapshot_path: PathBuf,
}

impl ZoneStore {
    /// Construct an empty store backed by `snapshot_path`. Does not touch
    /// disk; call [`Self::load_from_disk`] explicitly to hydrate.
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self {
            inner: RwLock::new(Arc::new(Inner::default())),
            snapshot_path,
        }
    }

    /// Try to hydrate from `zone.json`. Missing or unreadable files are
    /// **not** errors — we log and start with an empty zone so the agent
    /// can boot even on a fresh node.
    pub fn load_from_disk(&self) {
        match std::fs::read(&self.snapshot_path) {
            Ok(bytes) => match serde_json::from_slice::<Snapshot>(&bytes) {
                Ok(snap) => {
                    let new_inner = Inner {
                        generation: snap.generation,
                        records: snap.records,
                    };
                    let mut guard = self.inner.write().expect("zone-store rwlock poisoned");
                    *guard = Arc::new(new_inner);
                    info!(
                        path = %self.snapshot_path.display(),
                        generation = guard.generation,
                        records = guard.records.len(),
                        "Loaded DNS zone snapshot"
                    );
                }
                Err(e) => {
                    warn!(
                        path = %self.snapshot_path.display(),
                        error = %e,
                        "Failed to parse zone snapshot — starting with empty zone"
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(
                    path = %self.snapshot_path.display(),
                    "No zone snapshot on disk yet"
                );
            }
            Err(e) => {
                warn!(
                    path = %self.snapshot_path.display(),
                    error = %e,
                    "Failed to read zone snapshot — starting with empty zone"
                );
            }
        }
    }

    /// Replace the entire zone (full snapshot from a `since=0` sync).
    /// Returns the persisted generation.
    pub fn replace(&self, generation: i64, records: Vec<ZoneRecord>) -> i64 {
        let new_inner = Arc::new(Inner {
            generation,
            records,
        });
        {
            let mut guard = self.inner.write().expect("zone-store rwlock poisoned");
            *guard = new_inner;
        }
        self.persist();
        generation
    }

    /// Merge a diff: upsert each record by `id`, drop any with `id` in
    /// `removed_ids`. Bumps the stored generation to `new_generation`.
    pub fn apply_diff(&self, new_generation: i64, upserts: Vec<ZoneRecord>, removed_ids: &[i64]) {
        let mut guard = self.inner.write().expect("zone-store rwlock poisoned");
        let current = guard.as_ref();
        // Build the new record list: keep everything not removed and not
        // shadowed by an upsert, then append upserts.
        let upsert_ids: std::collections::HashSet<i64> = upserts.iter().map(|r| r.id).collect();
        let removed: std::collections::HashSet<i64> = removed_ids.iter().copied().collect();
        let mut new_records: Vec<ZoneRecord> = current
            .records
            .iter()
            .filter(|r| !removed.contains(&r.id) && !upsert_ids.contains(&r.id))
            .cloned()
            .collect();
        new_records.extend(upserts);

        *guard = Arc::new(Inner {
            generation: new_generation,
            records: new_records,
        });
        drop(guard);
        self.persist();
    }

    /// Snapshot read — cheap clone of the `Arc`, no lock held during use.
    pub fn snapshot(&self) -> ZoneSnapshot {
        let guard = self.inner.read().expect("zone-store rwlock poisoned");
        ZoneSnapshot {
            inner: Arc::clone(&guard),
        }
    }

    pub fn generation(&self) -> i64 {
        self.snapshot().generation()
    }

    fn persist(&self) {
        let snap = {
            let guard = self.inner.read().expect("zone-store rwlock poisoned");
            Snapshot {
                generation: guard.generation,
                records: guard.records.clone(),
            }
        };
        if let Err(e) = write_atomic(&self.snapshot_path, &snap) {
            warn!(
                path = %self.snapshot_path.display(),
                error = %e,
                "Failed to persist zone snapshot — in-memory state is still authoritative"
            );
        }
    }
}

/// Borrowed view of the current zone state. Holds an `Arc` so the data is
/// stable even if the store is mutated mid-iteration.
#[derive(Debug, Clone)]
pub struct ZoneSnapshot {
    inner: Arc<Inner>,
}

impl ZoneSnapshot {
    pub fn generation(&self) -> i64 {
        self.inner.generation
    }

    pub fn records(&self) -> &[ZoneRecord] {
        &self.inner.records
    }

    /// Records matching this FQDN (case-insensitive, trailing dot ignored).
    pub fn lookup<'a>(&'a self, fqdn: &str) -> impl Iterator<Item = &'a ZoneRecord> + 'a {
        let needle = normalise(fqdn);
        self.inner
            .records
            .iter()
            .filter(move |r| normalise(&r.fqdn) == needle)
    }
}

fn normalise(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

fn write_atomic(path: &Path, snap: &Snapshot) -> Result<(), ResolverError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ResolverError::SnapshotWrite {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(snap).map_err(|e| ResolverError::SnapshotWrite {
        path: tmp.clone(),
        source: std::io::Error::other(e),
    })?;
    std::fs::write(&tmp, &bytes).map_err(|e| ResolverError::SnapshotWrite {
        path: tmp.clone(),
        source: e,
    })?;
    // Atomic on POSIX; fsync the directory below for durability if we
    // ever need it. Today, "best-effort" persistence is the documented
    // contract — a node restart between write and fsync just loses the
    // most recent generation, which the next sync corrects within ~1s.
    std::fs::rename(&tmp, path).map_err(|e| ResolverError::SnapshotWrite {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rec(id: i64, fqdn: &str, ip: &str, generation: i64) -> ZoneRecord {
        ZoneRecord {
            id,
            fqdn: fqdn.into(),
            record_type: "A".into(),
            target_ip: Some(ip.into()),
            target_port: None,
            ttl: 30,
            owner_kind: "service_member".into(),
            owner_id: id,
            node_id: None,
            generation,
        }
    }

    #[test]
    fn empty_store_serves_no_records() {
        let dir = tempdir().unwrap();
        let store = ZoneStore::new(dir.path().join("zone.json"));
        let snap = store.snapshot();
        assert_eq!(snap.generation(), 0);
        assert_eq!(snap.records().len(), 0);
        assert_eq!(snap.lookup("anything.temps.local").count(), 0);
    }

    #[test]
    fn replace_persists_to_disk_and_reloads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("zone.json");
        let store = ZoneStore::new(path.clone());

        let g = store.replace(
            7,
            vec![
                rec(1, "a.temps.local", "172.20.5.10", 7),
                rec(2, "b.temps.local", "172.20.5.11", 7),
            ],
        );
        assert_eq!(g, 7);
        assert!(path.exists(), "snapshot file must be written");

        let store2 = ZoneStore::new(path);
        store2.load_from_disk();
        let snap = store2.snapshot();
        assert_eq!(snap.generation(), 7);
        assert_eq!(snap.records().len(), 2);
    }

    #[test]
    fn apply_diff_upserts_and_removes() {
        let dir = tempdir().unwrap();
        let store = ZoneStore::new(dir.path().join("zone.json"));
        store.replace(
            10,
            vec![
                rec(1, "a.temps.local", "1.1.1.1", 10),
                rec(2, "b.temps.local", "2.2.2.2", 10),
                rec(3, "c.temps.local", "3.3.3.3", 10),
            ],
        );

        // Update id=2's IP, drop id=1, add id=4.
        store.apply_diff(
            11,
            vec![
                rec(2, "b.temps.local", "2.2.2.99", 11),
                rec(4, "d.temps.local", "4.4.4.4", 11),
            ],
            &[1],
        );
        let snap = store.snapshot();
        assert_eq!(snap.generation(), 11);
        let by_id: std::collections::HashMap<i64, &ZoneRecord> =
            snap.records().iter().map(|r| (r.id, r)).collect();
        assert!(!by_id.contains_key(&1), "id=1 must be removed");
        assert_eq!(by_id[&2].target_ip.as_deref(), Some("2.2.2.99"));
        assert!(by_id.contains_key(&3), "id=3 must remain untouched");
        assert!(by_id.contains_key(&4), "id=4 must be added");
    }

    #[test]
    fn lookup_is_case_insensitive_and_trailing_dot_tolerant() {
        let dir = tempdir().unwrap();
        let store = ZoneStore::new(dir.path().join("zone.json"));
        store.replace(1, vec![rec(1, "Pg-Orders.Temps.Local", "172.20.5.10", 1)]);
        let snap = store.snapshot();
        assert_eq!(snap.lookup("pg-orders.temps.local").count(), 1);
        assert_eq!(snap.lookup("PG-ORDERS.TEMPS.LOCAL.").count(), 1);
    }

    #[test]
    fn load_from_disk_handles_missing_file() {
        let dir = tempdir().unwrap();
        let store = ZoneStore::new(dir.path().join("zone.json"));
        store.load_from_disk(); // must not panic
        assert_eq!(store.generation(), 0);
    }

    #[test]
    fn load_from_disk_handles_corrupt_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("zone.json");
        std::fs::write(&path, b"this is not json").unwrap();
        let store = ZoneStore::new(path);
        store.load_from_disk();
        assert_eq!(store.generation(), 0);
    }
}
