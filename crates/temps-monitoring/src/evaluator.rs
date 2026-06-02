//! Alert evaluator — evaluates `monitoring_alert_rules` against the metrics
//! store every 30 seconds and fires/resolves alarms via [`AlarmService`].
//!
//! # Evaluation loop
//!
//! Every tick the evaluator:
//! 1. Loads all enabled, non-silenced rules from `monitoring_alert_rules`.
//! 2. Rules are grouped by `(source_kind, source_id)` so all metrics for a
//!    given source are fetched in a single [`MetricsStore::query_latest`] call.
//! 3. Compares each value against the threshold using the rule's comparator.
//! 4. If breaching:
//!    - Records `breach_start[rule_id] = Instant::now()` on first breach.
//!    - If elapsed ≥ `for_duration_secs`: fires the alarm via
//!      [`AlarmService::fire_alarm`] and records the returned alarm ID.
//! 5. If not breaching:
//!    - Clears `breach_start[rule_id]`.
//!    - If an alarm was previously fired for this rule, resolves it.
//!
//! # In-memory state
//!
//! Breach start times are stored in-memory only.  They are lost on restart,
//! which is acceptable because a restart naturally resets the breach window.
//! This prevents phantom alarms from firing immediately after a restart.
//!
//! `firing_alarms` is repopulated from the database on startup so that
//! already-firing alarms are not re-fired after a restart within the 5-minute
//! cooldown window.
//!
//! # Default rule seeding
//!
//! [`seed_default_rules`] inserts a standard set of alert rules for a newly
//! metrics-enabled service (Postgres, Redis) or deployment (containers).  It
//! is idempotent — it does nothing if rules already exist for the given
//! `service_id`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
#[cfg(test)]
use sea_orm::Set;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use temps_entities::monitoring_alert_rules;
use temps_metrics::{LatestQuery, MetricsStore, SourceKind};

use crate::alarm_service::{AlarmService, AlarmSeverity, AlarmStatus, AlarmType, FireAlarmRequest};

/// Interval between evaluation cycles.
const EVAL_INTERVAL_SECS: u64 = 30;

// FIXME(metrics-scale): Issue 7 (Security Review) — No per-project alert rule limit.
//
// A user may create an unlimited number of alert rules.  At 50,000 rules, the
// evaluation loop would issue 50,000 individual `query_latest` calls per 30s
// cycle, exhausting the DB connection pool and causing the server to fall
// behind indefinitely.
//
// Required fixes before GA:
//   1. Enforce a hard per-project limit (suggested: 100 rules per project) in
//      the alert rule creation handler via a COUNT(*) guard before INSERT.
//   2. Add a per-cycle timeout: if `run_cycle` takes more than 25 seconds,
//      log a warning and return early rather than letting cycles stack.
//   3. Add a `alert_evaluator_cycle_duration_ms` metric so an admin can detect
//      evaluator saturation before it affects availability.

/// Background alert evaluator.
///
/// Spawn via [`AlertEvaluator::start`].  All fields are cheaply cloneable
/// through their inner `Arc`-wrapped state.
pub struct AlertEvaluator {
    db: Arc<DatabaseConnection>,
    store: Arc<dyn MetricsStore>,
    alarm_service: Arc<AlarmService>,
    /// Tracks when each rule first entered a breaching state.
    /// Key: `rule_id`, Value: `Instant` of first observed breach.
    /// Lost on restart — that is intentional (prevents phantom alerts).
    breach_start: Arc<RwLock<HashMap<i32, Instant>>>,
    /// Tracks which alarm ID was fired for each rule so it can be resolved.
    /// Key: `rule_id`, Value: `alarm_id` returned by [`AlarmService::fire_alarm`].
    firing_alarms: Arc<RwLock<HashMap<i32, i32>>>,
}

/// Cached alarm context `(project_id, environment_id, deployment_id, service_id)`
/// for a rule.
///
/// Deployment-scoped rules carry real environment + deployment IDs and no
/// service. Service-scoped (database) rules carry only `service_id`; their
/// environment and deployment are `None` and the alarm row stores NULL for
/// both, matching the nullable FK shape on `alarms.environment_id`,
/// `alarms.deployment_id`, and `alarms.service_id`.
type AlarmContext = (i32, Option<i32>, Option<i32>, Option<i32>);

