// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The OSS [`temps_core::ProjectIpGate`]: per-project / per-environment
//! allow/deny CIDR lists read from `project_ip_access_rules`.
//!
//! # Hot path
//!
//! `is_allowed` is one `ArcSwap::load` (lock-free), one `HashMap` lookup keyed
//! by project id (the overwhelmingly common miss ends here), an optional
//! binary search over the project's environment overrides, and a binary search
//! over merged integer ranges. No allocation, no I/O, no locks.
//!
//! # Memory bound
//!
//! At most [`MAX_CIDRS_PER_POLICY`] entries per policy, one policy per
//! project plus one per environment override, stored as merged 8/32-byte
//! ranges. Operators with thousands of restricted environments are the only
//! way this reaches megabytes.
//!
//! # Freshness and failure modes
//!
//! Every write to `project_ip_access_rules` draws a value from one sequence
//! (a trigger assigns it, so a manual edit is covered too); deletes leave a
//! tombstone carrying the same kind of value. A background task reads the
//! sequence head every [`POLL_INTERVAL`] -- one O(1) read -- and only when it
//! moved fetches the rows and tombstones above the revision it last applied,
//! patching just the affected scopes of the snapshot. Cost is proportional to
//! the number of changes, not the number of rules. A rule change is therefore
//! enforced within [`POLL_INTERVAL`] in every process, including a standalone
//! `temps proxy`.
//!
//! The triggers serialise writers on one advisory lock held until commit, so
//! revisions become visible in order. The gate therefore advances its cursor
//! to the newest revision it actually *read*, never to the sequence head: a
//! writer that has drawn a revision but not committed yet is picked up by a
//! later poll instead of being skipped. A rolled-back writer leaves a gap in
//! the sequence; the poll then costs two empty indexed reads until the next
//! write, which is still independent of the number of rules. The deletions
//! and the changed rows are read inside one repeatable-read transaction, so a
//! deletion can never fall between the two reads and be skipped by the cursor.
//!
//! A full reload (O(rules)) is not part of the steady state. It runs at
//! startup, when the sequence head is *behind* the cursor (the database was
//! restored), when no poll has succeeded for [`STALE_AFTER`] (the tombstones a
//! missed deletion would need are pruned after an hour), and every
//! [`FULL_RELOAD_INTERVAL`] as a rare safety net for edits made with triggers
//! disabled.
//!
//! * The **initial load must succeed**: [`ProjectIpAccessGate::hydrate`]
//!   errors are returned to the caller, which aborts startup rather than
//!   serve a protected project with no rules.
//! * A **later reload failure keeps the last good snapshot** and logs. The
//!   proxy keeps enforcing what it last knew; it never reverts to open.
//! * The gate is only consulted once a project/environment has been resolved,
//!   so the console and its API are never subject to a project's rules.

use super::cidr_set::{parse_cidr, CidrSet};
use super::IpAccessMode;
use arc_swap::ArcSwap;
use sea_orm::{
    AccessMode, ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult,
    IsolationLevel, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::ProjectIpGate;
use temps_entities::project_ip_access_rules;
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// How often the change feed head is read. One sequence read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Safety-net interval between full snapshot rebuilds. The change feed is the
/// normal path; this only covers edits that bypassed its triggers.
pub const FULL_RELOAD_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// A gate that has not completed a poll for this long rebuilds from scratch:
/// deletions are only recorded for an hour, so a longer silence could have
/// missed one.
pub const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Error)]
pub enum IpAccessGateError {
    #[error("Failed to {operation} project IP access rules: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// A compiled, enforceable policy. A scope that is `disabled` (or a deny list
/// with no entries) compiles to no policy at all.
#[derive(Debug)]
struct Policy {
    mode: IpAccessMode,
    set: CidrSet,
}

#[derive(Debug, Default, Clone)]
struct ProjectPolicies {
    /// Project-wide policy, if one is active.
    project: Option<Arc<Policy>>,
    /// Environment overrides sorted by environment id. `None` is an explicit
    /// "unrestricted", which exempts the environment from `project`.
    environments: Vec<(i32, Option<Arc<Policy>>)>,
}

impl ProjectPolicies {
    /// No scope is recorded, so the project need not be tracked.
    ///
    /// An environment override that enforces nothing (`disabled`) is still a
    /// record: it exempts the environment from a project-wide list that may be
    /// saved later, so a project holding one is not inert.
    fn is_inert(&self) -> bool {
        self.project.is_none() && self.environments.is_empty()
    }

    fn set_scope(&mut self, environment_id: Option<i32>, policy: Option<Arc<Policy>>) {
        match environment_id {
            None => self.project = policy,
            Some(id) => match self.environments.binary_search_by_key(&id, |(env, _)| *env) {
                Ok(index) => self.environments[index].1 = policy,
                Err(index) => self.environments.insert(index, (id, policy)),
            },
        }
    }

    fn remove_scope(&mut self, environment_id: Option<i32>) {
        match environment_id {
            None => self.project = None,
            Some(id) => self.environments.retain(|(env, _)| *env != id),
        }
    }
}

#[derive(Debug, Default)]
struct Snapshot {
    projects: HashMap<i32, Arc<ProjectPolicies>>,
}

/// A scope whose rule was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemovedScope {
    project_id: i32,
    environment_id: Option<i32>,
}

/// One tombstone as stored: the scope plus the revision that deleted it.
#[derive(Debug, FromQueryResult)]
struct TombstoneRow {
    revision: i64,
    project_id: i32,
    environment_id: Option<i32>,
}

