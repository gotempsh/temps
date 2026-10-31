// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Every project's edge rules, compiled and held in memory (ADR-071 §5).
//!
//! # Hot path
//!
//! [`EdgeRuleGate::rules_for`] is one `ArcSwap::load` (lock-free), one
//! `HashMap` lookup by project id and an `Arc` clone. A project without rules
//! (the common case) ends there. Evaluation itself is the engine's: no I/O,
//! no locks, and match counters are relaxed atomics.
//!
//! # Memory bound
//!
//! At most 100 rules per project, each at most 32 conditions with bounded
//! values (64 list entries of 256 bytes, or 1,000 CIDRs). A project at every
//! limit compiles to a few hundred kilobytes; a typical one to a few
//! kilobytes. Only projects with at least one enabled rule are held.
//!
//! # Freshness
//!
//! A trigger records, per project, the last value drawn from
//! `project_edge_rule_versions_seq` whenever any of its rules changed. Every
//! [`POLL_INTERVAL`] the gate reads the sequence head (one O(1) read) and,
//! only when it moved, reloads the projects whose version is above the one
//! it applied. Writers are serialised on an advisory lock until commit, so the
//! cursor can advance to the newest version actually read.
//!
//! # Failure modes
//!
//! * The initial load must succeed; [`EdgeRuleGate::start_blocking`] returns
//!   the error and startup aborts rather than serve projects without their
//!   rules.
//! * A later reload failure keeps the last good rules and logs.
//! * A stored rule that cannot be decoded or compiled (only possible through a
//!   manual edit) is left out of its project's program and logged at error
//!   level every reload; the project's other rules still apply.
//! * Match counters are drained every [`HIT_FLUSH_INTERVAL`] into one batched
//!   upsert. A failed flush keeps the counts and retries; counts of a process
//!   that exits between flushes are lost, which only affects statistics.

