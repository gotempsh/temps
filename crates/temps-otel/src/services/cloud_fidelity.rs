// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project egress policy for the optional Temps Cloud telemetry mirror
//! (ADR-040 §1).
//!
//! Two columns on `projects` decide how much of a span may leave this
//! instance: `cloud_telemetry_fidelity` and
//! `cloud_telemetry_attribute_allowlist`. Both live on the project row rather
//! than in configuration, so they are per-project, changeable at runtime, and
//! audit-logged for free.
//!
//! # Why this is a cache and not a query per span
//!
//! `OtelService::ingest_spans` runs on the ingest path with a permit held. A
//! `SELECT` per span — or even per batch — would put a Postgres round trip
//! between an exporter and its acknowledgement, for a value that changes when
//! an operator clicks a toggle, i.e. approximately never. [`CloudPolicyCache`]
//! resolves one entry per *distinct project in the batch* and reuses it for
//! [`CLOUD_POLICY_CACHE_TTL`].
//!
//! # Why every failure resolves to `Metered` *and* `Local`
//!
//! A missing project row, a database error, or an unwired cache all yield
//! [`CloudTelemetryPolicy::metered`], which is `Metered` fidelity **and**
//! `Local` writes (ADR-041 §1). Both halves fail in the safe direction, and
//! they are different directions:
//!
//! - Failing to `Metered` means *less egress*. The worst case is that a project
//!   which opted in keeps mirroring the pre-ADR-040 projection for up to one
//!   TTL, which is a recoverable gap. The opposite failure — shipping real span
//!   names because a lookup errored — cannot be undone once the bytes have
//!   left.
//! - Failing to `Local` means *more storage*. The worst case is that a
//!   Cloud-primary project writes some spans locally that it did not need to,
//!   which costs disk. The opposite failure — treating an unresolvable project
//!   as Cloud-primary — would silently stop storing its spans anywhere on this
//!   instance, and that is unrecoverable once the window has passed.
//!
//! An unresolvable project therefore always ends up in the state that stores
//! more and sends less, never the reverse.
//!
//! # Why one caller does *not* get that collapse
//!
//! Collapsing "this project does not exist" into "metered" is right on the
//! ingest path and wrong at an operator's terminal: it turns a mistyped
//! `--project` into advice to raise a fidelity setting on a project that is not
//! there. [`CloudPolicyCache::resolve_project`] keeps the three outcomes apart
//! for callers that can act on the difference.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};
use temps_core::DBDateTime;
use temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use temps_entities::projects;
use tracing::warn;

/// Why a single-project policy lookup could not produce an answer.
///
/// Only [`CloudPolicyCache::resolve_project`] returns these. The batch lookup
/// the ingest path uses deliberately has no error type at all — see the module
/// docs.
#[derive(Debug, thiserror::Error)]
pub enum CloudPolicyError {
    #[error(
        "Project {project_id} does not exist on this instance — it was never created, or it \
         has been deleted. Check the project id (the Console shows it in the project's URL); \
         there is no telemetry fidelity to read or raise for it."
    )]
    ProjectNotFound { project_id: i32 },

    #[error(
        "Failed to read the Temps Cloud telemetry fidelity for project {project_id}: {source}"
    )]
    Lookup {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// How long a resolved per-project policy stays valid.
///
/// Short enough that raising or lowering fidelity in the UI takes effect
/// without a restart (CLAUDE.md: an operator must not have to restart the
/// binary to change one project's behaviour), long enough that a busy ingest
/// path is not issuing a project lookup per batch.
pub const CLOUD_POLICY_CACHE_TTL: Duration = Duration::from_secs(30);

/// The resolved, per-project answer to "what may leave this instance for this
/// project's spans".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudTelemetryPolicy {
    /// Which projection [`crate::services::otel_service::cloud_span`] builds.
    pub fidelity: CloudTelemetryFidelity,
    /// Exact-match attribute keys permitted at `Queryable` fidelity. Empty
    /// means no attributes leave — the default, and the safe one.
    ///
    /// `Arc` so a cache hit clones a pointer rather than the key set on the
    /// ingest path.
    pub attribute_allowlist: Arc<BTreeSet<String>>,
    /// ADR-041 §1: whether this project's spans are written to local storage at
    /// all.
    ///
    /// Carried on the same lookup, behind the same TTL, and with the **same
    /// fail-safe direction** as `fidelity`: a project that cannot be resolved
    /// is [`CloudTelemetryWriteMode::Local`], so a lookup failure can only ever
    /// be safer. The opposite default would mean a database blip silently
    /// stopped storing a project's spans, which is unrecoverable once the
    /// window has passed — whereas failing to `Local` merely writes spans to a
    /// store that already exists.
    pub write_mode: CloudTelemetryWriteMode,
    /// ADR-043 §1: whether this project's non-span telemetry (metrics first,
    /// under Phase C1) is written to local storage at all. Independent of
    /// `write_mode` -- a project may be Cloud-primary for spans and local for
    /// analytics, or vice versa, during an incremental cutover. Same
    /// fail-safe direction as `write_mode`: an unresolved project is
    /// `CloudAnalyticsWriteMode::Local`.
    pub analytics_write_mode: CloudAnalyticsWriteMode,
}

