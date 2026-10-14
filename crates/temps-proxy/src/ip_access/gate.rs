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
//! A background task polls a `count(*)` + `max(updated_at)` fingerprint every
//! [`POLL_INTERVAL`] and rebuilds the snapshot only when it changed, with a
//! full reload every [`FULL_RELOAD_INTERVAL`] as a floor (a transaction that
//! commits late can carry an `updated_at` below the fingerprint's maximum).
//! A rule change is therefore enforced within [`POLL_INTERVAL`] in every
//! process, including a standalone `temps proxy`.
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
use sea_orm::{DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, Statement};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::ProjectIpGate;
use temps_entities::project_ip_access_rules;
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// How often the change fingerprint is read. One aggregate over a tiny table.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Floor between full snapshot rebuilds even when the fingerprint is unchanged.
pub const FULL_RELOAD_INTERVAL: Duration = Duration::from_secs(60);

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

#[derive(Debug, Default)]
struct ProjectPolicies {
    /// Project-wide policy, if one is active.
    project: Option<Policy>,
    /// Environment overrides sorted by environment id. `None` is an explicit
    /// "unrestricted", which exempts the environment from `project`.
    environments: Vec<(i32, Option<Policy>)>,
}

#[derive(Debug, Default)]
struct Snapshot {
    projects: HashMap<i32, ProjectPolicies>,
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
            Ok(index) => scopes.environments[index].1.as_ref(),
            Err(_) => scopes.project.as_ref(),
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct FingerprintRow {
    rule_count: i64,
    latest_update: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    rule_count: i64,
    latest_update: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct ProjectIpAccessGate {
    db: Arc<DatabaseConnection>,
    snapshot: ArcSwap<Snapshot>,
}

impl ProjectIpAccessGate {
    /// An empty gate. Call [`Self::hydrate`] before handing it to the proxy.
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
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
        gate.snapshot.store(Arc::new(build_snapshot(rows)));
        gate
    }

    pub async fn hydrate(&self) -> Result<(), IpAccessGateError> {
        match self.reload().await {
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
        let mut last_fingerprint = self.read_fingerprint().await.ok();
        let mut last_full_reload = Instant::now();
        let mut consecutive_failures: u64 = 0;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let outcome = async {
                let fingerprint = self.read_fingerprint().await?;
                let changed = last_fingerprint != Some(fingerprint);
                if changed || last_full_reload.elapsed() >= FULL_RELOAD_INTERVAL {
                    let loaded = self.reload().await?;
                    last_full_reload = Instant::now();
                    // Record the fingerprint the rebuild was *at least as new
                    // as*; if a write raced in between, the next poll sees it.
                    last_fingerprint = Some(fingerprint);
                    if changed {
                        info!(rules = loaded, "Project IP access rules reloaded");
                    }
                }
                Ok::<(), IpAccessGateError>(())
            }
            .await;

            match outcome {
                Ok(()) => {
                    if consecutive_failures > 0 {
                        info!(
                            failed_polls = consecutive_failures,
                            "Project IP access rule refresh recovered"
                        );
                        consecutive_failures = 0;
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
        }
    }

    async fn read_fingerprint(&self) -> Result<Fingerprint, IpAccessGateError> {
        let row = FingerprintRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS rule_count, MAX(updated_at) AS latest_update \
             FROM project_ip_access_rules",
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| IpAccessGateError::Database {
            operation: "read the change fingerprint of",
            source,
        })?;
        Ok(match row {
            Some(row) => Fingerprint {
                rule_count: row.rule_count,
                latest_update: row.latest_update,
            },
            None => Fingerprint {
                rule_count: 0,
                latest_update: None,
            },
        })
    }

    /// Rebuild and publish the snapshot. Returns the number of rows loaded.
    async fn reload(&self) -> Result<usize, IpAccessGateError> {
        let rows = project_ip_access_rules::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|source| IpAccessGateError::Database {
                operation: "load",
                source,
            })?;
        let count = rows.len();
        self.snapshot.store(Arc::new(build_snapshot(rows)));
        debug!(rules = count, "Published project IP access snapshot");
        Ok(count)
    }
}

fn is_missing_table(error: &sea_orm::DbErr) -> bool {
    let message = error.to_string();
    message.contains("project_ip_access_rules") && message.contains("does not exist")
}

fn build_snapshot(rows: Vec<project_ip_access_rules::Model>) -> Snapshot {
    let mut projects: HashMap<i32, ProjectPolicies> = HashMap::new();
    for row in rows {
        let policy = compile_policy(&row);
        let scopes = projects.entry(row.project_id).or_default();
        match row.environment_id {
            None => scopes.project = policy,
            Some(environment_id) => scopes.environments.push((environment_id, policy)),
        }
    }
    for scopes in projects.values_mut() {
        scopes.environments.sort_unstable_by_key(|(id, _)| *id);
    }
    // Projects whose only rows compile to "unrestricted" are dropped so the
    // hot-path miss is a single failed hash lookup.
    projects.retain(|_, scopes| scopes.project.is_some() || !scopes.environments.is_empty());
    Snapshot { projects }
}

/// Compile one stored row. Returns `None` for a scope that enforces nothing.
///
/// Entries that fail to parse can only come from a manual edit of the table
/// (the service canonicalises on write). They are skipped and logged; for an
/// allow list that fails closed (fewer networks are admitted).
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
            return Some(Policy {
                mode: IpAccessMode::Allow,
                set: CidrSet::default(),
            });
        }
    };
    if mode == IpAccessMode::Disabled {
        return None;
    }

    let mut networks = Vec::with_capacity(row.cidrs.0.len());
    for entry in &row.cidrs.0 {
        match parse_cidr(&entry.cidr) {
            Ok(network) => networks.push(network),
            Err(e) => warn!(
                rule_id = row.id,
                project_id = row.project_id,
                error = %e,
                "Skipping unparsable CIDR in project IP access rule"
            ),
        }
    }
    let set = CidrSet::from_networks(&networks);

    // A deny list that names nothing blocks nothing: report no policy so an
    // unresolvable client IP is not treated as a denial for it.
    if mode == IpAccessMode::Deny && set.is_empty() {
        return None;
    }
    Some(Policy { mode, set })
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
            created_at: now,
            updated_at: now,
        }
    }

    fn gate_with(rows: Vec<project_ip_access_rules::Model>) -> ProjectIpAccessGate {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let gate = ProjectIpAccessGate::new(Arc::new(db));
        gate.snapshot.store(Arc::new(build_snapshot(rows)));
        gate
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

    #[tokio::test]
    async fn hydrate_publishes_rows_from_the_database() {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
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
        gate.snapshot.store(Arc::new(build_snapshot(vec![row(
            1,
            7,
            None,
            "allow",
            &["10.0.0.0/8"],
        )])));

        let err = gate.hydrate().await.expect_err("hydrate must fail");
        assert!(err.to_string().contains("connection lost"));
        // Still enforcing the last good rules.
        assert!(!gate.is_allowed(7, 70, ip("203.0.113.1")));
    }
}