use super::definition_from_model;
use arc_swap::ArcSwap;
use sea_orm::{
    AccessMode, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, IsolationLevel, QueryFilter, QueryOrder, Statement, TransactionTrait,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use temps_edge_rules::{eval::needs_geo, Program, RuleDefinition};
use temps_entities::project_edge_rules;
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// How often the change feed head is read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How often match counters are written to `project_edge_rule_stats`.
pub const HIT_FLUSH_INTERVAL: Duration = Duration::from_secs(10);

/// Safety-net interval between full reloads, for edits that bypassed the
/// trigger.
pub const FULL_RELOAD_INTERVAL: Duration = Duration::from_secs(15 * 60);

const READ_HEAD_SQL: &str = "SELECT CASE WHEN is_called THEN last_value ELSE 0 END AS version \
     FROM project_edge_rule_versions_seq";

/// "Nothing loaded yet."
const NOT_LOADED: i64 = -1;

#[derive(Debug, Error)]
pub enum EdgeRuleGateError {
    #[error("Failed to {operation} project edge rules: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> EdgeRuleGateError {
    move |source| EdgeRuleGateError::Database { operation, source }
}

/// One project's compiled rules.
pub struct ProjectRules {
    pub program: Program,
    /// Some enabled rule tests country, continent, ASN or hosting network, so
    /// the request needs a GeoIP lookup.
    pub needs_geo: bool,
}

#[derive(Debug, FromQueryResult)]
struct VersionRow {
    version: i64,
}

#[derive(Debug, FromQueryResult)]
struct ChangedProject {
    project_id: i32,
    version: i64,
}

type Snapshot = HashMap<i32, Arc<ProjectRules>>;

pub struct EdgeRuleGate {
    db: Arc<DatabaseConnection>,
    snapshot: ArcSwap<Snapshot>,
    applied: AtomicI64,
    /// Counts taken from programs that were replaced before their counters
    /// were flushed, and counts whose flush failed. Control plane only.
    pending_hits: Mutex<HashMap<i32, u64>>,
}

impl EdgeRuleGate {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            snapshot: ArcSwap::from_pointee(HashMap::new()),
            applied: AtomicI64::new(NOT_LOADED),
            pending_hits: Mutex::new(HashMap::new()),
        }
    }

    /// Build a loaded gate and start its loops on `handle`'s runtime. Blocks
    /// on the initial load, so it must not be called from async code.
    pub fn start_blocking(
        db: Arc<DatabaseConnection>,
        handle: &tokio::runtime::Handle,
    ) -> Result<Arc<Self>, EdgeRuleGateError> {
        let gate = Arc::new(Self::new(db));
        handle.block_on(gate.hydrate())?;
        handle.spawn(Arc::clone(&gate).run_refresh_loop());
        handle.spawn(Arc::clone(&gate).run_flush_loop());
        Ok(gate)
    }

    /// The initial load. A missing table (migration not applied yet) is
    /// "no rules"; the refresh loop picks the table up once it exists.
    pub async fn hydrate(&self) -> Result<(), EdgeRuleGateError> {
        match self.refresh(true).await {
            Ok(_) => Ok(()),
            Err(EdgeRuleGateError::Database { source, .. }) if is_missing_table(&source) => {
                warn!(
                    "project_edge_rules does not exist yet (migration pending); enforcing no \
                     edge rules until it does"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// The project's rules, if it has any enabled ones. Lock-free.
    pub fn rules_for(&self, project_id: i32) -> Option<Arc<ProjectRules>> {
        self.snapshot.load().get(&project_id).cloned()
    }

    #[cfg(test)]
    pub(crate) fn with_rules_for_tests(rules: Vec<project_edge_rules::Model>) -> Self {
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let gate = Self::new(Arc::new(db));
        let mut by_project: HashMap<i32, Vec<project_edge_rules::Model>> = HashMap::new();
        for rule in rules {
            by_project.entry(rule.project_id).or_default().push(rule);
        }
        let snapshot = by_project
            .into_iter()
            .filter_map(|(id, rows)| compile_project(id, &rows).map(|p| (id, Arc::new(p))))
            .collect();
        gate.snapshot.store(Arc::new(snapshot));
        gate
    }

    async fn run_refresh_loop(self: Arc<Self>) {
        let mut last_full = Instant::now();
        let mut failures: u64 = 0;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let force_full = last_full.elapsed() >= FULL_RELOAD_INTERVAL;
            match self.refresh(force_full).await {
                Ok(changed) => {
                    if failures > 0 {
                        info!(failed_polls = failures, "Edge rule refresh recovered");
                        failures = 0;
                    }
                    if force_full {
                        last_full = Instant::now();
                    }
                    if changed > 0 {
                        info!(projects = changed, "Edge rules updated");
                    }
                }
                Err(e) => {
                    failures += 1;
                    if failures == 1 || failures.is_multiple_of(12) {
                        error!(
                            failed_polls = failures,
                            error = %e,
                            "Edge rule refresh failed; continuing to enforce the last loaded rules"
                        );
                    }
                }
            }
        }
    }

    async fn run_flush_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(HIT_FLUSH_INTERVAL).await;
            if let Err(e) = self.flush_hits().await {
                warn!(error = %e, "Failed to record edge rule match counts; will retry");
            }
        }
    }

    /// Moves every counter into `pending_hits`.
    fn collect_hits(&self) {
        let snapshot = self.snapshot.load();
        let mut drained = Vec::new();
        for rules in snapshot.values() {
            drained.extend(rules.program.drain_hits());
        }
        if drained.is_empty() {
            return;
        }
        if let Ok(mut pending) = self.pending_hits.lock() {
            for (rule_id, hits) in drained {
                *pending.entry(rule_id).or_default() += hits;
            }
        }
    }

    /// Writes the counted matches in one statement.
    pub async fn flush_hits(&self) -> Result<usize, EdgeRuleGateError> {
        self.collect_hits();
        let batch: Vec<(i32, u64)> = match self.pending_hits.lock() {
            Ok(mut pending) => pending.drain().collect(),
            Err(_) => return Ok(0),
        };
        if batch.is_empty() {
            return Ok(0);
        }
        // Integers only, so they are written into the statement directly.
        let mut values = String::new();
        for (i, (rule_id, hits)) in batch.iter().enumerate() {
            if i > 0 {
                values.push(',');
            }
            let _ = write!(values, "({rule_id},{})", (*hits).min(i64::MAX as u64));
        }
        // The join drops rules deleted since they were counted.
        let sql = format!(
            "INSERT INTO project_edge_rule_stats (rule_id, hits, last_hit_at) \
             SELECT v.rule_id, v.hits, NOW() FROM (VALUES {values}) AS v(rule_id, hits) \
             JOIN project_edge_rules r ON r.id = v.rule_id \
             ON CONFLICT (rule_id) DO UPDATE SET \
             hits = project_edge_rule_stats.hits + EXCLUDED.hits, last_hit_at = EXCLUDED.last_hit_at"
        );
        match self
            .db
            .execute(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
        {
            Ok(_) => Ok(batch.len()),
            Err(source) => {
                if let Ok(mut pending) = self.pending_hits.lock() {
                    for (rule_id, hits) in batch {
                        *pending.entry(rule_id).or_default() += hits;
                    }
                }
                Err(EdgeRuleGateError::Database {
                    operation: "record match counts of",
                    source,
                })
            }
        }
    }

    /// Brings the rules up to date; returns how many projects changed.
    async fn refresh(&self, force_full: bool) -> Result<usize, EdgeRuleGateError> {
        let head = VersionRow::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            READ_HEAD_SQL,
        ))
        .one(self.db.as_ref())
        .await
        .map_err(db("read the change version of"))?
        .map_or(0, |r| r.version);
        let applied = self.applied.load(Ordering::Acquire);
        let full = force_full || applied == NOT_LOADED || head < applied;
        if !full && head == applied {
            return Ok(0);
        }

        let txn = self
            .db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
            .map_err(db("open a snapshot to read"))?;
        let since = if full { i64::MIN } else { applied };
        let changed = ChangedProject::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT project_id, version FROM project_edge_rule_versions WHERE version > $1",
            [since.into()],
        ))
        .all(&txn)
        .await
        .map_err(db("read the changed projects of"))?;
        let Some(newest) = changed.iter().map(|c| c.version).max() else {
            // The head moved but nothing is visible yet (an uncommitted or
            // rolled-back writer). Keep the cursor.
            txn.commit().await.map_err(db("close the snapshot of"))?;
            if full {
                self.publish_full(HashMap::new());
                self.applied.store(0, Ordering::Release);
            }
            return Ok(0);
        };
        let mut query = project_edge_rules::Entity::find()
            .order_by_asc(project_edge_rules::Column::ProjectId)
            .order_by_asc(project_edge_rules::Column::Position)
            .order_by_asc(project_edge_rules::Column::Id);
        if !full {
            let ids: Vec<i32> = changed.iter().map(|c| c.project_id).collect();
            query = query.filter(project_edge_rules::Column::ProjectId.is_in(ids));
        }
        let rows = query.all(&txn).await.map_err(db("load"))?;
        txn.commit().await.map_err(db("close the snapshot of"))?;

        let mut by_project: HashMap<i32, Vec<project_edge_rules::Model>> = HashMap::new();
        for row in rows {
            by_project.entry(row.project_id).or_default().push(row);
        }
        let mut compiled: HashMap<i32, Option<Arc<ProjectRules>>> =
            changed.iter().map(|c| (c.project_id, None)).collect();
        for (project_id, rows) in &by_project {
            compiled.insert(
                *project_id,
                compile_project(*project_id, rows).map(Arc::new),
            );
        }
        let count = compiled.len();

        // Keep the counts of the programs about to be replaced.
        self.collect_hits();
        if full {
            self.publish_full(
                compiled
                    .into_iter()
                    .filter_map(|(id, rules)| rules.map(|r| (id, r)))
                    .collect(),
            );
        } else {
            let mut next: Snapshot = (**self.snapshot.load()).clone();
            for (project_id, rules) in compiled {
                match rules {
                    Some(rules) => {
                        next.insert(project_id, rules);
                    }
                    None => {
                        next.remove(&project_id);
                    }
                }
            }
            self.snapshot.store(Arc::new(next));
        }
        self.applied.store(newest.max(0), Ordering::Release);
        debug!(
            projects = count,
            applied = newest,
            full,
            "Published edge rules"
        );
        Ok(count)
    }

    fn publish_full(&self, snapshot: Snapshot) {
        self.snapshot.store(Arc::new(snapshot));
    }
}