impl Snapshot {
    /// The policy that governs `environment_id`: its own override when one
    /// exists (even an explicit "unrestricted"), otherwise the project's.
    fn effective(&self, project_id: i32, environment_id: i32) -> Option<&Policy> {
        let scopes = self.projects.get(&project_id)?;
        match scopes
            .environments
            .binary_search_by_key(&environment_id, |(id, _)| *id)
        {
            Ok(index) => scopes.environments[index].1.as_deref(),
            Err(_) => scopes.project.as_deref(),
        }
    }

    /// A new snapshot with `removed` scopes dropped and `changed` rows
    /// compiled in. Untouched projects are shared with `self`, so the cost is
    /// proportional to the delta (plus one pointer copy per tracked project).
    ///
    /// Removals apply first: a scope that was deleted and then recreated has
    /// both a tombstone and a live row, and the row must win.
    fn patched(
        &self,
        removed: &[RemovedScope],
        changed: &[project_ip_access_rules::Model],
    ) -> Snapshot {
        let mut projects = self.projects.clone();
        for scope in removed {
            if let Some(entry) = projects.get_mut(&scope.project_id) {
                Arc::make_mut(entry).remove_scope(scope.environment_id);
                if entry.is_inert() {
                    projects.remove(&scope.project_id);
                }
            }
        }
        for row in changed {
            let policy = compile_policy(row).map(Arc::new);
            let entry = projects.entry(row.project_id).or_default();
            Arc::make_mut(entry).set_scope(row.environment_id, policy);
            if entry.is_inert() {
                projects.remove(&row.project_id);
            }
        }
        Snapshot { projects }
    }
}

#[derive(Debug, FromQueryResult)]
struct RevisionRow {
    revision: i64,
}

/// The change feed's head: the last value the revision sequence handed out.
const READ_REVISION_SQL: &str = "SELECT CASE WHEN is_called THEN last_value ELSE 0 END \
     AS revision FROM project_ip_access_rules_revision_seq";

/// Revision value meaning "no snapshot has been loaded yet".
const NO_REVISION: i64 = -1;

enum Refresh {
    Unchanged,
    Patched { changed: usize, removed: usize },
    Reloaded { rules: usize },
}

pub struct ProjectIpAccessGate {
    db: Arc<DatabaseConnection>,
    snapshot: ArcSwap<Snapshot>,
    /// Change-feed revision the snapshot is current as of, or [`NO_REVISION`].
    /// Written only by the refresh path (startup, then the single loop task).
    applied_revision: AtomicI64,
}