impl Default for CloudTelemetryPolicy {
    fn default() -> Self {
        Self::metered()
    }
}

impl CloudTelemetryPolicy {
    /// The default and the fallback: today's pre-ADR-040 projection, written to
    /// local storage exactly as it always was.
    pub fn metered() -> Self {
        Self {
            fidelity: CloudTelemetryFidelity::Metered,
            attribute_allowlist: Arc::new(BTreeSet::new()),
            write_mode: CloudTelemetryWriteMode::Local,
            analytics_write_mode: CloudAnalyticsWriteMode::Local,
        }
    }

    /// Opt-in fidelity with an explicit attribute allowlist.
    ///
    /// Passing an empty iterator is meaningful and supported: the span becomes
    /// renderable while still shipping zero attributes.
    pub fn queryable(allowlist: impl IntoIterator<Item = String>) -> Self {
        Self {
            fidelity: CloudTelemetryFidelity::Queryable,
            attribute_allowlist: Arc::new(allowlist.into_iter().collect()),
            write_mode: CloudTelemetryWriteMode::Local,
            analytics_write_mode: CloudAnalyticsWriteMode::Local,
        }
    }

    /// The same policy with Cloud-primary writes.
    ///
    /// Only meaningful at `Queryable` fidelity — the §1 gate makes the
    /// `Metered` combination unreachable through every write path — so this is
    /// a builder on top of [`Self::queryable`] rather than a free constructor
    /// that could produce the forbidden pair.
    pub fn cloud_primary(mut self) -> Self {
        self.write_mode = CloudTelemetryWriteMode::Cloud;
        self
    }

    /// The same policy with Cloud-primary analytics writes (ADR-043 §1) —
    /// independent of [`Self::cloud_primary`], which only sets the span
    /// switch.
    pub fn analytics_cloud_primary(mut self) -> Self {
        self.analytics_write_mode = CloudAnalyticsWriteMode::Cloud;
        self
    }

    /// Whether spans for this project skip local storage entirely.
    ///
    /// Defence in depth against the one state that must be unreachable: even if
    /// a row somehow carried `cloud` at `metered` fidelity — a hand-written
    /// `UPDATE` against a database whose CHECK constraint was dropped, a
    /// restored dump from a build that predates the constraint — the ingest
    /// path treats it as `Local` and stores the spans, rather than discarding
    /// them locally and shipping unreadable placeholders. Failing towards *more
    /// storage* is the only safe direction here.
    pub fn is_cloud_primary(&self) -> bool {
        self.write_mode.is_cloud_primary() && self.fidelity.is_queryable()
    }

    /// Whether this project's non-span telemetry (metrics under Phase C1)
    /// skips local storage entirely. Same defence-in-depth reasoning as
    /// [`Self::is_cloud_primary`], applied to the analytics switch.
    pub fn is_analytics_cloud_primary(&self) -> bool {
        self.analytics_write_mode.is_cloud_primary() && self.fidelity.is_queryable()
    }

    /// Whether `key` may be mirrored.
    ///
    /// Exact match only. No prefix, suffix or glob semantics — a pattern
    /// language here would let a single `http.*` entry widen egress to
    /// whatever an instrumentation library decides to add next release.
    pub fn allows_attribute(&self, key: &str) -> bool {
        self.attribute_allowlist.contains(key)
    }