/// Compiles one project's rows (in evaluation order). `None` when nothing is
/// enabled. Rules that cannot be decoded or compiled are left out and logged.
fn compile_project(project_id: i32, rows: &[project_edge_rules::Model]) -> Option<ProjectRules> {
    let mut definitions: Vec<RuleDefinition> = Vec::with_capacity(rows.len());
    for row in rows {
        match definition_from_model(row) {
            Ok(definition) => match Program::compile(std::slice::from_ref(&definition)) {
                Ok(_) => definitions.push(definition),
                Err(e) => error!(
                    project_id,
                    rule_id = row.id,
                    error = %e,
                    "Edge rule does not compile and is not enforced; save it again to fix it"
                ),
            },
            Err(e) => error!(
                project_id,
                rule_id = row.id,
                error = %e,
                "Edge rule cannot be read and is not enforced; save it again to fix it"
            ),
        }
    }
    if !definitions.iter().any(|d| d.enabled) {
        return None;
    }
    let needs_geo = needs_geo(&definitions);
    match Program::compile(&definitions) {
        Ok(program) => Some(ProjectRules { program, needs_geo }),
        Err(e) => {
            // Every rule compiled alone, so this cannot happen; keep enforcing
            // nothing new rather than panic.
            error!(project_id, error = %e, "Edge rules of the project failed to compile");
            None
        }
    }
}