impl ProjectIpAccessGate {
    /// An empty gate. Call [`Self::hydrate`] before handing it to the proxy.
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
            applied_revision: AtomicI64::new(NO_REVISION),
        }
    }

    /// Load every rule and publish the snapshot. Must succeed once at startup.
    ///
    /// A missing `project_ip_access_rules` table is treated as "no rules":
    /// a proxy started before the console has applied the migration has
    /// nothing to enforce, and the refresh loop picks the table up as soon as
    /// it exists. Every other failure is returned.
    /// Gate with a fixed snapshot and an unused mock connection, for tests
    /// elsewhere in the crate that need a real gate rather than a stub.
    #[cfg(test)]
    pub(crate) fn from_rows_for_tests(rows: Vec<project_ip_access_rules::Model>) -> Self {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
        let gate = Self::new(Arc::new(db));
        gate.snapshot
            .store(Arc::new(Snapshot::default().patched(&[], &rows)));
        gate
    }

    pub async fn hydrate(&self) -> Result<(), IpAccessGateError> {
        match self.refresh(true).await {
            Ok(_) => Ok(()),
            Err(IpAccessGateError::Database { source, .. }) if is_missing_table(&source) => {
                warn!(
                    "project_ip_access_rules does not exist yet (migration pending); \
                     enforcing no project IP rules until it does"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Build a hydrated gate and start its refresh loop on `handle`'s runtime.
    ///
    /// For synchronous startup code (`temps proxy`, `temps serve`): it blocks
    /// the calling thread on the initial load, so it must not be called from
    /// inside an async context. Async callers use [`Self::new`],
    /// [`Self::hydrate`] and [`Self::spawn_refresh_loop`] directly.
    pub fn start_blocking(
        db: Arc<DatabaseConnection>,
        handle: &tokio::runtime::Handle,
    ) -> Result<Arc<Self>, IpAccessGateError> {
        let gate = Arc::new(Self::new(db));
        handle.block_on(gate.hydrate())?;
        gate.spawn_refresh_loop(handle);
        Ok(gate)
    }

    /// Spawn the change-detection loop on `handle`'s runtime.
    pub fn spawn_refresh_loop(
        self: &Arc<Self>,
        handle: &tokio::runtime::Handle,
    ) -> tokio::task::JoinHandle<()> {
        handle.spawn(Arc::clone(self).run_refresh_loop())
    }

    async fn run_refresh_loop(self: Arc<Self>) {
        let mut last_full_reload = Instant::now();
        let mut last_success = Instant::now();
        let mut consecutive_failures: u64 = 0;

        loop {
            // Poll first: the startup load may already be a poll behind.
            let force_full = last_full_reload.elapsed() >= FULL_RELOAD_INTERVAL
                || last_success.elapsed() >= STALE_AFTER;
            match self.refresh(force_full).await {
                Ok(outcome) => {
                    last_success = Instant::now();
                    if consecutive_failures > 0 {
                        info!(
                            failed_polls = consecutive_failures,
                            "Project IP access rule refresh recovered"
                        );
                        consecutive_failures = 0;
                    }
                    match outcome {
                        Refresh::Unchanged => {}
                        Refresh::Patched { changed, removed } => {
                            info!(changed, removed, "Project IP access rules updated");
                        }
                        Refresh::Reloaded { rules } => {
                            last_full_reload = Instant::now();
                            debug!(rules, "Project IP access rules fully reloaded");
                        }
                    }
                }
                Err(e) => {
                    consecutive_failures += 1;
                    // First failure, then once a minute at the poll rate.
                    if consecutive_failures == 1 || consecutive_failures.is_multiple_of(12) {
                        error!(
                            failed_polls = consecutive_failures,
                            error = %e,
                            "Project IP access rule refresh failed; continuing to enforce the \
                             last loaded rules"
                        );
                    }
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn read_revision(&self) -> Result<i64, IpAccessGateError> {
        let row = RevisionRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            READ_REVISION_SQL,
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| IpAccessGateError::Database {
            operation: "read the change revision of",
            source,
        })?;
        Ok(row.map_or(0, |row| row.revision))
    }

    /// The deletions and the changed rows above `applied`, read from one
    /// database snapshot.
    ///
    /// Two separate reads could straddle a commit: a deletion that lands
    /// after the tombstone read, followed by another write that lands before
    /// the row read, would move the cursor past a removal that was never
    /// seen. A repeatable-read transaction makes both reads see the same
    /// committed state.
    async fn read_delta(
        &self,
        applied: i64,
    ) -> Result<(Vec<TombstoneRow>, Vec<project_ip_access_rules::Model>), IpAccessGateError> {
        let txn = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
            .map_err(|source| IpAccessGateError::Database {
                operation: "open a snapshot to read the changes to",
                source,
            })?;
        let tombstones = TombstoneRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT revision, project_id, environment_id \
             FROM project_ip_access_rule_tombstones \
             WHERE revision > $1 ORDER BY revision",
            [applied.into()],
        ))
        .all(&txn)
        .await
        .map_err(|source| IpAccessGateError::Database {
            operation: "read the deletions of",
            source,
        })?;
        let changed = project_ip_access_rules::Entity::find()
            .filter(project_ip_access_rules::Column::Revision.gt(applied))
            .order_by_asc(project_ip_access_rules::Column::Revision)
            .all(&txn)
            .await
            .map_err(|source| IpAccessGateError::Database {
                operation: "read the changes to",
                source,
            })?;
        // Read-only: the commit only releases the snapshot.
        txn.commit()
            .await
            .map_err(|source| IpAccessGateError::Database {
                operation: "close the snapshot that read the changes to",
                source,
            })?;
        Ok((tombstones, changed))
    }

    /// Bring the snapshot up to date. A full reload when `force_full`, when
    /// nothing has been loaded yet, or when the database's sequence is behind
    /// what was applied (a restore); otherwise a delta that costs one sequence
    /// read when nothing changed. Nothing is published unless every read
    /// succeeds, so a failure keeps the last good snapshot.
    async fn refresh(&self, force_full: bool) -> Result<Refresh, IpAccessGateError> {
        let head = self.read_revision().await?;
        let applied = self.applied_revision.load(Ordering::Acquire);

        if force_full || applied == NO_REVISION || head < applied {
            let rows = project_ip_access_rules::Entity::find()
                .all(self.db.as_ref())
                .await
                .map_err(|source| IpAccessGateError::Database {
                    operation: "load",
                    source,
                })?;
            let rules = rows.len();
            // The newest revision actually seen, not `head`: a writer that has
            // drawn a revision but not committed is not in `rows`, and must be
            // found by the next delta.
            let seen = rows.iter().map(|row| row.revision).max().unwrap_or(0);
            self.snapshot
                .store(Arc::new(Snapshot::default().patched(&[], &rows)));
            self.applied_revision.store(seen, Ordering::Release);
            debug!(
                rules,
                head,
                applied = seen,
                "Published project IP access snapshot"
            );
            return Ok(Refresh::Reloaded { rules });
        }

        if head == applied {
            return Ok(Refresh::Unchanged);
        }

        let (tombstones, changed) = self.read_delta(applied).await?;

        let newest = changed
            .iter()
            .map(|row| row.revision)
            .chain(tombstones.iter().map(|tombstone| tombstone.revision))
            .max();
        let Some(newest) = newest else {
            // The head moved but nothing is visible: a writer that has not
            // committed yet, or one that rolled back. Keep the cursor.
            return Ok(Refresh::Unchanged);
        };

        let removed: Vec<RemovedScope> = tombstones
            .iter()
            .map(|tombstone| RemovedScope {
                project_id: tombstone.project_id,
                environment_id: tombstone.environment_id,
            })
            .collect();
        let next = self.snapshot.load().patched(&removed, &changed);
        self.snapshot.store(Arc::new(next));
        self.applied_revision
            .store(newest.max(applied), Ordering::Release);
        Ok(Refresh::Patched {
            changed: changed.len(),
            removed: removed.len(),
        })
    }
}

fn is_missing_table(error: &sea_orm::DbErr) -> bool {
    let message = error.to_string();
    message.contains("project_ip_access_rules") && message.contains("does not exist")
}

/// The policy of a scope that must refuse every client: an allow list that
/// admits nothing.
fn deny_everyone() -> Policy {
    Policy {
        mode: IpAccessMode::Allow,
        set: CidrSet::default(),
    }
}

/// Compile one stored row. Returns `None` for a scope that enforces nothing.
///
/// Entries that fail to parse can only come from a manual edit of the table
/// (the service canonicalises on write). The scope fails closed:
///
/// * an **allow** list skips the bad entry, which admits fewer networks, and
///   an allow list left with no networks refuses everyone;
/// * a **deny** list cannot skip it -- the unreadable entry may be the one
///   that mattered, and dropping it (or the whole list, when every entry is
///   bad) would silently open the scope -- so the scope refuses everyone
///   until the entry is fixed.
fn compile_policy(row: &project_ip_access_rules::Model) -> Option<Policy> {
    let mode = match IpAccessMode::parse(&row.mode) {
        Some(mode) => mode,
        None => {
            // The CHECK constraint makes this unreachable. Fail closed for the
            // scope rather than silently treating it as open.
            error!(
                rule_id = row.id,
                project_id = row.project_id,
                environment_id = ?row.environment_id,
                mode = %row.mode,
                "Unknown IP access mode; denying all traffic for this scope"
            );
            return Some(deny_everyone());
        }
    };
    if mode == IpAccessMode::Disabled {
        return None;
    }

    let mut networks = Vec::with_capacity(row.cidrs.0.len());
    let mut unparsable = 0usize;
    for entry in &row.cidrs.0 {
        match parse_cidr(&entry.cidr) {
            Ok(network) => networks.push(network),
            Err(e) => {
                unparsable += 1;
                error!(
                    rule_id = row.id,
                    project_id = row.project_id,
                    environment_id = ?row.environment_id,
                    cidr = %entry.cidr,
                    error = %e,
                    "Unparsable CIDR in project IP access rule"
                );
            }
        }
    }

    if mode == IpAccessMode::Deny {
        if unparsable > 0 {
            error!(
                rule_id = row.id,
                project_id = row.project_id,
                environment_id = ?row.environment_id,
                unparsable,
                "Deny list has unreadable entries; denying all traffic for this scope"
            );
            return Some(deny_everyone());
        }
        // A deny list that names nothing blocks nothing: report no policy so
        // an unresolvable client IP is not treated as a denial for it.
        if networks.is_empty() {
            return None;
        }
    }
    Some(Policy {
        mode,
        set: CidrSet::from_networks(&networks),
    })
}

impl ProjectIpGate for ProjectIpAccessGate {
    fn is_allowed(&self, project_id: i32, environment_id: i32, ip: IpAddr) -> bool {
        let snapshot = self.snapshot.load();
        match snapshot.effective(project_id, environment_id) {
            None => true,
            Some(policy) => match policy.mode {
                IpAccessMode::Allow => policy.set.contains(ip),
                IpAccessMode::Deny => !policy.set.contains(ip),
                IpAccessMode::Disabled => true,
            },
        }
    }

    fn has_active_policy(&self, project_id: i32, environment_id: i32) -> bool {
        self.snapshot
            .load()
            .effective(project_id, environment_id)
            .is_some()
    }

    /// Only a deny list is a "deny floor". An allow list is exactly what a
    /// request-policy `Allow` decision is meant to be able to override.
    fn is_explicitly_denied(
        &self,
        project_id: i32,
        environment_id: i32,
        ip: Option<IpAddr>,
    ) -> bool {
        let snapshot = self.snapshot.load();
        match snapshot.effective(project_id, environment_id) {
            Some(policy) if policy.mode == IpAccessMode::Deny => match ip {
                Some(ip) => policy.set.contains(ip),
                // Cannot evaluate a deny list without an address.
                None => true,
            },
            _ => false,
        }
    }

    fn reads_project_ip_access_rules(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::project_ip_access_rules::{IpAccessCidrEntry, IpAccessCidrs};

    fn row(
        id: i32,
        project_id: i32,
        environment_id: Option<i32>,
        mode: &str,
        cidrs: &[&str],
    ) -> project_ip_access_rules::Model {
        let now = chrono::Utc::now();
        project_ip_access_rules::Model {
            id,
            project_id,
            environment_id,
            mode: mode.to_string(),
            cidrs: IpAccessCidrs(
                cidrs
                    .iter()
                    .map(|c| IpAccessCidrEntry {
                        cidr: c.to_string(),
                        description: None,
                    })
                    .collect(),
            ),
            updated_by: None,
            revision: id as i64,
            created_at: now,
            updated_at: now,
        }
    }

    fn gate_with(rows: Vec<project_ip_access_rules::Model>) -> ProjectIpAccessGate {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.snapshot
            .store(Arc::new(Snapshot::default().patched(&[], &rows)));
        gate
    }

    fn revision_row(revision: i64) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([("revision", sea_orm::Value::BigInt(Some(revision)))])
    }

    fn no_tombstones() -> Vec<std::collections::BTreeMap<&'static str, sea_orm::Value>> {
        Vec::new()
    }

    fn removed_row(
        revision: i64,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([
            ("revision", sea_orm::Value::BigInt(Some(revision))),
            ("project_id", sea_orm::Value::Int(Some(project_id))),
            ("environment_id", sea_orm::Value::Int(environment_id)),
        ])
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP is valid")
    }

    #[test]
    fn unconfigured_project_is_unrestricted() {
        let gate = gate_with(vec![]);
        assert!(gate.is_allowed(1, 1, ip("203.0.113.9")));
        assert!(!gate.has_active_policy(1, 1));
        assert!(!gate.is_explicitly_denied(1, 1, Some(ip("203.0.113.9"))));
        assert!(!gate.is_explicitly_denied(1, 1, None));
    }

    #[test]
    fn allow_list_admits_only_listed_networks() {
        let gate = gate_with(vec![row(
            1,
            7,
            None,
            "allow",
            &["100.64.0.0/10", "fd7a:115c:a1e0::/48"],
        )]);
        assert!(gate.has_active_policy(7, 70));
        assert!(gate.is_allowed(7, 70, ip("100.100.1.1")));
        assert!(gate.is_allowed(7, 70, ip("fd7a:115c:a1e0:ab12::1")));
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.9")));
        assert!(!gate.is_allowed(7, 70, ip("2001:db8::1")));
        // Another project is untouched.
        assert!(gate.is_allowed(8, 80, ip("203.0.113.9")));
    }

    #[test]
    fn allow_list_matches_a_v4_mapped_client() {
        let gate = gate_with(vec![row(1, 7, None, "allow", &["100.64.0.0/10"])]);
        assert!(gate.is_allowed(7, 70, ip("::ffff:100.64.0.5")));
        assert!(!gate.is_allowed(7, 70, ip("::ffff:8.8.8.8")));
    }

    #[test]
    fn deny_list_blocks_only_listed_networks() {
        let gate = gate_with(vec![row(1, 7, None, "deny", &["198.51.100.0/24"])]);
        assert!(!gate.is_allowed(7, 70, ip("198.51.100.200")));
        assert!(gate.is_allowed(7, 70, ip("198.51.101.1")));
        assert!(gate.has_active_policy(7, 70));
    }

    #[test]
    fn deny_list_is_an_explicit_deny_floor_but_allow_list_is_not() {
        let gate = gate_with(vec![
            row(1, 7, None, "deny", &["198.51.100.0/24"]),
            row(2, 8, None, "allow", &["10.0.0.0/8"]),
        ]);
        assert!(gate.is_explicitly_denied(7, 70, Some(ip("198.51.100.1"))));
        assert!(!gate.is_explicitly_denied(7, 70, Some(ip("203.0.113.1"))));
        assert!(gate.is_explicitly_denied(7, 70, None));
        assert!(!gate.is_explicitly_denied(8, 80, Some(ip("203.0.113.1"))));
        assert!(!gate.is_explicitly_denied(8, 80, None));
    }

    #[test]
    fn disabled_and_empty_deny_policies_enforce_nothing() {
        let gate = gate_with(vec![
            row(1, 7, None, "disabled", &["10.0.0.0/8"]),
            row(2, 8, None, "deny", &[]),
        ]);
        assert!(gate.is_allowed(7, 70, ip("203.0.113.1")));
        assert!(!gate.has_active_policy(7, 70));
        assert!(gate.is_allowed(8, 80, ip("203.0.113.1")));
        assert!(!gate.has_active_policy(8, 80));
        assert!(gate.snapshot.load().projects.is_empty());
    }

    #[test]
    fn empty_allow_list_denies_everyone() {
        let gate = gate_with(vec![row(1, 7, None, "allow", &[])]);
        assert!(!gate.is_allowed(7, 70, ip("10.0.0.1")));
        assert!(gate.has_active_policy(7, 70));
    }

    #[test]
    fn environment_override_replaces_the_project_policy() {
        let gate = gate_with(vec![
            row(1, 7, None, "allow", &["10.0.0.0/8"]),
            row(2, 7, Some(71), "allow", &["192.168.0.0/16"]),
        ]);
        // Environment 70 inherits the project list.
        assert!(gate.is_allowed(7, 70, ip("10.1.1.1")));
        assert!(!gate.is_allowed(7, 70, ip("192.168.1.1")));
        // Environment 71 uses only its own list, not the union.
        assert!(gate.is_allowed(7, 71, ip("192.168.1.1")));
        assert!(!gate.is_allowed(7, 71, ip("10.1.1.1")));
    }

    #[test]
    fn disabled_environment_override_exempts_it_from_the_project_policy() {
        let gate = gate_with(vec![
            row(1, 7, None, "allow", &["10.0.0.0/8"]),
            row(2, 7, Some(71), "disabled", &[]),
        ]);
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
        assert!(gate.is_allowed(7, 71, ip("203.0.113.1")));
        assert!(!gate.has_active_policy(7, 71));
        assert!(gate.has_active_policy(7, 70));
    }

    #[test]
    fn environment_only_policy_leaves_siblings_unrestricted() {
        let gate = gate_with(vec![row(1, 7, Some(71), "allow", &["10.0.0.0/8"])]);
        assert!(!gate.is_allowed(7, 71, ip("203.0.113.1")));
        assert!(gate.is_allowed(7, 70, ip("203.0.113.1")));
    }

    #[test]
    fn unknown_mode_fails_closed() {
        let gate = gate_with(vec![row(1, 7, None, "bogus", &["10.0.0.0/8"])]);
        assert!(!gate.is_allowed(7, 70, ip("10.0.0.1")));
        assert!(gate.has_active_policy(7, 70));
    }

    #[test]
    fn unparsable_entry_in_an_allow_list_narrows_the_list() {
        let gate = gate_with(vec![row(
            1,
            7,
            None,
            "allow",
            &["10.0.0.0/8", "garbage", "10.0.0.5/8"],
        )]);
        assert!(gate.is_allowed(7, 70, ip("10.1.1.1")));
        assert!(!gate.is_allowed(7, 70, ip("192.168.1.1")));
    }

    #[test]
    fn unparsable_entry_in_a_deny_list_denies_the_scope() {
        // Dropping the bad entry would silently reopen the scope.
        let gate = gate_with(vec![row(
            1,
            7,
            None,
            "deny",
            &["198.51.100.0/24", "garbage"],
        )]);
        assert!(gate.has_active_policy(7, 70));
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
        assert!(!gate.is_allowed(7, 70, ip("198.51.100.9")));
    }

    #[test]
    fn deny_list_of_only_unparsable_entries_does_not_become_open() {
        let gate = gate_with(vec![row(1, 7, None, "deny", &["garbage", "10.0.0.5/99"])]);
        assert!(gate.has_active_policy(7, 70));
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
    }

    #[test]
    fn allow_list_of_only_unparsable_entries_denies_everyone() {
        let gate = gate_with(vec![row(1, 7, None, "allow", &["garbage"])]);
        assert!(gate.has_active_policy(7, 70));
        assert!(!gate.is_allowed(7, 70, ip("10.0.0.1")));
    }

    #[test]
    fn a_patch_replaces_adds_and_removes_only_the_named_scopes() {
        let before = Snapshot::default().patched(
            &[],
            &[
                row(1, 7, None, "allow", &["10.0.0.0/8"]),
                row(2, 7, Some(71), "allow", &["192.168.0.0/16"]),
                row(3, 8, None, "deny", &["198.51.100.0/24"]),
            ],
        );
        let after = before.patched(
            &[
                RemovedScope {
                    project_id: 7,
                    environment_id: Some(71),
                },
                RemovedScope {
                    project_id: 8,
                    environment_id: None,
                },
            ],
            &[
                // Project 7's list is edited; project 9 appears.
                row(1, 7, None, "allow", &["172.16.0.0/12"]),
                row(4, 9, Some(90), "deny", &["203.0.113.0/24"]),
            ],
        );

        let expected = Snapshot::default().patched(
            &[],
            &[
                row(1, 7, None, "allow", &["172.16.0.0/12"]),
                row(4, 9, Some(90), "deny", &["203.0.113.0/24"]),
            ],
        );
        for (project, env, addr) in [
            (7, 70, "10.1.1.1"),
            (7, 70, "172.16.1.1"),
            (7, 71, "192.168.1.1"),
            (8, 80, "198.51.100.5"),
            (9, 90, "203.0.113.5"),
            (9, 91, "203.0.113.5"),
        ] {
            let addr = ip(addr);
            let verdict = |snapshot: &Snapshot| {
                snapshot
                    .effective(project, env)
                    .map(|policy| match policy.mode {
                        IpAccessMode::Allow => policy.set.contains(addr),
                        IpAccessMode::Deny => !policy.set.contains(addr),
                        IpAccessMode::Disabled => true,
                    })
            };
            assert_eq!(
                verdict(&after),
                verdict(&expected),
                "{project}/{env}/{addr}"
            );
        }
        // The snapshot it was patched from is untouched.
        assert!(before.effective(8, 80).is_some());
    }

    #[test]
    fn a_patch_drops_projects_left_with_nothing_to_enforce() {
        let before = Snapshot::default().patched(&[], &[row(1, 7, None, "allow", &["10.0.0.0/8"])]);
        let removed = before.patched(
            &[RemovedScope {
                project_id: 7,
                environment_id: None,
            }],
            &[],
        );
        assert!(removed.projects.is_empty());
        let disabled = before.patched(&[], &[row(1, 7, None, "disabled", &[])]);
        assert!(disabled.projects.is_empty());
    }

    #[test]
    fn a_disabled_environment_override_survives_a_later_project_list() {
        // The environment was exempted while the project had no list; saving a
        // project-wide allow list afterwards must not pull it back in.
        let exempt = Snapshot::default().patched(&[], &[row(1, 7, Some(70), "disabled", &[])]);
        let with_list = exempt.patched(&[], &[row(2, 7, None, "allow", &["10.0.0.0/8"])]);
        assert!(
            with_list.effective(7, 70).is_none(),
            "the exempted environment stays unrestricted"
        );
        assert!(
            with_list.effective(7, 71).is_some(),
            "other environments follow the project list"
        );
    }

    #[test]
    fn a_full_load_does_not_depend_on_row_order() {
        let env_row = row(1, 7, Some(70), "disabled", &[]);
        let project_row = row(2, 7, None, "allow", &["10.0.0.0/8"]);
        let env_first = Snapshot::default().patched(&[], &[env_row.clone(), project_row.clone()]);
        let project_first = Snapshot::default().patched(&[], &[project_row, env_row]);
        for snapshot in [env_first, project_first] {
            assert!(snapshot.effective(7, 70).is_none());
            assert!(snapshot.effective(7, 71).is_some());
        }
    }

    #[test]
    fn removing_the_last_override_drops_the_project() {
        let before = Snapshot::default().patched(&[], &[row(1, 7, Some(70), "disabled", &[])]);
        assert!(!before.projects.is_empty());
        let after = before.patched(
            &[RemovedScope {
                project_id: 7,
                environment_id: Some(70),
            }],
            &[],
        );
        assert!(after.projects.is_empty());
    }

    #[test]
    fn a_scope_deleted_and_recreated_in_one_delta_keeps_the_new_row() {
        let before = Snapshot::default().patched(&[], &[row(1, 7, None, "allow", &["10.0.0.0/8"])]);
        let after = before.patched(
            &[RemovedScope {
                project_id: 7,
                environment_id: None,
            }],
            &[row(2, 7, None, "allow", &["192.168.0.0/16"])],
        );
        let policy = after.effective(7, 70).expect("recreated scope is enforced");
        assert!(policy.set.contains(ip("192.168.1.1")));
        assert!(!policy.set.contains(ip("10.1.1.1")));
    }

    #[tokio::test]
    async fn refresh_costs_one_read_while_nothing_changed() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            // hydrate: head, then rows
            .append_query_results(vec![vec![revision_row(4)]])
            .append_query_results(vec![vec![row(4, 7, None, "allow", &["10.0.0.0/8"])]])
            // poll: head only
            .append_query_results(vec![vec![revision_row(4)]])
            .into_connection();
        let db = Arc::new(db);
        let gate = ProjectIpAccessGate::new(Arc::clone(&db));
        gate.hydrate().await.expect("hydrate succeeds");

        let outcome = gate.refresh(false).await.expect("poll succeeds");
        assert!(matches!(outcome, Refresh::Unchanged));
        drop(gate);
        let log = Arc::try_unwrap(db)
            .map_err(|_| "the gate still holds the database handle")
            .expect("gate dropped its handle")
            .into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "head + rows at startup, head only on the poll"
        );
    }

    #[tokio::test]
    async fn deletions_and_changed_rows_are_read_inside_one_transaction() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![revision_row(1)]])
                .append_query_results(vec![vec![row(1, 7, None, "allow", &["10.0.0.0/8"])]])
                .append_query_results(vec![vec![revision_row(3)]])
                .append_query_results(vec![vec![removed_row(3, 7, Some(70))]])
                .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
                .into_connection(),
        );
        let gate = ProjectIpAccessGate::new(Arc::clone(&db));
        gate.hydrate().await.expect("hydrate succeeds");
        gate.refresh(false).await.expect("poll succeeds");
        drop(gate);

        let log = Arc::try_unwrap(db)
            .map_err(|_| "the gate still holds the database handle")
            .expect("gate dropped its handle")
            .into_transaction_log();
        let statements: Vec<String> = log
            .iter()
            .flat_map(|transaction| transaction.statements())
            .map(|statement| statement.sql.to_uppercase())
            .collect();
        let begin = statements
            .iter()
            .rposition(|sql| sql == "BEGIN")
            .unwrap_or_else(|| panic!("no BEGIN in {statements:#?}"));
        let tombstones = statements
            .iter()
            .position(|sql| sql.contains("PROJECT_IP_ACCESS_RULE_TOMBSTONES"))
            .expect("tombstones are read");
        let rules = statements
            .iter()
            .rposition(|sql| {
                sql.contains("FROM \"PROJECT_IP_ACCESS_RULES\"") && sql.contains("REVISION\" >")
            })
            .expect("changed rows are read");
        let commit = statements
            .iter()
            .rposition(|sql| sql == "COMMIT")
            .expect("the snapshot is closed");
        // Both reads sit between BEGIN and COMMIT. The mock does not record
        // the isolation level; the sqlx backend applies the
        // `REPEATABLE READ` / `READ ONLY` the delta asks for.
        assert!(begin < tombstones && tombstones < rules && rules < commit);
    }

    #[tokio::test]
    async fn refresh_applies_only_the_rows_and_deletions_above_the_applied_revision() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![revision_row(2)]])
            .append_query_results(vec![vec![
                row(1, 7, None, "allow", &["10.0.0.0/8"]),
                row(2, 8, None, "allow", &["192.168.0.0/16"]),
            ]])
            // poll: head moved; tombstones, then changed rows
            .append_query_results(vec![vec![revision_row(5)]])
            .append_query_results(vec![vec![removed_row(4, 8, None)]])
            .append_query_results(vec![vec![row(5, 7, None, "allow", &["172.16.0.0/12"])]])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.hydrate().await.expect("hydrate succeeds");
        assert!(gate.is_allowed(7, 70, ip("10.1.1.1")));
        assert!(!gate.is_allowed(8, 80, ip("10.1.1.1")));

        let outcome = gate.refresh(false).await.expect("poll succeeds");
        assert!(matches!(
            outcome,
            Refresh::Patched {
                changed: 1,
                removed: 1
            }
        ));
        // Edited project follows its new list; the deleted scope is open.
        assert!(!gate.is_allowed(7, 70, ip("10.1.1.1")));
        assert!(gate.is_allowed(7, 70, ip("172.16.1.1")));
        assert!(gate.is_allowed(8, 80, ip("10.1.1.1")));
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 5);
    }

    #[tokio::test]
    async fn a_failed_delta_keeps_the_snapshot_and_retries_from_the_same_revision() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![revision_row(1)]])
            .append_query_results(vec![vec![row(1, 7, None, "allow", &["10.0.0.0/8"])]])
            .append_query_results(vec![vec![revision_row(3)]])
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection lost".into())])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.hydrate().await.expect("hydrate succeeds");

        assert!(gate.refresh(false).await.is_err());
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 1);
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
    }

    #[tokio::test]
    async fn the_cursor_is_the_newest_revision_read_not_the_head() {
        // Head is 7 but only rows up to 5 are visible: revision 6 or 7 belongs
        // to a writer that has not committed. The cursor stays at 5 so the
        // next poll still sees it when it lands.
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![revision_row(7)]])
            .append_query_results(vec![vec![row(5, 7, None, "allow", &["10.0.0.0/8"])]])
            // poll 1: head still 7, nothing above 5 is committed yet
            .append_query_results(vec![vec![revision_row(7)]])
            .append_query_results(vec![no_tombstones()])
            .append_query_results(vec![Vec::<project_ip_access_rules::Model>::new()])
            // poll 2: the writer committed revision 7
            .append_query_results(vec![vec![revision_row(7)]])
            .append_query_results(vec![no_tombstones()])
            .append_query_results(vec![vec![row(7, 8, None, "allow", &["192.168.0.0/16"])]])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.hydrate().await.expect("hydrate succeeds");
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 5);

        assert!(matches!(
            gate.refresh(false).await.expect("poll succeeds"),
            Refresh::Unchanged
        ));
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 5);
        assert!(gate.is_allowed(8, 80, ip("10.1.1.1")));

        assert!(matches!(
            gate.refresh(false).await.expect("poll succeeds"),
            Refresh::Patched {
                changed: 1,
                removed: 0
            }
        ));
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 7);
        assert!(!gate.is_allowed(8, 80, ip("10.1.1.1")));
    }

    #[tokio::test]
    async fn a_sequence_behind_the_cursor_forces_a_full_reload() {
        // A restore rewound the sequence; deltas keyed on the old cursor
        // would never fire again.
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![revision_row(9)]])
            .append_query_results(vec![vec![row(9, 7, None, "allow", &["10.0.0.0/8"])]])
            .append_query_results(vec![vec![revision_row(2)]])
            .append_query_results(vec![vec![row(2, 8, None, "allow", &["192.168.0.0/16"])]])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.hydrate().await.expect("hydrate succeeds");

        assert!(matches!(
            gate.refresh(false).await.expect("poll succeeds"),
            Refresh::Reloaded { rules: 1 }
        ));
        assert_eq!(gate.applied_revision.load(Ordering::Acquire), 2);
        assert!(gate.is_allowed(7, 70, ip("203.0.113.1")));
        assert!(!gate.is_allowed(8, 80, ip("203.0.113.1")));
    }

    #[tokio::test]
    async fn hydrate_publishes_rows_from_the_database() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![revision_row(1)]])
            .append_query_results(vec![vec![row(1, 7, None, "allow", &["10.0.0.0/8"])]])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        assert!(gate.is_allowed(7, 70, ip("203.0.113.1")));
        gate.hydrate().await.expect("hydrate succeeds");
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
        assert!(gate.is_allowed(7, 70, ip("10.0.0.1")));
    }

    #[tokio::test]
    async fn hydrate_treats_a_missing_table_as_no_rules() {
        let db = missing_table_db();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.hydrate()
            .await
            .expect("a pending migration means no rules, not a failed start");
        assert!(gate.is_allowed(7, 70, ip("203.0.113.1")));
    }

    fn missing_table_db() -> DatabaseConnection {
        sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "relation \"project_ip_access_rules\" does not exist".into(),
            )])
            .into_connection()
    }

    #[tokio::test]
    async fn hydrate_failure_is_reported_and_keeps_the_previous_snapshot() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection lost".into())])
            .into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.snapshot.store(Arc::new(
            Snapshot::default().patched(&[], &[row(1, 7, None, "allow", &["10.0.0.0/8"])]),
        ));

        let err = gate.hydrate().await.expect_err("hydrate must fail");
        assert!(err.to_string().contains("connection lost"));
        // Still enforcing the last good rules.
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
    }

    /// The change feed end to end against the real migration: the revision
    /// trigger on insert/update, the tombstone trigger on delete, and the
    /// gate's delta reads on top of them.
    #[tokio::test]
    async fn change_feed_triggers_drive_delta_refreshes_on_a_real_database() -> anyhow::Result<()> {
        use sea_orm::ConnectionTrait;
        use temps_database::test_utils::TestDatabase;

        let test_database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping IP access change feed test: Docker runtime unavailable");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let db = test_database.db.clone();
        let ops = crate::test_utils::TestDBMockOperations::new(db.clone())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (project, environment, _) = ops
            .create_test_project()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let exec = |sql: String| {
            let db = db.clone();
            async move {
                db.execute(Statement::from_string(DatabaseBackend::Postgres, sql))
                    .await
            }
        };

        let gate = ProjectIpAccessGate::new(db.clone());
        gate.hydrate().await?;
        let outside = ip("203.0.113.9");
        assert!(gate.is_allowed(project.id, environment.id, outside));
        assert!(matches!(gate.refresh(false).await?, Refresh::Unchanged));

        // Insert: picked up by the next poll, as one changed row.
        exec(format!(
            "INSERT INTO project_ip_access_rules (project_id, mode, cidrs) \
             VALUES ({}, 'allow', '[{{\"cidr\": \"10.0.0.0/8\"}}]'::jsonb)",
            project.id
        ))
        .await?;
        assert!(matches!(
            gate.refresh(false).await?,
            Refresh::Patched {
                changed: 1,
                removed: 0
            }
        ));
        assert!(!gate.is_allowed(project.id, environment.id, outside));
        assert!(matches!(gate.refresh(false).await?, Refresh::Unchanged));

        // A manual UPDATE (no service involved) is a change too.
        exec(format!(
            "UPDATE project_ip_access_rules SET mode = 'deny', \
             cidrs = '[{{\"cidr\": \"198.51.100.0/24\"}}]'::jsonb WHERE project_id = {}",
            project.id
        ))
        .await?;
        assert!(matches!(
            gate.refresh(false).await?,
            Refresh::Patched {
                changed: 1,
                removed: 0
            }
        ));
        assert!(gate.is_allowed(project.id, environment.id, outside));
        assert!(!gate.is_allowed(project.id, environment.id, ip("198.51.100.7")));

        // Delete: a tombstone reopens the scope without a rule scan.
        exec(format!(
            "DELETE FROM project_ip_access_rules WHERE project_id = {}",
            project.id
        ))
        .await?;
        assert!(matches!(
            gate.refresh(false).await?,
            Refresh::Patched {
                changed: 0,
                removed: 1
            }
        ));
        assert!(gate.is_allowed(project.id, environment.id, ip("198.51.100.7")));
        assert!(!gate.has_active_policy(project.id, environment.id));
        assert!(matches!(gate.refresh(false).await?, Refresh::Unchanged));

        // A forced full reload agrees with the patched snapshot.
        assert!(matches!(
            gate.refresh(true).await?,
            Refresh::Reloaded { rules: 0 }
        ));
        Ok(())
    }
}