    fn from_project_row(row: &ProjectPolicyRow) -> Self {
        Self {
            fidelity: row.cloud_telemetry_fidelity,
            attribute_allowlist: Arc::new(
                row.cloud_telemetry_attribute_allowlist
                    .iter()
                    .cloned()
                    .collect(),
            ),
            write_mode: row.cloud_telemetry_write_mode,
            analytics_write_mode: row.cloud_analytics_write_mode,
        }
    }
}

/// Only the columns the projection needs, so the ingest path never pulls the
/// whole (wide) project row just to read a consent flag.
#[derive(Debug, Clone, sea_orm::FromQueryResult)]
struct ProjectPolicyRow {
    id: i32,
    cloud_telemetry_fidelity: CloudTelemetryFidelity,
    cloud_telemetry_attribute_allowlist: Vec<String>,
    cloud_telemetry_write_mode: CloudTelemetryWriteMode,
    cloud_analytics_write_mode: CloudAnalyticsWriteMode,
    /// Read, but only acted on by [`CloudPolicyCache::resolve_project`]: a
    /// soft-deleted project is "gone" to an operator naming it on a command
    /// line, while the ingest path has no reason to care — spans still arriving
    /// for it are still spans it was told to mirror.
    deleted_at: Option<DBDateTime>,
}

struct CacheEntry {
    policy: CloudTelemetryPolicy,
    resolved_at: Instant,
}

/// TTL cache of per-project [`CloudTelemetryPolicy`] values.
pub struct CloudPolicyCache {
    db: Arc<DatabaseConnection>,
    ttl: Duration,
    entries: Mutex<HashMap<i32, CacheEntry>>,
}