fn is_missing_table(error: &sea_orm::DbErr) -> bool {
    let message = error.to_string();
    message.contains("does not exist") && message.contains("project_edge_rule")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::edge_rule_service::{
        tests::{block_admin, database, project},
        EdgeRuleService, UpdateEdgeRuleRequest,
    };
    use temps_edge_rules::{Decision, ParsedSample, SampleRequest};

    fn get(url: &str) -> ParsedSample {
        ParsedSample::new(SampleRequest {
            url: url.into(),
            ..Default::default()
        })
    }

    fn row(
        id: i32,
        project_id: i32,
        enabled: bool,
        expression: serde_json::Value,
    ) -> project_edge_rules::Model {
        project_edge_rules::Model {
            id,
            project_id,
            environment_id: None,
            position: id,
            name: format!("rule {id}"),
            enabled,
            mode: "enforce".into(),
            expression,
            action: serde_json::json!({"type": "block", "status": 403}),
            revision: 1,
            created_by: None,
            updated_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn admin_path() -> serde_json::Value {
        serde_json::json!({"combinator": "all", "children": [
            {"kind": "condition", "field": "path", "op": "starts_with", "values": ["/admin"]}
        ]})
    }

    #[test]
    fn only_projects_with_enabled_rules_are_held() {
        let gate = EdgeRuleGate::with_rules_for_tests(vec![
            row(1, 10, true, admin_path()),
            row(2, 11, false, admin_path()),
        ]);
        assert!(gate.rules_for(10).is_some());
        assert!(gate.rules_for(11).is_none(), "all rules disabled");
        assert!(gate.rules_for(12).is_none(), "no rules");
    }

    #[test]
    fn an_unreadable_rule_is_skipped_and_the_rest_still_apply() {
        let gate = EdgeRuleGate::with_rules_for_tests(vec![
            row(1, 10, true, serde_json::json!({"combinator": "sometimes"})),
            row(2, 10, true, admin_path()),
        ]);
        let rules = gate.rules_for(10).expect("the readable rule is held");
        assert_eq!(rules.program.len(), 1);
        let evaluation = rules.program.evaluate(&get("https://a.example.test/admin"));
        assert!(matches!(
            evaluation.decision,
            Decision::Block { rule_id: 2, .. }
        ));
    }

    #[test]
    fn geo_lookups_are_requested_only_when_a_rule_needs_them() {
        let geo = serde_json::json!({"combinator": "all", "children": [
            {"kind": "condition", "field": "country", "op": "in", "values": ["FR"]}
        ]});
        let gate = EdgeRuleGate::with_rules_for_tests(vec![
            row(1, 10, true, geo),
            row(2, 11, true, admin_path()),
        ]);
        assert!(gate.rules_for(10).map(|r| r.needs_geo).unwrap_or(false));
        assert!(!gate.rules_for(11).map(|r| r.needs_geo).unwrap_or(true));
    }

    #[tokio::test]
    async fn writes_reach_the_gate_through_the_change_feed() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        let service = EdgeRuleService::new(db.clone());
        let gate = EdgeRuleGate::new(db.clone());
        gate.hydrate()
            .await
            .unwrap_or_else(|e| panic!("hydrate: {e}"));
        let (project_id, _) = project(&test_db, "edge-feed").await;
        assert!(gate.rules_for(project_id).is_none());

        let rule = service
            .create(project_id, block_admin(), None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            gate.refresh(false).await.unwrap_or_else(|e| panic!("{e}")),
            1
        );
        let rules = gate.rules_for(project_id).expect("loaded after the poll");
        let evaluation = rules.program.evaluate(&get("https://a.example.test/Admin"));
        assert!(matches!(evaluation.decision, Decision::Block { .. }));

        // Nothing changed: no work.
        assert_eq!(
            gate.refresh(false).await.unwrap_or_else(|e| panic!("{e}")),
            0
        );

        // Hits are counted and flushed.
        assert_eq!(gate.flush_hits().await.unwrap_or_else(|e| panic!("{e}")), 1);
        let listed = service
            .get(project_id, rule.id)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(listed.hits, 1);
        assert!(listed.last_hit_at.is_some());

        // Disabling the only rule drops the project.
        service
            .update(
                project_id,
                rule.id,
                UpdateEdgeRuleRequest {
                    enabled: Some(false),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        gate.refresh(false).await.unwrap_or_else(|e| panic!("{e}"));
        assert!(gate.rules_for(project_id).is_none());

        // Deleting the project (cascade) is seen too.
        service
            .update(
                project_id,
                rule.id,
                UpdateEdgeRuleRequest {
                    enabled: Some(true),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        gate.refresh(false).await.unwrap_or_else(|e| panic!("{e}"));
        assert!(gate.rules_for(project_id).is_some());
        db.execute_unprepared(&format!("DELETE FROM projects WHERE id = {project_id}"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        gate.refresh(false).await.unwrap_or_else(|e| panic!("{e}"));
        assert!(gate.rules_for(project_id).is_none());

        // A fresh gate loads the current state in full.
        let fresh = EdgeRuleGate::new(db);
        fresh.hydrate().await.unwrap_or_else(|e| panic!("{e}"));
        assert!(fresh.rules_for(project_id).is_none());
    }
}
