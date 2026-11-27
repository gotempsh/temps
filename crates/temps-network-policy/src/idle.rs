// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keeping a pass cheap when nothing changed.
//!
//! Every node gets its snapshot in every mode, so it knows its workloads
//! and the egress proxy can identify a caller. Building a snapshot reads a
//! dozen tables, which a node in `off` (the default for every install)
//! must not pay every 15 seconds. So for `off`:
//!
//! - the node's mode and the policy revision come from **one query**
//!   ([`NetworkPolicyService::mode_and_revision`]), as cheap as reading the
//!   mode was;
//! - the built snapshot is cached per node ([`CachedSnapshot`]) and reused
//!   while the revision and the other nodes' shared observations are
//!   unchanged. `network_policy_state.revision` is bumped by statement
//!   triggers on every table a snapshot reads (m20261022_000007 and
//!   m20261125_000002), so a change is never missed; the cache is also
//!   rebuilt after [`SNAPSHOT_CACHE_MAX_AGE`] for what no trigger sees
//!   (the host's routes, reports expiring);
//! - the agent sends back the digest of the snapshot it holds, and the
//!   answer to an equal digest is `unchanged`, with no snapshot.
//!
//! `shadow` and `enforce` are not cached: those nodes get a freshly built
//! snapshot every pass, exactly as before.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sea_orm::{DatabaseBackend, FromQueryResult, Statement};
use temps_workload_netpol::{snapshot_digest, NodeMode, PolicySnapshot};

use crate::error::NetworkPolicyError;
use crate::service::{parse_mode, NetworkPolicyService, CONTROL_PLANE_NODE_ID};

/// The longest a cached snapshot is reused, whatever the revision says.
pub const SNAPSHOT_CACHE_MAX_AGE: Duration = Duration::from_secs(300);

/// What a node's snapshot was built from, its digest, and, for the control
/// plane's own node only, the snapshot itself. A worker's snapshot holds
/// every workload in the cluster: keeping one per polling worker would grow
/// as workers × workloads. A worker that holds the current snapshot is
/// answered from the digest alone; one that does not gets it built.
#[derive(Debug, Clone)]
pub(crate) struct CachedSnapshot {
    revision: i64,
    observed: u64,
    built: Instant,
    snapshot: Option<Arc<PolicySnapshot>>,
    digest: String,
}

/// What an `off` node is answered: that it holds the current snapshot, or
/// the snapshot to hold.
#[derive(Debug, Clone)]
pub enum OffSnapshot {
    Held(String),
    Fresh(Arc<PolicySnapshot>, String),
}

pub(crate) type SnapshotCache = std::sync::Mutex<SnapshotEntries>;

/// The cached snapshots, and when the stale ones were last evicted.
#[derive(Debug, Default)]
pub(crate) struct SnapshotEntries {
    entries: std::collections::HashMap<i32, CachedSnapshot>,
    swept: Option<Instant>,
}

#[derive(Debug, FromQueryResult)]
struct ModeAndRevision {
    mode: String,
    revision: i64,
}

const WORKER_MODE_SQL: &str = "\
    SELECT n.network_policy_mode AS mode, \
           COALESCE((SELECT revision FROM network_policy_state WHERE id = 0), 0) AS revision \
    FROM nodes n WHERE n.id = $1";

const CONTROL_PLANE_MODE_SQL: &str = "\
    SELECT c.network_policy_mode AS mode, \
           COALESCE((SELECT revision FROM network_policy_state WHERE id = 0), 0) AS revision \
    FROM control_plane_node_settings c WHERE c.id = 0";