impl CloudPolicyCache {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::with_ttl(db, CLOUD_POLICY_CACHE_TTL)
    }

    pub fn with_ttl(db: Arc<DatabaseConnection>, ttl: Duration) -> Self {
        Self {
            db,
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the policy for every distinct project id in `project_ids`.
    ///
    /// Never returns an error and never propagates one: an unresolvable
    /// project is simply absent from the returned map, and callers treat
    /// absence as [`CloudTelemetryPolicy::metered`]. Telemetry ingest must not
    /// fail because a consent lookup did.
    pub async fn policies_for(
        &self,
        project_ids: impl IntoIterator<Item = i32>,
    ) -> HashMap<i32, CloudTelemetryPolicy> {
        let mut resolved: HashMap<i32, CloudTelemetryPolicy> = HashMap::new();
        let mut missing: Vec<i32> = Vec::new();

        {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            for project_id in project_ids {
                if resolved.contains_key(&project_id) {
                    continue;
                }
                match entries.get(&project_id) {
                    Some(entry) if entry.resolved_at.elapsed() < self.ttl => {
                        resolved.insert(project_id, entry.policy.clone());
                    }
                    _ => {
                        if !missing.contains(&project_id) {
                            missing.push(project_id);
                        }
                    }
                }
            }
        }

        if missing.is_empty() {
            return resolved;
        }

        let rows = match self.fetch_policy_rows(&missing).await {
            Ok(rows) => rows,
            Err(error) => {
                // Fail towards less egress: every unresolved project keeps the
                // `Metered` projection until the next attempt succeeds.
                warn!(
                    projects = ?missing,
                    error = %error,
                    "Could not resolve Cloud telemetry fidelity; mirroring these \
                     projects at `metered` fidelity until the lookup succeeds"
                );
                return resolved;
            }
        };

        for (project_id, policy) in self.cache_rows(&rows) {
            resolved.insert(project_id, policy);
        }

        resolved
    }

    /// Convenience single-project lookup, used by the ingest-adjacent callers
    /// that share the batch path's "any failure means `Metered`" contract.
    pub async fn policy_for(&self, project_id: i32) -> CloudTelemetryPolicy {
        self.policies_for([project_id])
            .await
            .remove(&project_id)
            .unwrap_or_default()
    }

    /// Resolve one project's policy, keeping "no such project" and "the lookup
    /// failed" distinct from a genuine `Metered` answer.
    ///
    /// [`Self::policy_for`] collapses all three into `Metered`, which is
    /// correct for ingest (every one of them means "ship less") and actively
    /// misleading anywhere an operator reads the result: a mistyped or deleted
    /// `--project` would be reported as a fidelity-configuration problem, and
    /// send them looking for a settings toggle on a project that is not there.
    /// Callers that can act on the difference — the Cloud telemetry backfill
    /// command — use this instead.
    ///
    /// Deliberately bypasses the cache: this runs once per operator command,
    /// never on a hot path, and a cached entry cannot answer "does this project
    /// still exist" — only the row can.
    pub async fn resolve_project(
        &self,
        project_id: i32,
    ) -> Result<CloudTelemetryPolicy, CloudPolicyError> {
        let rows = self
            .fetch_policy_rows(&[project_id])
            .await
            .map_err(|source| CloudPolicyError::Lookup { project_id, source })?;
        self.cache_rows(&rows);

        rows.iter()
            .find(|row| row.id == project_id && row.deleted_at.is_none())
            .map(CloudTelemetryPolicy::from_project_row)
            .ok_or(CloudPolicyError::ProjectNotFound { project_id })
    }

    /// The one query this cache issues. Errors are propagated here and handled
    /// differently by each caller, which is the whole point of splitting it out.
    async fn fetch_policy_rows(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<ProjectPolicyRow>, sea_orm::DbErr> {
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::CloudTelemetryFidelity)
            .column(projects::Column::CloudTelemetryAttributeAllowlist)
            .column(projects::Column::CloudTelemetryWriteMode)
            .column(projects::Column::CloudAnalyticsWriteMode)
            .column(projects::Column::DeletedAt)
            .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
            .into_model::<ProjectPolicyRow>()
            .all(self.db.as_ref())
            .await
    }

    /// Store freshly-read rows and hand back the policies they resolved to.
    fn cache_rows(&self, rows: &[ProjectPolicyRow]) -> Vec<(i32, CloudTelemetryPolicy)> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // Prune before inserting so a long-lived process cannot accumulate an
        // entry per project that ever sent a span.
        entries.retain(|_, entry| entry.resolved_at.elapsed() < self.ttl * 2);

        rows.iter()
            .map(|row| {
                let policy = CloudTelemetryPolicy::from_project_row(row);
                entries.insert(
                    row.id,
                    CacheEntry {
                        policy: policy.clone(),
                        resolved_at: now,
                    },
                );
                (row.id, policy)
            })
            .collect()
    }

    /// Forget every cached decision.
    ///
    /// Used after an operator edits fidelity so the change is visible on the
    /// very next batch rather than up to one TTL later.
    pub fn invalidate_all(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Forget the cached decision for one project.
    pub fn invalidate(&self, project_id: i32) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&project_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::ArrayType;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, Value};
    use std::collections::BTreeMap;

    /// One mocked `projects` row carrying only the columns
    /// `ProjectPolicyRow` reads back out of.
    fn row(
        id: i32,
        fidelity: CloudTelemetryFidelity,
        allowlist: &[&str],
    ) -> BTreeMap<String, Value> {
        row_with_mode(id, fidelity, allowlist, CloudTelemetryWriteMode::Local)
    }

    fn row_with_mode(
        id: i32,
        fidelity: CloudTelemetryFidelity,
        allowlist: &[&str],
        write_mode: CloudTelemetryWriteMode,
    ) -> BTreeMap<String, Value> {
        let mut row = BTreeMap::new();
        row.insert("id".to_string(), Value::Int(Some(id)));
        row.insert(
            "cloud_telemetry_fidelity".to_string(),
            Value::String(Some(Box::new(fidelity.to_string()))),
        );
        row.insert(
            "cloud_telemetry_attribute_allowlist".to_string(),
            Value::Array(
                ArrayType::String,
                Some(Box::new(
                    allowlist
                        .iter()
                        .map(|key| Value::String(Some(Box::new((*key).to_string()))))
                        .collect(),
                )),
            ),
        );
        row.insert(
            "cloud_telemetry_write_mode".to_string(),
            Value::String(Some(Box::new(write_mode.to_string()))),
        );
        row.insert(
            "cloud_analytics_write_mode".to_string(),
            Value::String(Some(Box::new(CloudAnalyticsWriteMode::Local.to_string()))),
        );
        row.insert("deleted_at".to_string(), Value::ChronoDateTimeUtc(None));
        row
    }

    /// The same row, soft-deleted.
    fn deleted_row(
        id: i32,
        fidelity: CloudTelemetryFidelity,
        allowlist: &[&str],
    ) -> BTreeMap<String, Value> {
        let mut row = row(id, fidelity, allowlist);
        row.insert(
            "deleted_at".to_string(),
            Value::ChronoDateTimeUtc(Some(Box::new(chrono::Utc::now()))),
        );
        row
    }

    fn cache_with(results: Vec<Vec<BTreeMap<String, Value>>>) -> CloudPolicyCache {
        cache_with_ttl(results, CLOUD_POLICY_CACHE_TTL)
    }

    fn cache_with_ttl(
        results: Vec<Vec<BTreeMap<String, Value>>>,
        ttl: Duration,
    ) -> CloudPolicyCache {
        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for batch in results {
            db = db.append_query_results(vec![batch]);
        }
        CloudPolicyCache::with_ttl(Arc::new(db.into_connection()), ttl)
    }

    #[test]
    fn the_default_policy_is_metered_with_no_attributes_and_local_writes() {
        let policy = CloudTelemetryPolicy::default();
        assert_eq!(policy.fidelity, CloudTelemetryFidelity::Metered);
        assert!(policy.attribute_allowlist.is_empty());
        assert_eq!(policy.write_mode, CloudTelemetryWriteMode::Local);
        assert!(!policy.is_cloud_primary());
    }

    // ── ADR-041 §1: the write mode fails safe in its own direction ───────

    #[tokio::test]
    async fn a_cloud_primary_project_resolves_with_its_write_mode() {
        let cache = cache_with(vec![vec![row_with_mode(
            7,
            CloudTelemetryFidelity::Queryable,
            &["http.route"],
            CloudTelemetryWriteMode::Cloud,
        )]]);

        let policy = cache.policy_for(7).await;
        assert_eq!(policy.write_mode, CloudTelemetryWriteMode::Cloud);
        assert!(policy.is_cloud_primary());
    }

    #[tokio::test]
    async fn an_unresolvable_project_writes_locally_rather_than_nowhere() {
        // The fail-safe direction that matters most. Reading this as
        // Cloud-primary would mean a database blip silently stopped storing a
        // project's spans anywhere on this instance.
        let cache = cache_with(vec![vec![]]);
        assert!(!cache.policy_for(404).await.is_cloud_primary());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Custom("connection reset".into())])
            .into_connection();
        let cache = CloudPolicyCache::new(Arc::new(db));
        let policy = cache.policy_for(7).await;
        assert_eq!(
            policy.write_mode,
            CloudTelemetryWriteMode::Local,
            "a failed lookup must never stop a project's spans being stored"
        );
    }

    #[tokio::test]
    async fn cloud_primary_at_metered_fidelity_is_treated_as_local() {
        // Structurally unreachable through every write path — the service gate
        // and a database CHECK both forbid it. This asserts the third line of
        // defence: if a row somehow carries the pair anyway (a restored dump
        // from a build predating the constraint, a manual UPDATE), the ingest
        // path stores the spans rather than discarding them locally and
        // shipping unreadable placeholders.
        let cache = cache_with(vec![vec![row_with_mode(
            7,
            CloudTelemetryFidelity::Metered,
            &[],
            CloudTelemetryWriteMode::Cloud,
        )]]);

        let policy = cache.policy_for(7).await;
        assert_eq!(policy.write_mode, CloudTelemetryWriteMode::Cloud);
        assert!(
            !policy.is_cloud_primary(),
            "a metered project must never bypass local storage"
        );
    }

    #[tokio::test]
    async fn the_write_mode_shares_the_fidelity_ttl_and_invalidation() {
        // One lookup carries both, so an operator flipping the write mode sees
        // it take effect on the same terms as a fidelity change — no restart,
        // no second cache to reason about.
        let cache = cache_with(vec![
            vec![row_with_mode(
                7,
                CloudTelemetryFidelity::Queryable,
                &[],
                CloudTelemetryWriteMode::Cloud,
            )],
            vec![row_with_mode(
                7,
                CloudTelemetryFidelity::Queryable,
                &[],
                CloudTelemetryWriteMode::Local,
            )],
        ]);

        assert!(cache.policy_for(7).await.is_cloud_primary());
        cache.invalidate(7);
        assert!(
            !cache.policy_for(7).await.is_cloud_primary(),
            "reverting to local must take effect without a restart"
        );
    }

    #[test]
    fn the_allowlist_is_exact_match_only() {
        let policy = CloudTelemetryPolicy::queryable(["http.route".to_string()]);
        assert!(policy.allows_attribute("http.route"));
        // A prefix, a suffix and a different case are all misses. If any of
        // these ever passed, one allowlist entry would silently widen egress
        // to whatever an instrumentation library adds next.
        assert!(!policy.allows_attribute("http"));
        assert!(!policy.allows_attribute("http.route.template"));
        assert!(!policy.allows_attribute("HTTP.ROUTE"));
        assert!(!policy.allows_attribute("db.statement"));
    }

    #[test]
    fn a_queryable_policy_with_an_empty_allowlist_permits_no_attribute() {
        let policy = CloudTelemetryPolicy::queryable(std::iter::empty());
        assert_eq!(policy.fidelity, CloudTelemetryFidelity::Queryable);
        assert!(!policy.allows_attribute("http.route"));
        assert!(!policy.allows_attribute(""));
    }

    #[tokio::test]
    async fn a_project_row_resolves_to_its_stored_policy() {
        let cache = cache_with(vec![vec![row(
            7,
            CloudTelemetryFidelity::Queryable,
            &["http.route", "http.method"],
        )]]);

        let policies = cache.policies_for([7]).await;
        let policy = policies.get(&7).expect("project 7 must resolve");

        assert_eq!(policy.fidelity, CloudTelemetryFidelity::Queryable);
        assert!(policy.allows_attribute("http.route"));
        assert!(policy.allows_attribute("http.method"));
        assert!(!policy.allows_attribute("db.statement"));
    }

    #[tokio::test]
    async fn a_project_that_does_not_resolve_is_absent_and_therefore_metered() {
        // Empty result set: the project was deleted between ingest and lookup.
        let cache = cache_with(vec![vec![]]);

        assert!(!cache.policies_for([404]).await.contains_key(&404));
        // The single-project helper materialises the same decision explicitly.
        let cache = cache_with(vec![vec![]]);
        assert_eq!(
            cache.policy_for(404).await,
            CloudTelemetryPolicy::metered(),
            "an unresolvable project must never widen egress"
        );
    }

    #[tokio::test]
    async fn a_database_error_resolves_to_metered_rather_than_failing_ingest() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Custom("connection reset".into())])
            .into_connection();
        let cache = CloudPolicyCache::new(Arc::new(db));

        assert_eq!(
            cache.policy_for(7).await,
            CloudTelemetryPolicy::metered(),
            "a failed consent lookup must fail towards less egress, not more"
        );
    }

    #[tokio::test]
    async fn a_cached_policy_is_reused_without_a_second_query() {
        // Only one result set is queued; a second query would return empty and
        // the assertion below would fail.
        let cache = cache_with(vec![vec![row(
            7,
            CloudTelemetryFidelity::Queryable,
            &["http.route"],
        )]]);

        let first = cache.policy_for(7).await;
        let second = cache.policy_for(7).await;

        assert_eq!(first, second);
        assert_eq!(second.fidelity, CloudTelemetryFidelity::Queryable);
    }

    #[tokio::test]
    async fn invalidation_forces_a_fresh_lookup() {
        let cache = cache_with(vec![
            vec![row(7, CloudTelemetryFidelity::Queryable, &["http.route"])],
            vec![row(7, CloudTelemetryFidelity::Metered, &[])],
        ]);

        assert_eq!(
            cache.policy_for(7).await.fidelity,
            CloudTelemetryFidelity::Queryable
        );
        cache.invalidate(7);
        assert_eq!(
            cache.policy_for(7).await.fidelity,
            CloudTelemetryFidelity::Metered,
            "lowering fidelity must take effect without a restart"
        );
    }

    #[tokio::test]
    async fn an_expired_entry_is_refetched() {
        let cache = cache_with_ttl(
            vec![
                vec![row(7, CloudTelemetryFidelity::Queryable, &["http.route"])],
                vec![row(7, CloudTelemetryFidelity::Metered, &[])],
            ],
            Duration::from_millis(1),
        );

        assert_eq!(
            cache.policy_for(7).await.fidelity,
            CloudTelemetryFidelity::Queryable
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            cache.policy_for(7).await.fidelity,
            CloudTelemetryFidelity::Metered
        );
    }

    // ── `resolve_project`: the three outcomes the ingest path collapses ──

    #[tokio::test]
    async fn resolving_a_missing_project_names_it_instead_of_reporting_metered() {
        // The whole point: `policy_for` would answer `Metered` here, and the
        // caller would tell the operator to raise the fidelity of a project
        // that does not exist.
        let cache = cache_with(vec![vec![]]);

        let error = cache
            .resolve_project(404)
            .await
            .expect_err("a missing project must not resolve to a policy");

        assert!(matches!(
            error,
            CloudPolicyError::ProjectNotFound { project_id: 404 }
        ));
        let message = error.to_string();
        assert!(message.contains("404"), "{message}");
        assert!(message.contains("does not exist"), "{message}");
    }

    #[tokio::test]
    async fn resolving_a_soft_deleted_project_reports_it_as_missing() {
        let cache = cache_with(vec![vec![deleted_row(
            7,
            CloudTelemetryFidelity::Queryable,
            &["http.route"],
        )]]);

        assert!(matches!(
            cache.resolve_project(7).await,
            Err(CloudPolicyError::ProjectNotFound { project_id: 7 })
        ));
    }

    #[tokio::test]
    async fn resolving_through_a_broken_database_reports_the_lookup_not_the_project() {
        // "The database is down" and "you typed the wrong id" have completely
        // different fixes; reporting either as the other wastes the operator's
        // time on the one thing that is not wrong.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![DbErr::Custom("connection reset".into())])
            .into_connection();
        let cache = CloudPolicyCache::new(Arc::new(db));

        let error = cache
            .resolve_project(7)
            .await
            .expect_err("a failed lookup must not be reported as a missing project");

        assert!(matches!(
            error,
            CloudPolicyError::Lookup { project_id: 7, .. }
        ));
        assert!(error.to_string().contains("project 7"), "{error}");
    }

    #[tokio::test]
    async fn resolving_an_existing_project_returns_its_stored_policy() {
        let cache = cache_with(vec![vec![row(
            7,
            CloudTelemetryFidelity::Queryable,
            &["http.route"],
        )]]);

        let policy = cache
            .resolve_project(7)
            .await
            .expect("an existing project must resolve");

        assert_eq!(policy.fidelity, CloudTelemetryFidelity::Queryable);
        assert!(policy.allows_attribute("http.route"));
    }

    #[tokio::test]
    async fn resolving_a_metered_project_is_a_policy_not_an_error() {
        // `Metered` is a real, valid answer — the caller refuses the backfill
        // with fidelity advice, which is correct *because* the project exists.
        let cache = cache_with(vec![vec![row(7, CloudTelemetryFidelity::Metered, &[])]]);

        let policy = cache
            .resolve_project(7)
            .await
            .expect("a metered project still resolves");

        assert_eq!(policy, CloudTelemetryPolicy::metered());
    }

    #[tokio::test]
    async fn distinct_projects_in_one_batch_resolve_in_a_single_query() {
        let cache = cache_with(vec![vec![
            row(1, CloudTelemetryFidelity::Queryable, &["http.route"]),
            row(2, CloudTelemetryFidelity::Metered, &[]),
        ]]);

        // Repeats collapse: the ingest path passes one id per span.
        let policies = cache.policies_for([1, 2, 1, 2, 1]).await;

        assert_eq!(policies.len(), 2);
        assert_eq!(
            policies[&1].fidelity,
            CloudTelemetryFidelity::Queryable,
            "one project opting in must not affect the other"
        );
        assert_eq!(policies[&2].fidelity, CloudTelemetryFidelity::Metered);
    }
}