impl AlertEvaluator {
    /// Create a new evaluator.
    pub fn new(
        db: Arc<DatabaseConnection>,
        store: Arc<dyn MetricsStore>,
        alarm_service: Arc<AlarmService>,
    ) -> Self {
        Self {
            db,
            store,
            alarm_service,
            breach_start: Arc::new(RwLock::new(HashMap::new())),
            firing_alarms: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Run the evaluation loop forever.  Spawn this on a background task.
    ///
    /// On first invocation, already-firing alarms are loaded from the database
    /// so that the in-memory `firing_alarms` map is consistent after a restart.
    pub async fn start(self: Arc<Self>) {
        info!("AlertEvaluator started (interval: {}s)", EVAL_INTERVAL_SECS);

        // Populate firing_alarms from DB so a restart doesn't re-fire existing
        // alarms or lose track of alarms that need future resolution.
        if let Err(e) = self.load_firing_alarms_from_db().await {
            warn!("AlertEvaluator: failed to load firing alarms from DB on startup: {e}");
        }

        // Back-seed default alert rules for every metrics-enabled service.
        // The `seed_default_rules` upsert is ON CONFLICT DO NOTHING, so this
        // is a no-op for services that already have their defaults. It catches
        // services whose engine had no default seeds when metrics was first
        // toggled on (e.g. MongoDB services pre-dating the mongodb seed).
        if let Err(e) = self.backseed_default_rules_for_enabled_services().await {
            warn!("AlertEvaluator: failed to back-seed default rules on startup: {e}");
        }

        loop {
            if let Err(e) = self.run_cycle().await {
                error!("AlertEvaluator: cycle failed: {e}");
            }

            tokio::time::sleep(Duration::from_secs(EVAL_INTERVAL_SECS)).await;
        }
    }

    /// Load the set of currently-firing metric-threshold alarms from the DB and
    /// populate `firing_alarms` so a restart doesn't duplicate notifications.
    ///
    /// We load alarms whose `alarm_type` is one of the three metric threshold
    /// variants and whose `status` is `firing` or `acknowledged`.  The
    /// `rule_id` is stored in the alarm metadata JSON as `"rule_id"`.
    ///
    /// # TODO(metrics): Issue 9 (Scalability Review)
    ///
    /// This function issues 4 sequential DB queries with a write lock held
    /// across all of them.  At startup this is fine (only called once), but
    /// the write lock blocks any `handle_breach` calls that try to insert into
    /// `firing_alarms` until all 4 queries complete.
    ///
    /// Fix: replace the per-alarm-type loop with a single `IN (...)` query
    /// across all 4 alarm type strings, and release the write lock between
    /// the DB fetch and the HashMap population.
    async fn load_firing_alarms_from_db(&self) -> Result<(), String> {
        use temps_entities::alarms;

        let metric_types = [
            crate::alarm_service::AlarmType::DatabaseMetricThreshold.as_str(),
            crate::alarm_service::AlarmType::DeploymentMetricThreshold.as_str(),
            crate::alarm_service::AlarmType::ContainerMetricThreshold.as_str(),
            crate::alarm_service::AlarmType::NodeMetricThreshold.as_str(),
        ];

        for alarm_type_str in &metric_types {
            let rows = alarms::Entity::find()
                .filter(alarms::Column::AlarmType.eq(*alarm_type_str))
                .filter(
                    sea_orm::Condition::any()
                        .add(alarms::Column::Status.eq(AlarmStatus::Firing.as_str()))
                        .add(alarms::Column::Status.eq(AlarmStatus::Acknowledged.as_str())),
                )
                .all(self.db.as_ref())
                .await
                .map_err(|e| format!("load_firing_alarms_from_db: DB error: {e}"))?;

            let mut guard = self.firing_alarms.write().await;
            for alarm in rows {
                // Extract rule_id from metadata JSON: {"rule_id": <i32>, ...}
                if let Some(meta) = &alarm.metadata {
                    if let Some(rule_id) = meta.get("rule_id").and_then(|v| v.as_i64()) {
                        guard.insert(rule_id as i32, alarm.id);
                    }
                }
            }
        }

        info!(
            "AlertEvaluator: restored {} firing alarm(s) from DB",
            self.firing_alarms.read().await.len()
        );

        Ok(())
    }

    /// Back-seed default alert rules for every external service that has
    /// `metrics_enabled = true`. Idempotent — `seed_default_rules` uses
    /// `INSERT … ON CONFLICT DO NOTHING`, so services that already have their
    /// defaults seeded incur only a no-op upsert per rule.
    ///
    /// This exists to self-heal services whose engine had no default seeds
    /// when the user first toggled metrics on (e.g. MongoDB services pre-dating
    /// the mongodb seed). Without it the user must toggle metrics off and back
    /// on to pick up newly-added default rules.
    async fn backseed_default_rules_for_enabled_services(&self) -> Result<(), String> {
        use temps_entities::external_services;

        let services = external_services::Entity::find()
            .filter(external_services::Column::MetricsEnabled.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(|e| format!("backseed_default_rules_for_enabled_services: DB error: {e}"))?;

        let mut seeded = 0usize;
        for service in services {
            if let Err(e) =
                seed_default_rules(self.db.as_ref(), service.id, &service.service_type).await
            {
                warn!(
                    service_id = service.id,
                    engine = %service.service_type,
                    "AlertEvaluator: back-seed failed (non-fatal): {e}"
                );
            } else {
                seeded += 1;
            }
        }

        info!(
            "AlertEvaluator: back-seeded default alert rules for {} service(s)",
            seeded
        );

        Ok(())
    }

    /// Execute one evaluation cycle over all enabled, non-silenced rules.
    ///
    /// Rules are grouped by `(source_kind, source_id)` so metrics for each
    /// source are fetched in a single `query_latest` call instead of one per
    /// rule (avoids N+1 query pattern at scale).
    ///
    /// Alarm context (`project_id`, `environment_id`, `deployment_id`) is
    /// resolved once per rule at load time, eliminating per-breach DB lookups.
    async fn run_cycle(&self) -> Result<(), String> {
        let now_ts = Utc::now();

        // Load all enabled rules that are not currently silenced.
        let rules = monitoring_alert_rules::Entity::find()
            .filter(monitoring_alert_rules::Column::Enabled.eq(true))
            .filter(
                // silenced_until IS NULL  OR  silenced_until < NOW()
                sea_orm::Condition::any()
                    .add(monitoring_alert_rules::Column::SilencedUntil.is_null())
                    .add(monitoring_alert_rules::Column::SilencedUntil.lt(now_ts)),
            )
            .all(self.db.as_ref())
            .await
            .map_err(|e| format!("AlertEvaluator: failed to load rules: {e}"))?;

        if rules.is_empty() {
            debug!("AlertEvaluator: no enabled rules to evaluate");
            return Ok(());
        }

        debug!("AlertEvaluator: evaluating {} rule(s)", rules.len());

        // Prune stale in-memory state for rules that no longer exist.
        // This prevents unbounded growth when rules are deleted while firing.
        let active_ids: HashSet<i32> = rules.iter().map(|r| r.id).collect();
        {
            let mut bs = self.breach_start.write().await;
            bs.retain(|k, _| active_ids.contains(k));
        }
        {
            let mut fa = self.firing_alarms.write().await;
            fa.retain(|k, _| active_ids.contains(k));
        }

        // Resolve alarm context (project/env/deployment IDs) for all rules
        // upfront so `handle_breach` and `handle_recovery` don't need to query
        // the DB individually for each firing/recovering rule.
        let mut context_cache: HashMap<i32, AlarmContext> = HashMap::with_capacity(rules.len());
        for rule in &rules {
            let ctx = self.resolve_alarm_context(rule).await;
            context_cache.insert(rule.id, ctx);
        }

        // Group rules by (source_kind, source_id) to batch query_latest calls.
        // Key: (source_kind_str, source_id), Value: list of rules for that source.
        let mut source_groups: HashMap<(String, i32), Vec<&monitoring_alert_rules::Model>> =
            HashMap::new();
        let mut invalid_rules: Vec<i32> = Vec::new();

        for rule in &rules {
            match (rule.service_id, rule.deployment_id) {
                (Some(svc_id), None) => {
                    source_groups
                        .entry((SourceKind::Database.as_str().to_string(), svc_id))
                        .or_default()
                        .push(rule);
                }
                (None, Some(dep_id)) => {
                    source_groups
                        .entry((SourceKind::Deployment.as_str().to_string(), dep_id))
                        .or_default()
                        .push(rule);
                }
                _ => {
                    invalid_rules.push(rule.id);
                }
            }
        }

        for rule_id in invalid_rules {
            warn!(
                rule_id,
                "AlertEvaluator: rule has invalid target (both or neither service_id/deployment_id set); skipping"
            );
        }

        // Evaluate each source group with a single query_latest call.
        for ((source_kind_str, source_id), group) in &source_groups {
            let source_kind = match source_kind_str.as_str() {
                "database" => SourceKind::Database,
                "deployment" => SourceKind::Deployment,
                "container" => SourceKind::Container,
                "node" => SourceKind::Node,
                other => {
                    warn!(
                        "AlertEvaluator: unknown source_kind '{}', skipping group",
                        other
                    );
                    continue;
                }
            };

            // Deduplicate metric names for this source.
            let names: Vec<String> = group
                .iter()
                .map(|r| r.metric_name.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            let latest = match self
                .store
                .query_latest(LatestQuery {
                    source_kind,
                    source_id: *source_id,
                    names,
                })
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        source_kind = source_kind_str,
                        source_id, "AlertEvaluator: query_latest failed for source group: {e}"
                    );
                    continue;
                }
            };

            // Evaluate each rule in the group against the fetched values.
            for rule in group {
                let ctx = context_cache
                    .get(&rule.id)
                    .copied()
                    .unwrap_or((0, None, None, None));
                if let Err(e) = self.evaluate_rule(rule, &latest, ctx).await {
                    warn!(
                        rule_id = rule.id,
                        rule_name = rule.name,
                        "AlertEvaluator: rule evaluation failed: {e}"
                    );
                }
            }
        }

        Ok(())
    }

    /// Evaluate a single rule against a pre-fetched latest-values map.
    async fn evaluate_rule(
        &self,
        rule: &monitoring_alert_rules::Model,
        latest: &HashMap<String, f64>,
        ctx: AlarmContext,
    ) -> Result<(), String> {
        let value = match latest.get(&rule.metric_name) {
            Some(v) => *v,
            None => {
                // No data yet — clear any breach state and return.
                self.clear_breach(rule.id).await;
                return Ok(());
            }
        };

        let is_breaching = compare(value, rule.threshold, &rule.comparator);

        if is_breaching {
            self.handle_breach(rule, value, ctx).await;
        } else {
            self.handle_recovery(rule, ctx).await;
        }

        Ok(())
    }

    /// Handle a rule that is currently in breach.
    async fn handle_breach(
        &self,
        rule: &monitoring_alert_rules::Model,
        value: f64,
        ctx: AlarmContext,
    ) {
        let rule_id = rule.id;

        // Guard: negative for_duration_secs would cast to a huge u64, making
        // the alarm never fire. Treat as 0 (immediate breach).
        if rule.for_duration_secs < 0 {
            warn!(
                rule_id,
                for_duration_secs = rule.for_duration_secs,
                "AlertEvaluator: rule has negative for_duration_secs, treating as 0"
            );
        }
        let required_secs = rule.for_duration_secs.max(0) as u64;

        let now = Instant::now();

        // Record breach start if this is the first tick in breach.
        let elapsed_secs = {
            let mut guard = self.breach_start.write().await;
            let start = guard.entry(rule_id).or_insert(now);
            start.elapsed().as_secs()
        };

        // Has the breach persisted long enough to fire?
        if elapsed_secs < required_secs {
            debug!(
                rule_id,
                elapsed_secs,
                required_secs,
                metric = rule.metric_name,
                value,
                "AlertEvaluator: breach pending (for_duration not reached)"
            );
            return;
        }

        // Check if an alarm is already firing for this rule to avoid spam.
        {
            let guard = self.firing_alarms.read().await;
            if guard.contains_key(&rule_id) {
                // Alarm already open — nothing to do.
                return;
            }
        }

        // Determine AlarmType from source kind.
        let alarm_type = alarm_type_for_rule(rule);

        // Build a human-readable message.
        let message = format!(
            "{} is {:.3} (threshold: {} {:.3}) — rule '{}'",
            rule.metric_name, value, rule.comparator, rule.threshold, rule.name
        );

        // Case-insensitive severity match to avoid silently defaulting to Warning
        // when the stored value is e.g. "Critical" or "CRITICAL".
        let severity = match rule.severity.to_lowercase().as_str() {
            "critical" => AlarmSeverity::Critical,
            "info" => AlarmSeverity::Info,
            _ => AlarmSeverity::Warning,
        };

        let (project_id, environment_id, deployment_id, service_id) = ctx;

        let request = FireAlarmRequest {
            project_id,
            environment_id,
            deployment_id,
            container_id: None,
            service_id,
            alarm_type,
            severity,
            title: format!("Metric threshold breached: {}", rule.name),
            message,
            metadata: Some(serde_json::json!({
                "rule_id": rule_id,
                "rule_name": rule.name,
                "metric_name": rule.metric_name,
                "value": value,
                "threshold": rule.threshold,
                "comparator": rule.comparator,
                "for_duration_secs": rule.for_duration_secs,
            })),
        };

        match self.alarm_service.fire_alarm(request).await {
            Ok(Some(alarm_id)) => {
                info!(
                    rule_id,
                    alarm_id,
                    metric = rule.metric_name,
                    value,
                    "AlertEvaluator: alarm fired"
                );
                self.firing_alarms.write().await.insert(rule_id, alarm_id);
            }
            Ok(None) => {
                debug!(rule_id, "AlertEvaluator: alarm suppressed by cooldown");
            }
            Err(e) => {
                error!(rule_id, "AlertEvaluator: fire_alarm failed: {e}");
            }
        }
    }

    /// Handle a rule that is no longer in breach.
    async fn handle_recovery(&self, rule: &monitoring_alert_rules::Model, ctx: AlarmContext) {
        let rule_id = rule.id;
        self.clear_breach(rule_id).await;

        // If we have a firing alarm for this rule, resolve it.
        let alarm_id = {
            let mut guard = self.firing_alarms.write().await;
            guard.remove(&rule_id)
        };

        if let Some(alarm_id) = alarm_id {
            let (project_id, _, _, _) = ctx;

            match self.alarm_service.resolve_alarm(alarm_id, project_id).await {
                Ok(()) => {
                    info!(
                        rule_id,
                        alarm_id,
                        metric = rule.metric_name,
                        "AlertEvaluator: alarm resolved (metric recovered)"
                    );
                }
                Err(e) => {
                    error!(
                        rule_id,
                        alarm_id, "AlertEvaluator: resolve_alarm failed: {e}"
                    );
                    // Re-insert so we attempt resolution again next cycle.
                    self.firing_alarms.write().await.insert(rule_id, alarm_id);
                }
            }
        }
    }

    /// Clear breach tracking for a rule (metric recovered or no data).
    async fn clear_breach(&self, rule_id: i32) {
        self.breach_start.write().await.remove(&rule_id);
    }

    /// Resolve project/environment/deployment IDs for a FireAlarmRequest.
    ///
    /// For service-scoped rules, we look up the first project that uses the
    /// service.  If no project is found we fall back to sentinel values (0, 0, 0)
    /// so the alarm is still stored.
    ///
    /// For deployment-scoped rules, we look up the deployment's own
    /// project_id and environment_id.
    ///
    /// This is called once per rule per cycle in `run_cycle` and cached in
    /// `context_cache`, so it is not called per breach/recovery.
    ///
    /// # FIXME(metrics-scale): Issue D (Scalability Review)
    ///
    /// This function is called once per rule per 30-second cycle inside a
    /// sequential loop.  At 1,000+ rules this generates 1,000+ individual
    /// DB queries per cycle (33 queries/second sustained), which adds
    /// measurable pressure under TimescaleDB write load.
    ///
    /// Fix: denormalize `project_id`, `environment_id`, and `deployment_id`
    /// directly onto the `monitoring_alert_rules` row at creation time.
    /// Then eliminate this function entirely and read those columns directly
    /// from the loaded rule set.  Alternatively, batch the lookups as:
    ///
    ///   - 1 `SELECT ... WHERE id = ANY($1)` for all deployment IDs
    ///   - 1 `SELECT ... WHERE service_id = ANY($1)` for all service IDs
    ///
    /// before entering the per-rule evaluation loop.
    async fn resolve_alarm_context(&self, rule: &monitoring_alert_rules::Model) -> AlarmContext {
        if let Some(dep_id) = rule.deployment_id {
            if let Ok(Some(dep)) = temps_entities::deployments::Entity::find_by_id(dep_id)
                .one(self.db.as_ref())
                .await
            {
                // Deployment-scoped rules have no associated service.
                return (dep.project_id, Some(dep.environment_id), Some(dep_id), None);
            }
        }

        if let Some(svc_id) = rule.service_id {
            use temps_entities::project_services;
            if let Ok(Some(ps)) = project_services::Entity::find()
                .filter(project_services::Column::ServiceId.eq(svc_id))
                .one(self.db.as_ref())
                .await
            {
                // Service-scoped (database) rules have no environment or
                // deployment context — store NULL for those so the FK
                // constraints hold — but DO carry the service_id so the alarm
                // (and its notification) records which service breached.
                return (ps.project_id, None, None, Some(svc_id));
            }
        }

        // Fallback — no context found. project_id=0 keeps existing behaviour
        // for unsuppressed errors elsewhere; env/deployment/service stay None.
        (0, None, None, None)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Evaluate `lhs <comparator> rhs`.
fn compare(lhs: f64, rhs: f64, comparator: &str) -> bool {
    match comparator {
        ">" => lhs > rhs,
        "<" => lhs < rhs,
        ">=" => lhs >= rhs,
        "<=" => lhs <= rhs,
        _ => {
            warn!(
                "AlertEvaluator: unknown comparator '{}', treating as false",
                comparator
            );
            false
        }
    }
}

/// Map a rule to the most appropriate [`AlarmType`].
fn alarm_type_for_rule(rule: &monitoring_alert_rules::Model) -> AlarmType {
    match (rule.service_id, rule.deployment_id) {
        (Some(_), None) => AlarmType::DatabaseMetricThreshold,
        (None, Some(_)) => AlarmType::DeploymentMetricThreshold,
        _ => AlarmType::DatabaseMetricThreshold,
    }
}

// ── Default rule seeding ──────────────────────────────────────────────────────

/// Error type for seeding operations — re-exported from `temps_metrics`.
pub use temps_metrics::MetricsError;

/// Insert a default set of alert rules for `service_id` if none exist yet.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(service_id, metric_name)` so concurrent calls are safe — no TOCTOU race.
///
/// # Engines supported
///
/// - `"postgres"` — 6 default rules covering connections, cache hit ratio,
///   replication lag and deadlocks.
/// - `"redis"` — 4 default rules covering memory fragmentation, eviction,
///   client count and keyspace hit ratio.
/// - `"mongodb"` — 6 default rules covering connections, queued operations,
///   WiredTiger cache pressure, replication buffer pressure, and asserts.
/// - `"rustfs"` — 3 default rules covering offline nodes and free-capacity
///   pressure, tied to the Prometheus metrics emitted by RustFS / MinIO.
/// - `"s3"` — no defaults: the AWS S3 collector only emits `s3.bucket_count`,
///   which is informational, not actionable. Self-hosted deployments use the
///   `"rustfs"` service_type and get real alerts via the Prometheus scrape.
/// - Anything else — no rules inserted.
pub async fn seed_default_rules(
    db: &DatabaseConnection,
    service_id: i32,
    engine: &str,
) -> Result<(), MetricsError> {
    let seeds: Vec<RuleSeed> = match engine.to_lowercase().as_str() {
        "postgres" => postgres_default_seeds(),
        "redis" => redis_default_seeds(),
        "mongodb" => mongodb_default_seeds(),
        "rustfs" => rustfs_default_seeds(),
        _ => {
            debug!(
                service_id,
                engine, "seed_default_rules: no defaults for engine, skipping"
            );
            return Ok(());
        }
    };

    // Insert each rule individually with ON CONFLICT DO NOTHING so concurrent
    // invocations (e.g. rapid retries or parallel API calls) are safely handled
    // by the unique index on (service_id, metric_name).
    use sea_orm::ConnectionTrait;
    for seed in &seeds {
        let sql = format!(
            "INSERT INTO monitoring_alert_rules \
             (service_id, deployment_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled) \
             VALUES ({service_id}, NULL, '{name}', '{metric_name}', {threshold}, '{comparator}', '{severity}', {for_duration}, true) \
             ON CONFLICT (service_id, metric_name) WHERE service_id IS NOT NULL DO NOTHING",
            service_id = service_id,
            name = seed.name.replace('\'', "''"),
            metric_name = seed.metric_name.replace('\'', "''"),
            threshold = seed.threshold,
            comparator = seed.comparator.replace('\'', "''"),
            severity = seed.severity.replace('\'', "''"),
            for_duration = seed.for_duration_secs,
        );

        db.execute(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            sql,
        ))
        .await
        .map_err(MetricsError::DatabaseError)?;
    }

    info!(
        service_id,
        engine, "seed_default_rules: seeded default alert rules (ON CONFLICT DO NOTHING)"
    );

    Ok(())
}

/// Insert default container alert rules for `deployment_id` if none exist yet.
///
/// Uses `INSERT … ON CONFLICT DO NOTHING` against the unique index
/// `(deployment_id, metric_name)` so concurrent calls are safe.
pub async fn seed_default_container_rules(
    db: &DatabaseConnection,
    deployment_id: i32,
) -> Result<(), MetricsError> {
    let seeds = container_default_seeds();

    use sea_orm::ConnectionTrait;
    for seed in &seeds {
        let sql = format!(
            "INSERT INTO monitoring_alert_rules \
             (service_id, deployment_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled) \
             VALUES (NULL, {deployment_id}, '{name}', '{metric_name}', {threshold}, '{comparator}', '{severity}', {for_duration}, true) \
             ON CONFLICT (deployment_id, metric_name) WHERE deployment_id IS NOT NULL DO NOTHING",
            deployment_id = deployment_id,
            name = seed.name.replace('\'', "''"),
            metric_name = seed.metric_name.replace('\'', "''"),
            threshold = seed.threshold,
            comparator = seed.comparator.replace('\'', "''"),
            severity = seed.severity.replace('\'', "''"),
            for_duration = seed.for_duration_secs,
        );

        db.execute(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            sql,
        ))
        .await
        .map_err(MetricsError::DatabaseError)?;
    }

    info!(
        deployment_id,
        "seed_default_container_rules: seeded default container alert rules (ON CONFLICT DO NOTHING)"
    );

    Ok(())
}

// ── Default rule builders ─────────────────────────────────────────────────────

/// Plain-data representation of a rule definition used in seed functions.
/// Avoids Sea-ORM `ActiveValue` wrappers in seed SQL generation.
struct RuleSeed {
    name: &'static str,
    metric_name: &'static str,
    threshold: f64,
    comparator: &'static str,
    severity: &'static str,
    for_duration_secs: i32,
}

#[cfg(test)]
fn make_service_rule(
    service_id: i32,
    name: &str,
    metric_name: &str,
    threshold: f64,
    comparator: &str,
    severity: &str,
    for_duration_secs: i32,
) -> monitoring_alert_rules::ActiveModel {
    monitoring_alert_rules::ActiveModel {
        service_id: Set(Some(service_id)),
        deployment_id: Set(None),
        name: Set(name.to_string()),
        metric_name: Set(metric_name.to_string()),
        threshold: Set(threshold),
        comparator: Set(comparator.to_string()),
        severity: Set(severity.to_string()),
        for_duration_secs: Set(for_duration_secs),
        enabled: Set(true),
        silenced_until: Set(None),
        ..Default::default()
    }
}

#[cfg(test)]
fn make_deployment_rule(
    deployment_id: i32,
    name: &str,
    metric_name: &str,
    threshold: f64,
    comparator: &str,
    severity: &str,
    for_duration_secs: i32,
) -> monitoring_alert_rules::ActiveModel {
    monitoring_alert_rules::ActiveModel {
        service_id: Set(None),
        deployment_id: Set(Some(deployment_id)),
        name: Set(name.to_string()),
        metric_name: Set(metric_name.to_string()),
        threshold: Set(threshold),
        comparator: Set(comparator.to_string()),
        severity: Set(severity.to_string()),
        for_duration_secs: Set(for_duration_secs),
        enabled: Set(true),
        silenced_until: Set(None),
        ..Default::default()
    }
}

fn postgres_default_seeds() -> Vec<RuleSeed> {
    // NOTE(metrics-correctness-A): The Postgres collector emits
    // `pg.replication_replay_lag_seconds` and `pg.replication_write_lag_seconds`
    // (per-replica Gauge metrics).  Earlier versions of this seed used the
    // non-existent name `pg.replication_lag_seconds` which caused the rules to
    // silently never fire.  We use `pg.replication_replay_lag_seconds` here
    // because replay lag is the operationally critical measure (it reflects
    // how far behind standby databases are from the primary).
    //
    // If `label`-based filtering (e.g. per `replica_addr`) is required in
    // future, the alert rule schema must be extended to support JSONB label
    // matchers.  For now, `query_latest` returns the latest value across all
    // replicas regardless of label, which fires if ANY replica breaches the
    // threshold.
    vec![
        RuleSeed {
            name: "High active connections (warning)",
            metric_name: "pg.connections_active",
            threshold: 80.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "High active connections (critical)",
            metric_name: "pg.connections_active",
            threshold: 95.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
        RuleSeed {
            name: "Low cache hit ratio",
            metric_name: "pg.cache_hit_ratio",
            threshold: 0.90,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Replication replay lag (warning)",
            metric_name: "pg.replication_replay_lag_seconds",
            threshold: 30.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Replication replay lag (critical)",
            metric_name: "pg.replication_replay_lag_seconds",
            threshold: 120.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 30,
        },
        RuleSeed {
            name: "Deadlocks detected",
            metric_name: "pg.deadlocks_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
    ]
}

fn redis_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "High memory fragmentation ratio",
            metric_name: "redis.memory_fragmentation_ratio",
            threshold: 1.5,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Keys being evicted",
            metric_name: "redis.evicted_keys_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "High connected clients",
            metric_name: "redis.connected_clients",
            threshold: 1000.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Low keyspace hit ratio",
            metric_name: "redis.keyspace_hit_ratio",
            threshold: 0.90,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
    ]
}

fn mongodb_default_seeds() -> Vec<RuleSeed> {
    // Metric names mirror the MongoDB collector's serverStatus extractor
    // (see `temps_metrics::collector::mongodb`). Connection limits are based on
    // mongod's default `maxIncomingConnections = 65536`, but most deployments
    // run with the container default of ~819 (80% of 1024 ulimit) — the
    // ratio-style metric we don't have, so absolute thresholds are used.
    vec![
        RuleSeed {
            name: "High current connections (warning)",
            metric_name: "mongo.connections_current",
            threshold: 500.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Operations queued for reads",
            metric_name: "mongo.queued_reads",
            threshold: 5.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Operations queued for writes",
            metric_name: "mongo.queued_writes",
            threshold: 5.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "WiredTiger cache near full",
            metric_name: "mongo.wiredtiger_cache_ratio",
            threshold: 0.95,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Replication buffer pressure",
            metric_name: "mongo.replication_buffer_ratio",
            threshold: 0.80,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "Cursor timeouts detected",
            metric_name: "mongo.cursor_timed_out_total",
            threshold: 0.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 0,
        },
    ]
}

fn rustfs_default_seeds() -> Vec<RuleSeed> {
    // Metric names mirror what the S3/Prometheus collector maps from RustFS /
    // MinIO (see `RUSTFS_METRICS` in `temps_metrics::collector::prometheus`).
    //
    // Two raw thresholds we can trust:
    //   - `s3.nodes_offline` > 0 — any offline node degrades availability
    //   - `s3.capacity_usable_free_bytes` — absolute byte thresholds are the
    //     only safe option; the collector does not emit a free/used ratio.
    //     The thresholds below assume a small-to-medium deployment. Users on
    //     larger clusters should tune these per-rule via the UI.
    vec![
        RuleSeed {
            name: "Storage nodes offline",
            metric_name: "s3.nodes_offline",
            threshold: 0.0,
            comparator: ">",
            severity: "critical",
            for_duration_secs: 60,
        },
        RuleSeed {
            name: "Low free capacity (warning)",
            // 10 GiB free
            metric_name: "s3.capacity_usable_free_bytes",
            threshold: 10.0 * 1024.0 * 1024.0 * 1024.0,
            comparator: "<",
            severity: "warning",
            for_duration_secs: 300,
        },
        RuleSeed {
            name: "Low free capacity (critical)",
            // 2 GiB free
            metric_name: "s3.capacity_usable_free_bytes",
            threshold: 2.0 * 1024.0 * 1024.0 * 1024.0,
            comparator: "<",
            severity: "critical",
            for_duration_secs: 60,
        },
    ]
}

fn container_default_seeds() -> Vec<RuleSeed> {
    vec![
        RuleSeed {
            name: "High CPU usage",
            metric_name: "container.cpu_percent",
            threshold: 90.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 120,
        },
        RuleSeed {
            name: "High memory usage",
            metric_name: "container.memory_percent",
            threshold: 85.0,
            comparator: ">",
            severity: "warning",
            for_duration_secs: 60,
        },
    ]
}

#[cfg(test)]
fn postgres_defaults(service_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    postgres_default_seeds()
        .into_iter()
        .map(|s| {
            make_service_rule(
                service_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

#[cfg(test)]
fn redis_defaults(service_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    redis_default_seeds()
        .into_iter()
        .map(|s| {
            make_service_rule(
                service_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

#[cfg(test)]
fn container_defaults(deployment_id: i32) -> Vec<monitoring_alert_rules::ActiveModel> {
    container_default_seeds()
        .into_iter()
        .map(|s| {
            make_deployment_rule(
                deployment_id,
                s.name,
                s.metric_name,
                s.threshold,
                s.comparator,
                s.severity,
                s.for_duration_secs,
            )
        })
        .collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── compare() ──────────────────────────────────────────────────────────────

    #[test]
    fn compare_greater_than_true() {
        assert!(compare(92.0, 80.0, ">"));
    }

    #[test]
    fn compare_greater_than_false() {
        assert!(!compare(75.0, 80.0, ">"));
    }

    #[test]
    fn compare_less_than_true() {
        assert!(compare(0.85, 0.90, "<"));
    }

    #[test]
    fn compare_less_than_false() {
        assert!(!compare(0.95, 0.90, "<"));
    }

    #[test]
    fn compare_gte_equal() {
        assert!(compare(80.0, 80.0, ">="));
    }

    #[test]
    fn compare_lte_equal() {
        assert!(compare(85.0, 85.0, "<="));
    }

    #[test]
    fn compare_unknown_comparator_returns_false() {
        assert!(!compare(100.0, 1.0, "!="));
    }

    // ── alarm_type_for_rule() ───────────────────────────────────────────────────

    fn make_rule(
        service_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> monitoring_alert_rules::Model {
        monitoring_alert_rules::Model {
            id: 1,
            service_id,
            deployment_id,
            name: "test".into(),
            metric_name: "pg.connections_active".into(),
            threshold: 80.0,
            comparator: ">".into(),
            severity: "warning".into(),
            for_duration_secs: 60,
            enabled: true,
            silenced_until: None,
        }
    }

    #[test]
    fn alarm_type_service_rule_is_database() {
        let rule = make_rule(Some(1), None);
        assert_eq!(
            alarm_type_for_rule(&rule).as_str(),
            "database_metric_threshold"
        );
    }

    #[test]
    fn alarm_type_deployment_rule_is_deployment() {
        let rule = make_rule(None, Some(5));
        assert_eq!(
            alarm_type_for_rule(&rule).as_str(),
            "deployment_metric_threshold"
        );
    }

    // ── default rule builders ──────────────────────────────────────────────────

    #[test]
    fn postgres_defaults_generates_six_rules() {
        let rules = postgres_defaults(42);
        assert_eq!(rules.len(), 6);
        for r in &rules {
            assert_eq!(r.service_id.clone().unwrap(), Some(42));
            assert_eq!(r.deployment_id.clone().unwrap(), None);
        }
        // Correctness fix: replication lag rules must reference the actual
        // collected metric name (pg.replication_replay_lag_seconds), not the
        // non-existent pg.replication_lag_seconds.
        let repl_names: Vec<String> = rules
            .iter()
            .filter(|r| r.metric_name.clone().unwrap().contains("replication"))
            .map(|r| r.metric_name.clone().unwrap())
            .collect();
        assert!(
            !repl_names.is_empty(),
            "should have at least one replication rule"
        );
        for name in &repl_names {
            assert_eq!(
                name, "pg.replication_replay_lag_seconds",
                "replication lag rule should reference pg.replication_replay_lag_seconds, got: {name}"
            );
        }
    }

    #[test]
    fn redis_defaults_generates_four_rules() {
        let rules = redis_defaults(7);
        assert_eq!(rules.len(), 4);
    }

    #[test]
    fn container_defaults_generates_two_rules() {
        let rules = container_defaults(99);
        assert_eq!(rules.len(), 2);
        for r in &rules {
            assert_eq!(r.deployment_id.clone().unwrap(), Some(99));
            assert_eq!(r.service_id.clone().unwrap(), None);
        }
    }

    #[test]
    fn postgres_defaults_severities_correct() {
        let rules = postgres_defaults(1);
        let conn_rules: Vec<_> = rules
            .iter()
            .filter(|r| r.metric_name.clone().unwrap() == "pg.connections_active")
            .collect();
        assert_eq!(conn_rules.len(), 2);
        let severities: std::collections::HashSet<String> = conn_rules
            .iter()
            .map(|r| r.severity.clone().unwrap())
            .collect();
        assert!(severities.contains("warning"));
        assert!(severities.contains("critical"));
    }

    #[test]
    fn container_defaults_metric_names() {
        let rules = container_defaults(1);
        let names: Vec<String> = rules
            .iter()
            .map(|r| r.metric_name.clone().unwrap())
            .collect();
        assert!(names.contains(&"container.cpu_percent".to_string()));
        assert!(names.contains(&"container.memory_percent".to_string()));
    }

    // ── resolve_alarm_context() ─────────────────────────────────────────────────

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::notifications::{
        EmailMessage, NotificationData, NotificationError, NotificationService,
    };
    use temps_metrics::{
        LabelledMetric, LatestByLabelQuery, MetricPoint, MetricsError, RangeQuery,
    };

    /// No-op metrics store — `resolve_alarm_context` never touches it.
    struct NoopStore;

    #[async_trait]
    impl MetricsStore for NoopStore {
        async fn write_batch(&self, _points: Vec<MetricPoint>) -> Result<(), MetricsError> {
            Ok(())
        }
        async fn query_range(
            &self,
            _filter: RangeQuery,
        ) -> Result<Vec<(DateTime<Utc>, f64)>, MetricsError> {
            Ok(Vec::new())
        }
        async fn query_latest(
            &self,
            _filter: LatestQuery,
        ) -> Result<HashMap<String, f64>, MetricsError> {
            Ok(HashMap::new())
        }
        async fn query_latest_by_label(
            &self,
            _filter: LatestByLabelQuery,
        ) -> Result<Vec<LabelledMetric>, MetricsError> {
            Ok(Vec::new())
        }
        async fn latest_timestamp(
            &self,
            _source_kind: SourceKind,
            _source_id: i32,
        ) -> Result<Option<DateTime<Utc>>, MetricsError> {
            Ok(None)
        }
        async fn prune(&self, _older_than: DateTime<Utc>) -> Result<u64, MetricsError> {
            Ok(0)
        }
    }

    struct NoopNotifications;

    #[async_trait]
    impl NotificationService for NoopNotifications {
        async fn send_notification(&self, _n: NotificationData) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_email(&self, _m: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    struct NoopQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::jobs::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed in tests")
        }
    }

    fn evaluator_with_db(db: DatabaseConnection) -> AlertEvaluator {
        let db = Arc::new(db);
        let alarm_service = Arc::new(AlarmService::new(
            db.clone(),
            Arc::new(NoopNotifications),
            Arc::new(NoopQueue),
        ));
        AlertEvaluator::new(db, Arc::new(NoopStore), alarm_service)
    }

    /// A service-scoped rule must resolve its `service_id` into the alarm
    /// context. This is the exact bug that lost service identity: previously the
    /// project lookup discarded the `service_id`, so the alarm (and its email)
    /// could only show "Project N" with no indication of which service breached.
    #[tokio::test]
    async fn resolve_alarm_context_carries_service_id_for_service_rule() {
        let rule = make_rule(Some(7), None);

        // Service-scoped rules look up `project_services` by service_id.
        let ps = temps_entities::project_services::Model {
            id: 1,
            project_id: 4,
            service_id: 7,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![ps]])
            .into_connection();

        let evaluator = evaluator_with_db(db);
        let (project_id, environment_id, deployment_id, service_id) =
            evaluator.resolve_alarm_context(&rule).await;

        assert_eq!(project_id, 4);
        assert_eq!(environment_id, None);
        assert_eq!(deployment_id, None);
        assert_eq!(
            service_id,
            Some(7),
            "service-scoped rule must carry the service_id into the alarm context"
        );
    }

    /// A service-scoped rule whose service has no owning project falls back to
    /// the sentinel context but still preserves the service_id so the alarm can
    /// at least name the service.
    #[tokio::test]
    async fn resolve_alarm_context_service_rule_without_project_keeps_service_id() {
        let rule = make_rule(Some(9), None);

        // project_services lookup returns nothing.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<temps_entities::project_services::Model>::new()])
            .into_connection();

        let evaluator = evaluator_with_db(db);
        let (project_id, environment_id, deployment_id, service_id) =
            evaluator.resolve_alarm_context(&rule).await;

        // Falls through to the sentinel — no project mapping found.
        assert_eq!(project_id, 0);
        assert_eq!(environment_id, None);
        assert_eq!(deployment_id, None);
        // service_id is None here because the fallback path can't attribute it
        // to a project; the alarm is still stored under the sentinel project.
        assert_eq!(service_id, None);
    }
}