impl NetworkPolicyService {
    /// The node's mode and the policy revision, in one query.
    pub async fn mode_and_revision(
        &self,
        node_id: i32,
    ) -> Result<(NodeMode, i64), NetworkPolicyError> {
        let statement = if node_id == CONTROL_PLANE_NODE_ID {
            Statement::from_string(DatabaseBackend::Postgres, CONTROL_PLANE_MODE_SQL)
        } else {
            Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                WORKER_MODE_SQL,
                [node_id.into()],
            )
        };
        let row = ModeAndRevision::find_by_statement(statement)
            .one(self.db.as_ref())
            .await
            .map_err(NetworkPolicyError::db(
                "read a node's network policy mode and the policy revision",
            ))?;
        match row {
            Some(row) => Ok((parse_mode(node_id, &row.mode)?, row.revision)),
            None if node_id == CONTROL_PLANE_NODE_ID => {
                // The settings row was removed: recreated with the default.
                let mode = self.node_mode(node_id).await?;
                Ok((mode, self.revision().await?))
            }
            None => Err(NetworkPolicyError::NodeNotFound { node_id }),
        }
    }

    /// `node_id`'s snapshot and its digest, rebuilt only when the revision,
    /// the observations shared by other nodes, or [`SNAPSHOT_CACHE_MAX_AGE`]
    /// says so (for a worker, when it does not hold the current one).
    pub async fn cached_snapshot(
        &self,
        node_id: i32,
        revision: i64,
    ) -> Result<(Arc<PolicySnapshot>, String), NetworkPolicyError> {
        match self.off_snapshot(node_id, revision, None).await? {
            OffSnapshot::Fresh(snapshot, digest) => Ok((snapshot, digest)),
            // Never with nothing held; built so the caller gets one.
            OffSnapshot::Held(_) => {
                let snapshot = Arc::new(self.node_snapshot(node_id).await?);
                let digest = snapshot_digest(&snapshot);
                Ok((snapshot, digest))
            }
        }
    }

    /// What `off` node `node_id`, holding the snapshot of digest `held`, is
    /// answered. Built only when nothing cached says whether it is current,
    /// or the node does not hold it and no copy is kept.
    pub async fn off_snapshot(
        &self,
        node_id: i32,
        revision: i64,
        held: Option<&str>,
    ) -> Result<OffSnapshot, NetworkPolicyError> {
        let observed = self.observed_key(node_id);
        {
            let cache = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = cache.entries.get(&node_id).filter(|c| {
                c.revision == revision
                    && c.observed == observed
                    && c.built.elapsed() < SNAPSHOT_CACHE_MAX_AGE
            }) {
                if held == Some(hit.digest.as_str()) {
                    return Ok(OffSnapshot::Held(hit.digest.clone()));
                }
                if let Some(snapshot) = &hit.snapshot {
                    return Ok(OffSnapshot::Fresh(snapshot.clone(), hit.digest.clone()));
                }
            }
        }
        let snapshot = Arc::new(self.node_snapshot(node_id).await?);
        let digest = snapshot_digest(&snapshot);
        let mut cache = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        store(
            &mut cache,
            node_id,
            CachedSnapshot {
                revision,
                observed,
                built: Instant::now(),
                snapshot: (node_id == CONTROL_PLANE_NODE_ID).then(|| snapshot.clone()),
                digest: digest.clone(),
            },
        );
        Ok(if held == Some(digest.as_str()) {
            OffSnapshot::Held(digest)
        } else {
            OffSnapshot::Fresh(snapshot, digest)
        })
    }

    /// A digest of what other nodes share with `node_id` (their live
    /// observations outside `off`), which is part of its snapshot but
    /// not of the database revision. In memory.
    ///
    /// Built from each report's digest, taken when it was recorded: a poll
    /// copies and serializes no other node's map.
    fn observed_key(&self, node_id: i32) -> u64 {
        let mut elsewhere = self.observed_digests_elsewhere(node_id);
        elsewhere.sort_unstable();
        let mut hasher = DefaultHasher::new();
        elsewhere.hash(&mut hasher);
        hasher.finish()
    }
}

