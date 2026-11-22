// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The reputation index this ingress checks every request against (ADR 066,
//! I3 and I4).
//!
//! It starts as the built-in baseline. A loader reads the newest row of
//! `shield_snapshots` every [`POLL_INTERVAL`], one indexed read that returns
//! nothing unless a newer revision exists, and swaps the decoded index in.
//! A body that fails its checksum or does not decode is skipped and the
//! last good index stays: the lists only ever get older, never emptier.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder};
use sha2::{Digest, Sha256};
use temps_entities::shield_snapshots;
use temps_shield::{Index, ShieldSnapshot, SnapshotError, Verdict};
use tracing::{debug, error, info, warn};

pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ShieldListsError {
    #[error("could not read shield_snapshots: {0}")]
    Database(#[from] DbErr),
    #[error("snapshot {revision} does not match its checksum")]
    Checksum { revision: i64 },
    #[error("snapshot {revision} could not be read: {source}")]
    Snapshot {
        revision: i64,
        #[source]
        source: SnapshotError,
    },
    #[error("snapshot {revision} could not be indexed: {source}")]
    Indexing {
        revision: i64,
        #[source]
        source: tokio::task::JoinError,
    },
}

impl ShieldListsError {
    /// The revision at fault, when the row itself is bad rather than the
    /// read.
    fn bad_revision(&self) -> Option<i64> {
        match self {
            Self::Database(_) => None,
            Self::Checksum { revision }
            | Self::Snapshot { revision, .. }
            | Self::Indexing { revision, .. } => Some(*revision),
        }
    }
}

pub struct ShieldLists {
    index: ArcSwap<Index>,
}

impl Default for ShieldLists {
    fn default() -> Self {
        Self {
            index: ArcSwap::from_pointee(Index::baseline()),
        }
    }
}

impl ShieldLists {
    /// What the lists say about `ip`. Lock-free, and allocates only when a
    /// list holds it.
    pub fn evaluate(&self, ip: IpAddr, now: i64) -> Verdict {
        self.index.load().evaluate(ip, now)
    }

    pub fn revision(&self) -> u64 {
        self.index.load().revision()
    }

    /// The loaded index, for [`temps_shield::ShieldEngine::decide`].
    /// Lock-free.
    pub fn index(&self) -> arc_swap::Guard<Arc<Index>> {
        self.index.load()
    }

    /// Start polling `shield_snapshots` on `handle`'s runtime.
    pub fn spawn_loader(
        self: &Arc<Self>,
        db: Arc<DatabaseConnection>,
        handle: &tokio::runtime::Handle,
    ) -> tokio::task::JoinHandle<()> {
        handle.spawn(Arc::clone(self).run_loader(db))
    }

    async fn run_loader(self: Arc<Self>, db: Arc<DatabaseConnection>) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Revisions that failed their checks. At most the retained ones
        // above the loaded revision, so this stays as small as the table.
        let mut skipped: Vec<i64> = Vec::new();
        let mut consecutive_failures: u64 = 0;
        loop {
            ticker.tick().await;
            match self.refresh(&db, &skipped).await {
                Ok(_) => {
                    consecutive_failures = 0;
                    let loaded = i64::try_from(self.revision()).unwrap_or(i64::MAX);
                    skipped.retain(|&revision| revision > loaded);
                }
                Err(ShieldListsError::Database(e)) if is_missing_table(&e) => {
                    debug!("shield_snapshots does not exist yet; keeping the built-in lists");
                }
                Err(e) => {
                    if let Some(revision) = e.bad_revision() {
                        error!(error = %e, "Skipping a Shield snapshot; keeping revision {}", self.revision());
                        skipped.push(revision);
                        continue;
                    }
                    consecutive_failures += 1;
                    // First failure, then once a minute at the poll rate.
                    if consecutive_failures == 1 || consecutive_failures.is_multiple_of(12) {
                        warn!(
                            failed_polls = consecutive_failures,
                            error = %e,
                            "Shield snapshot poll failed; keeping the loaded lists"
                        );
                    }
                }
            }
        }
    }

    /// Load the newest snapshot that is newer than the loaded one and not
    /// `skipped`, so a bad newest revision falls back to the good one below
    /// it. Returns whether the index changed.
    pub async fn refresh(
        &self,
        db: &DatabaseConnection,
        skipped: &[i64],
    ) -> Result<bool, ShieldListsError> {
        let loaded = i64::try_from(self.revision()).unwrap_or(i64::MAX);
        let Some(row) = shield_snapshots::Entity::find()
            .filter(shield_snapshots::Column::Revision.gt(loaded))
            .filter(shield_snapshots::Column::Revision.is_not_in(skipped.iter().copied()))
            .order_by_desc(shield_snapshots::Column::Revision)
            .one(db)
            .await?
        else {
            return Ok(false);
        };
        let revision = row.revision;
        if hex::encode(Sha256::digest(&row.body)) != row.sha256 {
            return Err(ShieldListsError::Checksum { revision });
        }
        // Decoding and sorting a large list takes milliseconds: off the
        // runtime's worker threads.
        let index = tokio::task::spawn_blocking(move || {
            ShieldSnapshot::decode(&row.body).map(|snapshot| {
                let mut snapshot = snapshot;
                snapshot.revision = u64::try_from(revision).unwrap_or(0);
                Index::new(&snapshot)
            })
        })
        .await
        .map_err(|source| ShieldListsError::Indexing { revision, source })?
        .map_err(|source| ShieldListsError::Snapshot { revision, source })?;
        info!(revision, "Shield lists updated");
        self.index.store(Arc::new(index));
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn from_snapshot(snapshot: &ShieldSnapshot) -> Self {
        Self {
            index: ArcSwap::from_pointee(Index::new(snapshot)),
        }
    }
}

fn is_missing_table(error: &DbErr) -> bool {
    let message = error.to_string();
    message.contains("shield_snapshots") && message.contains("does not exist")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_shield::{Action, ListVerdict, Network, SourceList};

    const NOW: i64 = 1_800_000_000;

    fn snapshot(revision: u64) -> ShieldSnapshot {
        let network = Network::parse("198.18.0.0/15");
        ShieldSnapshot {
            revision,
            generated_at: NOW,
            lists: vec![SourceList::new(
                "drop",
                ListVerdict::Block,
                NOW,
                Some(3600),
                network,
            )],
            ..ShieldSnapshot::default()
        }
    }

    fn row(revision: i64, body: Vec<u8>) -> shield_snapshots::Model {
        shield_snapshots::Model {
            revision,
            created_at: chrono::Utc::now(),
            sha256: hex::encode(Sha256::digest(&body)),
            body,
            sources_updated_at: None,
        }
    }

    #[tokio::test]
    async fn a_newer_snapshot_replaces_the_baseline() {
        let lists = ShieldLists::default();
        let ip: IpAddr = "198.18.0.1".parse().unwrap();
        assert_eq!(lists.evaluate(ip, NOW), Verdict::PASS);

        let body = snapshot(0).encode().unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(4, body)]])
            .append_query_results([Vec::<shield_snapshots::Model>::new()])
            .into_connection();
        assert!(lists.refresh(&db, &[]).await.unwrap());
        assert_eq!(
            lists.revision(),
            4,
            "the row's revision wins over the body's"
        );
        assert_eq!(lists.evaluate(ip, NOW).action, Action::Block);
        // The baseline still applies under a loaded snapshot.
        let reserved: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(lists.evaluate(reserved, NOW).action, Action::Log);
        assert!(!lists.refresh(&db, &[]).await.unwrap());
    }

    #[tokio::test]
    async fn a_corrupt_snapshot_keeps_the_last_good_lists() {
        let lists = ShieldLists::from_snapshot(&snapshot(2));
        let mut tampered = row(3, snapshot(3).encode().unwrap());
        tampered.body[0] ^= 1;
        let mut garbage = row(3, b"not a snapshot".to_vec());
        garbage.sha256 = hex::encode(Sha256::digest(&garbage.body));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![tampered]])
            .append_query_results([vec![garbage]])
            // The query leaves out skipped revisions, so the database
            // answers nothing once 3 is skipped.
            .append_query_results([Vec::<shield_snapshots::Model>::new()])
            .into_connection();
        let error = lists.refresh(&db, &[]).await.unwrap_err();
        assert_eq!(error.bad_revision(), Some(3));
        assert!(matches!(error, ShieldListsError::Checksum { .. }));
        assert!(matches!(
            lists.refresh(&db, &[]).await.unwrap_err(),
            ShieldListsError::Snapshot { .. }
        ));
        assert!(!lists.refresh(&db, &[3]).await.unwrap());
        assert_eq!(lists.revision(), 2);
        let ip: IpAddr = "198.18.0.1".parse().unwrap();
        assert_eq!(lists.evaluate(ip, NOW).action, Action::Block);
    }

    /// The query leaves out skipped revisions, so the good revision below a
    /// bad newest one is loaded rather than nothing.
    #[tokio::test]
    async fn a_bad_newest_snapshot_falls_back_to_the_one_below() {
        let lists = ShieldLists::default();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(3, snapshot(3).encode().unwrap())]])
            .into_connection();
        assert!(lists.refresh(&db, &[4]).await.unwrap());
        assert_eq!(lists.revision(), 3);
        let sql = db.into_transaction_log()[0].statements()[0].sql.clone();
        assert!(sql.contains("NOT IN"), "{sql}");
    }
}