/// Caches `entry` for `node_id`, evicting the entries too old to be reused
/// at most once per [`SNAPSHOT_CACHE_MAX_AGE`]: a node that was removed, or
/// stopped asking, holds no memory past twice that age, so the cache is
/// bounded by the nodes that ask. Sweeping on every store would make a
/// revision change, which every node rebuilds after, cost a scan per node.
fn store(cache: &mut SnapshotEntries, node_id: i32, entry: CachedSnapshot) {
    let now = Instant::now();
    if cache
        .swept
        .is_none_or(|swept| now.duration_since(swept) >= SNAPSHOT_CACHE_MAX_AGE)
    {
        cache
            .entries
            .retain(|_, cached| now.duration_since(cached.built) < SNAPSHOT_CACHE_MAX_AGE);
        cache.swept = Some(now);
    }
    cache.entries.insert(node_id, entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_test_utils::TestDatabase;
    use temps_workload::{WorkloadKind, WorkloadRef};
    use temps_workload_netpol::{IdentityMap, NodePolicy, NodePolicyReport, POLICY_PROTOCOL};

    fn cached(built: Instant) -> CachedSnapshot {
        CachedSnapshot {
            revision: 1,
            observed: 0,
            built,
            snapshot: None,
            digest: "d".into(),
        }
    }

    /// A node that stopped asking (removed, or gone) does not keep its
    /// snapshot in memory past the age at which it would be rebuilt.
    #[test]
    fn storing_evicts_snapshots_too_old_to_be_reused() {
        let mut cache = SnapshotEntries::default();
        let Some(old) = Instant::now().checked_sub(SNAPSHOT_CACHE_MAX_AGE + Duration::from_secs(1))
        else {
            return;
        };
        cache.entries.insert(7, cached(old));
        cache.entries.insert(8, cached(Instant::now()));
        store(&mut cache, 9, cached(Instant::now()));
        let mut held: Vec<i32> = cache.entries.keys().copied().collect();
        held.sort();
        assert_eq!(held, vec![8, 9]);
    }

    /// A worker's entry keeps its digest, never its snapshot (which holds
    /// every workload in the cluster): N polling workers cost N digests.
    /// The control plane's own node keeps its snapshot, read every pass.
    #[tokio::test]
    async fn only_the_control_plane_keeps_its_snapshot_cached() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, queries) = counted(&db).await;
        let first = service.node_policy(worker, new_agent(None)).await.unwrap();
        let held = first.snapshot_digest.clone().expect("a digest");
        let _ = service
            .cached_snapshot(CONTROL_PLANE_NODE_ID, 0)
            .await
            .unwrap();
        {
            let cache = service.snapshots.lock().unwrap();
            assert!(cache.entries[&worker].snapshot.is_none(), "a digest only");
            assert!(cache.entries[&CONTROL_PLANE_NODE_ID].snapshot.is_some());
        }
        queries.store(0, std::sync::atomic::Ordering::Relaxed);
        let again = service
            .node_policy(worker, new_agent(Some(&held)))
            .await
            .unwrap();
        assert!(again.unchanged);
        assert!(
            queries.load(std::sync::atomic::Ordering::Relaxed) <= 1,
            "answered from the digest"
        );
    }

    /// The key an `off` node's cache is checked against follows the other
    /// nodes' observations (a change rebuilds it), never this node's own,
    /// and a report of the same workloads leaves it as it was.
    #[tokio::test]
    async fn the_observed_key_follows_other_nodes_observations() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, _) = counted(&db).await;
        let other = worker + 1000;
        let sandbox = |id| WorkloadRef::new(WorkloadKind::Sandbox, id);
        let map =
            |id, address: &str| IdentityMap::new().with(sandbox(id), address.parse().unwrap());
        service.record_observed(other, map(1, "10.202.1.5"), NodeMode::Shadow);
        let before = service.observed_key(worker);
        service.record_observed(other, map(1, "10.202.1.5"), NodeMode::Shadow);
        assert_eq!(service.observed_key(worker), before, "the same report");
        service.record_observed(worker, map(2, "10.202.9.5"), NodeMode::Shadow);
        assert_eq!(service.observed_key(worker), before, "its own report");
        service.record_observed(other, map(1, "10.202.1.6"), NodeMode::Shadow);
        assert_ne!(
            service.observed_key(worker),
            before,
            "another node's change"
        );
    }

    /// Eviction is a sweep at most once per cache age, not a scan on every
    /// store: N nodes rebuilding after a revision change cost N inserts.
    #[test]
    fn stores_between_sweeps_do_not_scan_the_cache() {
        let mut cache = SnapshotEntries::default();
        store(&mut cache, 1, cached(Instant::now()));
        let swept = cache.swept.expect("the first store sweeps");
        let Some(old) = Instant::now().checked_sub(SNAPSHOT_CACHE_MAX_AGE + Duration::from_secs(1))
        else {
            return;
        };
        cache.entries.insert(7, cached(old));
        store(&mut cache, 2, cached(Instant::now()));
        assert_eq!(cache.swept, Some(swept), "no sweep before the age passes");
        assert!(
            cache.entries.contains_key(&7),
            "evicted on the next sweep, not now"
        );
        cache.swept = Some(old);
        store(&mut cache, 3, cached(Instant::now()));
        assert!(!cache.entries.contains_key(&7));
    }

    /// A service on its own connection, counting every statement it runs.
    async fn counted(db: &TestDatabase) -> (NetworkPolicyService, Arc<AtomicUsize>) {
        let mut conn: DatabaseConnection = Database::connect(&db.database_url)
            .await
            .expect("connect to the test database");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        conn.set_metric_callback(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        (NetworkPolicyService::new(Arc::new(conn)), count)
    }

    async fn insert_id(db: &TestDatabase, sql: &str) -> i32 {
        db.connection()
            .query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_else(|| panic!("{sql}: no row"))
            .try_get_by_index::<i32>(0)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// A migrated database with one worker, in `off`, and two live
    /// sandboxes on it (or `None` without Docker).
    async fn fixture() -> Option<(TestDatabase, i32)> {
        let db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping: {e}");
                return None;
            }
        };
        let worker = insert_id(
            &db,
            "INSERT INTO nodes (name, token_hash, address, private_address, compute_cidr) \
             VALUES ('worker-1', 'x', 'https://10.201.0.9:3100', '10.201.0.9', '10.202.1.0/24') \
             RETURNING id",
        )
        .await;
        db.connection()
            .execute_unprepared(&format!(
                "INSERT INTO users (id, name, email) VALUES (8101, 'Owner', 'owner@example.com'); \
                 INSERT INTO projects (id, name, slug, repo_name, repo_owner, directory, \
                   main_branch, preset, created_at, updated_at) \
                 VALUES (8101, 'Agent', 'agent-idle', 'agent', 'example', '/', 'main', 'static', \
                   now(), now()); \
                 INSERT INTO sandboxes (id, public_id, name, user_id, project_id, status, backend, \
                   node_id) VALUES \
                   (8101, 'sbx_00000000000d8101', 'a', 8101, 8101, 'running', 'docker', {worker}), \
                   (8102, 'sbx_00000000000d8102', 'b', 8101, 8101, 'running', 'docker', {worker})"
            ))
            .await
            .expect("seed");
        Some((db, worker))
    }

    fn new_agent(held: Option<&str>) -> NodePolicyReport {
        NodePolicyReport {
            backend: "nftables".into(),
            protocol: POLICY_PROTOCOL,
            held_snapshot: held.map(str::to_string),
            ..Default::default()
        }
    }

    fn old_agent() -> NodePolicyReport {
        NodePolicyReport {
            backend: "nftables".into(),
            ..Default::default()
        }
    }

    fn bytes(policy: &NodePolicy) -> usize {
        serde_json::to_vec(policy).map(|b| b.len()).unwrap_or(0)
    }

    /// The idle pass, measured. Before this change a `off` pass was one
    /// query (the mode) and a mode-only answer. Sending the snapshot every
    /// pass would cost a full build; the cache and the digest bring an
    /// unchanged pass back to one query and a mode-plus-digest answer.
    #[tokio::test]
    async fn an_idle_legacy_pass_costs_one_query_and_no_snapshot() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, queries) = counted(&db).await;

        // An old agent: exactly today's answer.
        queries.store(0, Ordering::Relaxed);
        let old = service.node_policy(worker, old_agent()).await.unwrap();
        let old_queries = queries.load(Ordering::Relaxed);
        assert!(old.snapshot.is_none() && old.snapshot_digest.is_none() && !old.unchanged);

        // A new agent's first pass: the full snapshot, built once.
        queries.store(0, Ordering::Relaxed);
        let first = service.node_policy(worker, new_agent(None)).await.unwrap();
        let build_queries = queries.load(Ordering::Relaxed);
        let digest = first.snapshot_digest.clone().expect("a digest");
        assert!(first.snapshot.is_some());

        // Later passes from an agent that holds it: unchanged.
        queries.store(0, Ordering::Relaxed);
        let idle = service
            .node_policy(worker, new_agent(Some(&digest)))
            .await
            .unwrap();
        let idle_queries = queries.load(Ordering::Relaxed);
        assert!(idle.unchanged && idle.snapshot.is_none());
        assert_eq!(idle.snapshot_digest.as_deref(), Some(digest.as_str()));

        // Without the digest (an agent that restarted) it is built again: a
        // worker's snapshot holds every workload, so none is kept per
        // worker, only its digest.
        queries.store(0, Ordering::Relaxed);
        let resent = service.node_policy(worker, new_agent(None)).await.unwrap();
        let resend_queries = queries.load(Ordering::Relaxed);
        assert!(resent.snapshot.is_some());

        println!(
            "idle off pass: before (old agent) {old_queries} queries, {} B answer; \
             full snapshot {build_queries} queries, {} B answer; \
             after (unchanged) {idle_queries} queries, {} B answer; \
             snapshot again without a digest {resend_queries} queries, {} B",
            bytes(&old),
            bytes(&first),
            bytes(&idle),
            bytes(&resent)
        );
        assert_eq!(
            old_queries, 1,
            "an old agent's pass is one query, as before"
        );
        assert_eq!(idle_queries, 1, "an unchanged pass is one query");
        assert_eq!(
            resend_queries, build_queries,
            "a worker's snapshot is not kept: built again for one that lost it"
        );
        assert!(
            build_queries > 5,
            "a build reads many tables: {build_queries}"
        );
        assert!(bytes(&idle) < 80, "{}", bytes(&idle));
    }

    /// Every input a snapshot reads moves the revision, so the cache never
    /// serves a snapshot older than the data it came from.
    #[tokio::test]
    async fn a_change_a_snapshot_depends_on_rebuilds_it() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, _) = counted(&db).await;
        let digest = |p: &NodePolicy| p.snapshot_digest.clone().unwrap();
        let mut held = digest(&service.node_policy(worker, new_agent(None)).await.unwrap());
        let revision = || async { service.revision().await.unwrap() };
        for (what, sql) in [
            (
                "a sandbox opting in to recording",
                "UPDATE sandboxes SET egress_recording = true WHERE id = 8101".to_string(),
            ),
            (
                "a sandbox destroyed",
                "UPDATE sandboxes SET status = 'destroyed' WHERE id = 8102".to_string(),
            ),
            (
                "another node joining",
                "INSERT INTO nodes (name, token_hash, address, private_address) \
                 VALUES ('worker-9', 'z', 'https://10.201.0.99:3100', '10.201.0.99')"
                    .to_string(),
            ),
        ] {
            let before = revision().await;
            db.connection().execute_unprepared(&sql).await.unwrap();
            assert!(revision().await > before, "{what} bumps the revision");
            let answer = service
                .node_policy(worker, new_agent(Some(&held)))
                .await
                .unwrap();
            assert!(!answer.unchanged, "{what}: a fresh snapshot");
            held = digest(&answer);
        }
        // A change that does not alter this node's snapshot moves the
        // revision, rebuilds, and still answers unchanged.
        let before = revision().await;
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE nodes SET compute_cidr = '10.202.9.0/24' WHERE id = {worker}"
            ))
            .await
            .unwrap();
        assert!(
            revision().await > before,
            "a compute subnet bumps the revision"
        );
        assert!(
            service
                .node_policy(worker, new_agent(Some(&held)))
                .await
                .unwrap()
                .unchanged
        );
        // Heartbeats and activity do not.
        let before = revision().await;
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE nodes SET last_heartbeat = now() WHERE id = {worker}; \
                 UPDATE sandboxes SET last_activity_at = now() WHERE id = 8101"
            ))
            .await
            .unwrap();
        assert_eq!(revision().await, before);
        let answer = service
            .node_policy(worker, new_agent(Some(&held)))
            .await
            .unwrap();
        assert!(answer.unchanged);
        // The opted-in sandbox is marked in the snapshot an `off` node holds.
        let snapshot = service.cached_snapshot(worker, before).await.unwrap().0;
        assert!(snapshot
            .egress
            .iter()
            .any(|p| p.subject == WorkloadRef::new(WorkloadKind::Sandbox, 8101) && p.record));
    }

    /// An `off` node whose snapshot cannot be built (a table it reads is
    /// unreadable) still hears its mode: an agent turned off to recover
    /// from a bad policy removes its rules instead of keeping them while
    /// every poll fails.
    #[tokio::test]
    async fn an_off_node_hears_its_mode_when_its_snapshot_cannot_be_built() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, _) = counted(&db).await;
        db.connection()
            .execute_unprepared(
                "ALTER TABLE network_policy_rules RENAME TO network_policy_rules_gone",
            )
            .await
            .unwrap();
        let answer = service
            .node_policy(worker, new_agent(None))
            .await
            .expect("the mode, not an error");
        assert_eq!(answer.mode, NodeMode::Off);
        assert!(answer.snapshot.is_none() && !answer.unchanged);
    }

    /// A node outside `off` gets exactly what it got before: a freshly
    /// built snapshot every pass, never `unchanged`, never a digest.
    #[tokio::test]
    async fn shadow_and_enforce_nodes_are_answered_as_before() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE nodes SET network_policy_mode = 'shadow' WHERE id = {worker}"
            ))
            .await
            .unwrap();
        let (service, queries) = counted(&db).await;
        for report in [new_agent(Some("anything")), old_agent()] {
            queries.store(0, Ordering::Relaxed);
            let answer = service.node_policy(worker, report).await.unwrap();
            assert!(answer.snapshot.is_some() && !answer.unchanged);
            assert_eq!(answer.snapshot_digest, None);
            assert!(queries.load(Ordering::Relaxed) > 5, "built fresh, uncached");
        }
    }

    /// Observations are reported in every mode, but only a node outside
    /// `off` has its workloads handed to other nodes, as before.
    #[tokio::test]
    async fn legacy_observations_are_not_shared_with_other_nodes() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let other = insert_id(
            &db,
            "INSERT INTO nodes (name, token_hash, address, private_address, compute_cidr) \
             VALUES ('worker-2', 'y', 'https://10.201.0.10:3100', '10.201.0.10', '10.202.2.0/24') \
             RETURNING id",
        )
        .await;
        let (service, _) = counted(&db).await;
        let sandbox = WorkloadRef::new(WorkloadKind::Sandbox, 8101);
        let report = |mode_identities: IdentityMap| NodePolicyReport {
            identities: mode_identities,
            ..new_agent(None)
        };
        let seen = IdentityMap::new().with(sandbox, "10.202.1.5".parse().unwrap());
        service
            .node_policy(worker, report(seen.clone()))
            .await
            .unwrap();
        let snapshot = service.node_snapshot(other).await.unwrap();
        assert_eq!(
            snapshot.remote.addresses(&sandbox).count(),
            0,
            "an `off` node's workloads are not remote peers"
        );
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE nodes SET network_policy_mode = 'shadow' WHERE id = {worker}"
            ))
            .await
            .unwrap();
        service.node_policy(worker, report(seen)).await.unwrap();
        let snapshot = service.node_snapshot(other).await.unwrap();
        assert_eq!(snapshot.remote.addresses(&sandbox).count(), 1);
    }

    /// Node 0 reads its mode and the revision in one query, and in `off`
    /// it takes no snapshot at all (nothing reads what it would observe):
    /// a steady pass is one query and no clone.
    #[tokio::test]
    async fn node_zero_in_off_takes_no_snapshot() {
        use temps_workload_netpol::PolicySource;
        let Some((db, _)) = fixture().await else {
            return;
        };
        let (service, queries) = counted(&db).await;
        let source = crate::source::DbPolicySource::new(Arc::new(service), CONTROL_PLANE_NODE_ID);
        assert_eq!(source.mode().await.unwrap(), NodeMode::Off);
        assert!(source.off_snapshot().await.unwrap().is_none());
        queries.store(0, Ordering::Relaxed);
        assert_eq!(source.mode().await.unwrap(), NodeMode::Off);
        assert!(source.off_snapshot().await.unwrap().is_none());
        assert_eq!(
            queries.load(Ordering::Relaxed),
            1,
            "one query per steady pass"
        );
    }

    /// Version skew on the wire: each agent reads a mode its enum has. An
    /// agent before protocol 3 gets `legacy`, and before protocol 2 no
    /// snapshot; protocol 3 gets `off`. Setting `off` stores `legacy`, so a
    /// rolled-back binary still reads it, and `off` rows are read too.
    #[tokio::test]
    async fn each_agent_reads_a_mode_its_version_knows() {
        let Some((db, worker)) = fixture().await else {
            return;
        };
        let (service, _) = counted(&db).await;
        let wire = |policy: &NodePolicy| serde_json::to_value(policy).unwrap();
        for (protocol, name, snapshot) in
            [(0, "legacy", false), (2, "legacy", true), (3, "off", true)]
        {
            let report = NodePolicyReport {
                protocol,
                ..old_agent()
            };
            let answer = wire(&service.node_policy(worker, report).await.unwrap());
            assert_eq!(answer["mode"], name, "protocol {protocol}");
            assert_eq!(
                answer.get("snapshot").is_some(),
                snapshot,
                "protocol {protocol}"
            );
        }
        let (_, node) = service.set_node_mode(worker, NodeMode::Off).await.unwrap();
        assert_eq!(node.mode, NodeMode::Off);
        let stored: String = db
            .connection()
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!("SELECT network_policy_mode FROM nodes WHERE id = {worker}"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "network_policy_mode")
            .unwrap();
        assert_eq!(stored, "legacy", "stored under the name every binary reads");
        db.connection()
            .execute_unprepared(&format!(
                "UPDATE nodes SET network_policy_mode = 'off' WHERE id = {worker}"
            ))
            .await
            .unwrap();
        assert_eq!(service.node_mode(worker).await.unwrap(), NodeMode::Off);
        assert_eq!(
            service.mode_and_revision(worker).await.unwrap().0,
            NodeMode::Off
        );
    }

    #[tokio::test]
    async fn an_unknown_node_is_not_found() {
        let Some((db, _)) = fixture().await else {
            return;
        };
        let (service, _) = counted(&db).await;
        assert!(matches!(
            service.mode_and_revision(9999).await,
            Err(NetworkPolicyError::NodeNotFound { node_id: 9999 })
        ));
        let (mode, _) = service
            .mode_and_revision(CONTROL_PLANE_NODE_ID)
            .await
            .unwrap();
        assert_eq!(mode, NodeMode::Off);
    }
}
