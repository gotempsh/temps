// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operator-defined resource alerts (CPU / memory / disk) on a node.
//!
//! The alert evaluator already evaluates any `monitoring_alert_rules` row whose
//! `node_id` is set, against the `node.*` series stored for that node. The
//! control plane (node `0`) is seeded with a few defaults; workers get their
//! series from heartbeats (see `temps_metrics::node_capacity`) but, until now,
//! had no way to define a rule. This service is that way.
//!
//! Only the three usage percentages are creatable. They are the ones every
//! node reports and the ones an operator reasons about ("page me at 90% disk"),
//! and restricting the set keeps a typo from creating a rule that can never
//! fire because no series by that name exists.

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};
use temps_entities::{monitoring_alert_rules, nodes};

/// Synthetic node ID of the control plane. It has no `nodes` row.
pub(crate) const CONTROL_PLANE_NODE_ID: i32 = 0;

/// Usage metrics an operator can alert on, for any node. Percentages, so a
/// threshold is meaningful on a 4 GB worker and a 512 GB one alike, and is
/// held to 0..=100.
const USAGE_PERCENT_NODE_METRICS: [&str; 3] = [
    "node.cpu_percent",
    "node.memory_percent",
    "node.disk_percent",
];

/// Disk-health metrics every Linux node reports, and that worker nodes get
/// default rules for. Not usage percentages: degraded arrays and media
/// errors are counts, and drive wear can pass 100% of rated endurance, so
/// their thresholds only have to be finite and non-negative.
const DISK_HEALTH_NODE_METRICS: [&str; 3] = [
    "node.raid_degraded",
    "node.disk_wear_percent",
    "node.disk_media_errors",
];

/// Every metric a worker node's rule may use.
pub(crate) const ALERTABLE_NODE_METRICS: [&str; 6] = [
    USAGE_PERCENT_NODE_METRICS[0],
    USAGE_PERCENT_NODE_METRICS[1],
    USAGE_PERCENT_NODE_METRICS[2],
    DISK_HEALTH_NODE_METRICS[0],
    DISK_HEALTH_NODE_METRICS[1],
    DISK_HEALTH_NODE_METRICS[2],
];

/// Longest rule name accepted; names are shown in notifications.
const MAX_RULE_NAME_CHARS: usize = 120;

/// Longest `for_duration_secs`: a day. Anything longer is a rule that will
/// effectively never fire and is far more likely a unit mistake.
const MAX_FOR_DURATION_SECS: i32 = 86_400;

/// A request to create a node alert rule, already parsed from the wire.
#[derive(Debug, Clone)]
pub(crate) struct NewNodeAlertRule {
    pub name: String,
    pub metric_name: String,
    pub threshold: f64,
    pub comparator: String,
    pub severity: String,
    pub for_duration_secs: i32,
    pub enabled: bool,
}

/// A partial update to a node alert rule: only the fields that are `Some` change.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeAlertRuleChanges {
    pub name: Option<String>,
    pub metric_name: Option<String>,
    pub threshold: Option<f64>,
    pub comparator: Option<String>,
    pub severity: Option<String>,
    pub for_duration_secs: Option<i32>,
    pub enabled: Option<bool>,
}

impl NodeAlertRuleChanges {
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.metric_name.is_none()
            && self.threshold.is_none()
            && self.comparator.is_none()
            && self.severity.is_none()
            && self.for_duration_secs.is_none()
            && self.enabled.is_none()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum NodeAlertRuleError {
    #[error("Node {node_id} not found while managing an alert rule")]
    NodeNotFound { node_id: i32 },
    #[error("Alert rule {rule_id} not found on node {node_id}")]
    RuleNotFound { node_id: i32, rule_id: i32 },
    #[error(
        "Node {node_id} already has an alert rule for '{metric_name}' (rule {existing_rule_id}); \
         edit that rule instead of creating another"
    )]
    AlreadyExists {
        node_id: i32,
        metric_name: String,
        existing_rule_id: i32,
    },
    #[error(
        "Alert rule {rule_id} belongs to the control plane, whose default rules are re-created \
         on every start; disable it instead of deleting it"
    )]
    ControlPlaneRuleProtected { rule_id: i32 },
    #[error("Invalid alert rule for node {node_id}: {message}")]
    Validation { node_id: i32, message: String },
    #[error("Database error while managing an alert rule on node {node_id}: {source}")]
    Database {
        node_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

pub(crate) struct NodeAlertRuleService {
    db: Arc<DatabaseConnection>,
}

impl NodeAlertRuleService {
    pub(crate) fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Create a rule on `node_id` (`0` = control plane).
    ///
    /// At most one rule exists per `(node, metric)`; that is enforced by a
    /// unique index, so a concurrent duplicate loses the race and gets the same
    /// `AlreadyExists` answer as a sequential one.
    pub(crate) async fn create(
        &self,
        node_id: i32,
        rule: NewNodeAlertRule,
    ) -> Result<monitoring_alert_rules::Model, NodeAlertRuleError> {
        validate(node_id, &rule)?;
        let db_err = |source| NodeAlertRuleError::Database { node_id, source };

        if node_id != CONTROL_PLANE_NODE_ID {
            let exists = nodes::Entity::find_by_id(node_id)
                .count(self.db.as_ref())
                .await
                .map_err(db_err)?;
            if exists == 0 {
                return Err(NodeAlertRuleError::NodeNotFound { node_id });
            }
        }

        if let Some(existing) = monitoring_alert_rules::Entity::find()
            .filter(monitoring_alert_rules::Column::NodeId.eq(node_id))
            .filter(monitoring_alert_rules::Column::MetricName.eq(rule.metric_name.as_str()))
            .one(self.db.as_ref())
            .await
            .map_err(db_err)?
        {
            return Err(NodeAlertRuleError::AlreadyExists {
                node_id,
                metric_name: rule.metric_name,
                existing_rule_id: existing.id,
            });
        }

        let metric_name = rule.metric_name.clone();
        monitoring_alert_rules::ActiveModel {
            service_id: Set(None),
            deployment_id: Set(None),
            node_id: Set(Some(node_id)),
            name: Set(rule.name.trim().to_string()),
            metric_name: Set(rule.metric_name),
            threshold: Set(rule.threshold),
            comparator: Set(rule.comparator),
            severity: Set(rule.severity),
            for_duration_secs: Set(rule.for_duration_secs),
            enabled: Set(rule.enabled),
            silenced_until: Set(None),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(|source| {
            // Lost a race with a concurrent create: the unique index on
            // (node_id, metric_name) is the arbiter.
            if matches!(
                source.sql_err(),
                Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
            ) {
                NodeAlertRuleError::AlreadyExists {
                    node_id,
                    metric_name,
                    existing_rule_id: 0,
                }
            } else {
                NodeAlertRuleError::Database { node_id, source }
            }
        })
    }

    /// Apply a partial update to a rule on `node_id`, validating the rule as
    /// it will be stored (the merge of the existing rule and `changes`).
    ///
    /// A worker's rule keeps to what `create` allows: one of the usage
    /// percentages with a threshold between 0 and 100, or one of the
    /// disk-health metrics its default rules use with a non-negative
    /// threshold, so an edit cannot leave a rule that can never fire or that
    /// has no series to evaluate. The
    /// control plane's seeded rules cover metrics that are not percentages
    /// (file descriptors, ingest rates), so for node `0` only the shape of the
    /// rule is checked, not the metric or the threshold range.
    ///
    /// Renaming the metric onto one the node already has a rule for is refused
    /// with `AlreadyExists`, the same answer `create` gives.
    pub(crate) async fn update(
        &self,
        node_id: i32,
        rule_id: i32,
        changes: NodeAlertRuleChanges,
    ) -> Result<monitoring_alert_rules::Model, NodeAlertRuleError> {
        let db_err = |source| NodeAlertRuleError::Database { node_id, source };

        let existing = monitoring_alert_rules::Entity::find_by_id(rule_id)
            .filter(monitoring_alert_rules::Column::NodeId.eq(node_id))
            .one(self.db.as_ref())
            .await
            .map_err(db_err)?
            .ok_or(NodeAlertRuleError::RuleNotFound { node_id, rule_id })?;

        if changes.is_empty() {
            return Ok(existing);
        }

        let merged = NewNodeAlertRule {
            name: changes
                .name
                .clone()
                .unwrap_or_else(|| existing.name.clone()),
            metric_name: changes
                .metric_name
                .clone()
                .unwrap_or_else(|| existing.metric_name.clone()),
            threshold: changes.threshold.unwrap_or(existing.threshold),
            comparator: changes
                .comparator
                .clone()
                .unwrap_or_else(|| existing.comparator.clone()),
            severity: changes
                .severity
                .clone()
                .unwrap_or_else(|| existing.severity.clone()),
            for_duration_secs: changes
                .for_duration_secs
                .unwrap_or(existing.for_duration_secs),
            enabled: changes.enabled.unwrap_or(existing.enabled),
        };
        if node_id == CONTROL_PLANE_NODE_ID {
            validate_shape(node_id, &merged)?;
            if !merged.threshold.is_finite() {
                return Err(NodeAlertRuleError::Validation {
                    node_id,
                    message: format!("threshold {} is not a finite number", merged.threshold),
                });
            }
        } else {
            validate(node_id, &merged)?;
        }

        if merged.metric_name != existing.metric_name {
            if let Some(other) = monitoring_alert_rules::Entity::find()
                .filter(monitoring_alert_rules::Column::NodeId.eq(node_id))
                .filter(monitoring_alert_rules::Column::MetricName.eq(merged.metric_name.as_str()))
                .filter(monitoring_alert_rules::Column::Id.ne(rule_id))
                .one(self.db.as_ref())
                .await
                .map_err(db_err)?
            {
                return Err(NodeAlertRuleError::AlreadyExists {
                    node_id,
                    metric_name: merged.metric_name,
                    existing_rule_id: other.id,
                });
            }
        }

        let metric_name = merged.metric_name.clone();
        let mut active: monitoring_alert_rules::ActiveModel = existing.into();
        if changes.name.is_some() {
            active.name = Set(merged.name.trim().to_string());
        }
        if changes.metric_name.is_some() {
            active.metric_name = Set(merged.metric_name);
        }
        if changes.threshold.is_some() {
            active.threshold = Set(merged.threshold);
        }
        if changes.comparator.is_some() {
            active.comparator = Set(merged.comparator);
        }
        if changes.severity.is_some() {
            active.severity = Set(merged.severity);
        }
        if changes.for_duration_secs.is_some() {
            active.for_duration_secs = Set(merged.for_duration_secs);
        }
        if changes.enabled.is_some() {
            active.enabled = Set(merged.enabled);
        }

        active.update(self.db.as_ref()).await.map_err(|source| {
            // Lost a race with a concurrent create or rename onto the same
            // metric: the unique index on (node_id, metric_name) arbitrates.
            if matches!(
                source.sql_err(),
                Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
            ) {
                NodeAlertRuleError::AlreadyExists {
                    node_id,
                    metric_name,
                    existing_rule_id: 0,
                }
            } else {
                NodeAlertRuleError::Database { node_id, source }
            }
        })
    }

    /// Delete a worker's rule. Returns the deleted row for auditing.
    ///
    /// The control plane's rules are refused: its defaults are re-seeded on
    /// every start, so a deleted one silently comes back while `enabled: false`
    /// survives. Refusing says so up front instead of appearing to work.
    pub(crate) async fn delete(
        &self,
        node_id: i32,
        rule_id: i32,
    ) -> Result<monitoring_alert_rules::Model, NodeAlertRuleError> {
        if node_id == CONTROL_PLANE_NODE_ID {
            return Err(NodeAlertRuleError::ControlPlaneRuleProtected { rule_id });
        }
        let db_err = |source| NodeAlertRuleError::Database { node_id, source };

        let rule = monitoring_alert_rules::Entity::find_by_id(rule_id)
            .filter(monitoring_alert_rules::Column::NodeId.eq(node_id))
            .one(self.db.as_ref())
            .await
            .map_err(db_err)?
            .ok_or(NodeAlertRuleError::RuleNotFound { node_id, rule_id })?;

        monitoring_alert_rules::Entity::delete_by_id(rule_id)
            .exec(self.db.as_ref())
            .await
            .map_err(db_err)?;

        Ok(rule)
    }
}

/// Full validation for a worker rule: its shape, a supported metric and a
/// threshold in that metric's range.
fn validate(node_id: i32, rule: &NewNodeAlertRule) -> Result<(), NodeAlertRuleError> {
    let invalid = |message: String| NodeAlertRuleError::Validation { node_id, message };

    validate_shape(node_id, rule)?;
    let metric = rule.metric_name.as_str();
    if USAGE_PERCENT_NODE_METRICS.contains(&metric) {
        if !(rule.threshold.is_finite() && (0.0..=100.0).contains(&rule.threshold)) {
            return Err(invalid(format!(
                "threshold {} for {metric} is not a percentage between 0 and 100",
                rule.threshold
            )));
        }
    } else if DISK_HEALTH_NODE_METRICS.contains(&metric) {
        if !(rule.threshold.is_finite() && rule.threshold >= 0.0) {
            return Err(invalid(format!(
                "threshold {} for {metric} must be a finite number of 0 or more",
                rule.threshold
            )));
        }
    } else {
        return Err(invalid(format!(
            "metric_name '{}' cannot be alerted on; choose one of: {}",
            rule.metric_name,
            ALERTABLE_NODE_METRICS.join(", ")
        )));
    }
    Ok(())
}

/// The checks that hold for every node's rules, whatever the metric: name,
/// comparator, severity and duration.
fn validate_shape(node_id: i32, rule: &NewNodeAlertRule) -> Result<(), NodeAlertRuleError> {
    let invalid = |message: String| NodeAlertRuleError::Validation { node_id, message };

    let name = rule.name.trim();
    if name.is_empty() {
        return Err(invalid("name cannot be empty".to_string()));
    }
    if name.chars().count() > MAX_RULE_NAME_CHARS {
        return Err(invalid(format!(
            "name is {} characters; the limit is {MAX_RULE_NAME_CHARS}",
            name.chars().count()
        )));
    }
    if !matches!(rule.comparator.as_str(), ">" | "<" | ">=" | "<=") {
        return Err(invalid(format!(
            "comparator '{}' must be one of: >, <, >=, <=",
            rule.comparator
        )));
    }
    if !matches!(rule.severity.as_str(), "warning" | "critical") {
        return Err(invalid(format!(
            "severity '{}' must be one of: warning, critical",
            rule.severity
        )));
    }
    if !(0..=MAX_FOR_DURATION_SECS).contains(&rule.for_duration_secs) {
        return Err(invalid(format!(
            "for_duration_secs {} must be between 0 and {MAX_FOR_DURATION_SECS}",
            rule.for_duration_secs
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn rule(metric: &str) -> NewNodeAlertRule {
        NewNodeAlertRule {
            name: "High disk".to_string(),
            metric_name: metric.to_string(),
            threshold: 90.0,
            comparator: ">".to_string(),
            severity: "critical".to_string(),
            for_duration_secs: 300,
            enabled: true,
        }
    }

    fn stored(id: i32, node_id: i32, metric: &str) -> monitoring_alert_rules::Model {
        monitoring_alert_rules::Model {
            id,
            service_id: None,
            deployment_id: None,
            node_id: Some(node_id),
            name: "High disk".to_string(),
            metric_name: metric.to_string(),
            threshold: 90.0,
            comparator: ">".to_string(),
            severity: "critical".to_string(),
            for_duration_secs: 300,
            enabled: true,
            silenced_until: None,
            breach_started_at: None,
        }
    }

    fn count_row(n: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([("num_items".to_string(), Value::BigInt(Some(n)))])
    }

    fn service(db: MockDatabase) -> NodeAlertRuleService {
        NodeAlertRuleService::new(Arc::new(db.into_connection()))
    }

    fn assert_invalid(
        result: Result<monitoring_alert_rules::Model, NodeAlertRuleError>,
        needle: &str,
    ) {
        match result {
            Err(NodeAlertRuleError::Validation { message, .. }) => {
                assert!(message.contains(needle), "'{message}' lacks '{needle}'")
            }
            other => panic!("expected a validation error mentioning '{needle}', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn creates_a_worker_rule_after_confirming_the_node_exists() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![count_row(1)]])
                .append_query_results([Vec::<monitoring_alert_rules::Model>::new()])
                .append_query_results([vec![stored(5, 7, "node.disk_percent")]]),
        );

        let created = svc.create(7, rule("node.disk_percent")).await.unwrap();

        assert_eq!(created.node_id, Some(7));
        assert_eq!(created.metric_name, "node.disk_percent");
    }

    #[tokio::test]
    async fn control_plane_rules_skip_the_node_lookup() {
        // No node-count result is queued: a lookup for node 0 would fail.
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<monitoring_alert_rules::Model>::new()])
                .append_query_results([vec![stored(1, 0, "node.cpu_percent")]]),
        );

        let created = svc.create(0, rule("node.cpu_percent")).await.unwrap();

        assert_eq!(created.node_id, Some(0));
    }

    #[tokio::test]
    async fn unknown_node_is_reported_with_its_id() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![count_row(0)]]),
        );

        let err = svc.create(99, rule("node.cpu_percent")).await.unwrap_err();

        assert!(matches!(
            err,
            NodeAlertRuleError::NodeNotFound { node_id: 99 }
        ));
        assert!(err.to_string().contains("99"));
    }

    #[tokio::test]
    async fn second_rule_for_the_same_metric_names_the_existing_one() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![count_row(1)]])
                .append_query_results([vec![stored(42, 7, "node.cpu_percent")]]),
        );

        let err = svc.create(7, rule("node.cpu_percent")).await.unwrap_err();

        assert!(matches!(
            err,
            NodeAlertRuleError::AlreadyExists {
                node_id: 7,
                existing_rule_id: 42,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_rules_before_touching_the_database() {
        // An empty mock fails on any query, proving validation runs first.
        let svc = service(MockDatabase::new(DatabaseBackend::Postgres));

        assert_invalid(
            svc.create(7, rule("node.fd_percent")).await,
            "cannot be alerted on",
        );
        assert_invalid(
            svc.create(7, rule("node.cpu_percent; DROP")).await,
            "cannot be alerted on",
        );

        let mut r = rule("node.cpu_percent");
        r.threshold = 150.0;
        assert_invalid(svc.create(7, r).await, "percentage");

        let mut r = rule("node.cpu_percent");
        r.threshold = f64::NAN;
        assert_invalid(svc.create(7, r).await, "percentage");

        let mut r = rule("node.cpu_percent");
        r.comparator = "==".to_string();
        assert_invalid(svc.create(7, r).await, "comparator");

        let mut r = rule("node.cpu_percent");
        r.severity = "urgent".to_string();
        assert_invalid(svc.create(7, r).await, "severity");

        let mut r = rule("node.cpu_percent");
        r.name = "   ".to_string();
        assert_invalid(svc.create(7, r).await, "name cannot be empty");

        let mut r = rule("node.cpu_percent");
        r.name = "x".repeat(MAX_RULE_NAME_CHARS + 1);
        assert_invalid(svc.create(7, r).await, "limit");

        let mut r = rule("node.cpu_percent");
        r.for_duration_secs = -1;
        assert_invalid(svc.create(7, r).await, "for_duration_secs");

        let mut r = rule("node.cpu_percent");
        r.for_duration_secs = MAX_FOR_DURATION_SECS + 1;
        assert_invalid(svc.create(7, r).await, "for_duration_secs");
    }

    #[tokio::test]
    async fn deletes_a_worker_rule_and_returns_it_for_auditing() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored(5, 7, "node.disk_percent")]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]),
        );

        let deleted = svc.delete(7, 5).await.unwrap();

        assert_eq!(deleted.id, 5);
    }

    #[tokio::test]
    async fn deleting_a_rule_that_is_not_on_the_node_is_not_found() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<monitoring_alert_rules::Model>::new()]),
        );

        let err = svc.delete(7, 5).await.unwrap_err();

        assert!(matches!(
            err,
            NodeAlertRuleError::RuleNotFound {
                node_id: 7,
                rule_id: 5
            }
        ));
    }

    #[tokio::test]
    async fn control_plane_rules_cannot_be_deleted() {
        let svc = service(MockDatabase::new(DatabaseBackend::Postgres));

        let err = svc.delete(0, 3).await.unwrap_err();

        assert!(matches!(
            err,
            NodeAlertRuleError::ControlPlaneRuleProtected { rule_id: 3 }
        ));
        assert!(err.to_string().contains("disable"));
    }

    #[tokio::test]
    async fn database_failures_carry_the_node_id() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("boom".to_string())]),
        );

        let err = svc.create(7, rule("node.cpu_percent")).await.unwrap_err();

        assert!(matches!(
            err,
            NodeAlertRuleError::Database { node_id: 7, .. }
        ));
        assert!(err.to_string().contains("node 7"));
    }

    fn changes() -> NodeAlertRuleChanges {
        NodeAlertRuleChanges::default()
    }

    #[tokio::test]
    async fn updates_a_worker_rule_threshold() {
        let mut after = stored(5, 7, "node.disk_percent");
        after.threshold = 80.0;
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored(5, 7, "node.disk_percent")]])
                .append_query_results([vec![after]]),
        );

        let updated = svc
            .update(
                7,
                5,
                NodeAlertRuleChanges {
                    threshold: Some(80.0),
                    ..changes()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.threshold, 80.0);
        assert_eq!(updated.metric_name, "node.disk_percent");
    }

    /// The disk-health rules seeded on every worker can be edited: turned
    /// off, or given a count threshold above 100, which only usage
    /// percentages refuse.
    #[tokio::test]
    async fn a_seeded_worker_disk_health_rule_can_be_disabled_and_retuned() {
        let mut after = stored(9, 7, "node.disk_media_errors");
        after.enabled = false;
        after.threshold = 250.0;
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored(9, 7, "node.disk_media_errors")]])
                .append_query_results([vec![after]]),
        );

        let updated = svc
            .update(
                7,
                9,
                NodeAlertRuleChanges {
                    enabled: Some(false),
                    threshold: Some(250.0),
                    ..changes()
                },
            )
            .await
            .expect("a seeded disk-health rule must be editable");

        assert!(!updated.enabled);
        assert_eq!(updated.threshold, 250.0);
    }

    #[tokio::test]
    async fn a_negative_disk_health_threshold_is_rejected_before_any_write() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![stored(
                9,
                7,
                "node.raid_degraded",
            )]]),
        );

        let result = svc
            .update(
                7,
                9,
                NodeAlertRuleChanges {
                    threshold: Some(-1.0),
                    ..changes()
                },
            )
            .await;

        assert_invalid(result, "node.raid_degraded");
    }

    #[tokio::test]
    async fn a_worker_threshold_above_100_is_rejected_before_any_write() {
        // Only the lookup is queued: a write would fail the mock.
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![stored(
                5,
                7,
                "node.cpu_percent",
            )]]),
        );

        let result = svc
            .update(
                7,
                5,
                NodeAlertRuleChanges {
                    threshold: Some(150.0),
                    ..changes()
                },
            )
            .await;

        assert_invalid(result, "between 0 and 100");
    }

    #[tokio::test]
    async fn a_worker_rule_cannot_be_renamed_to_an_unsupported_metric() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![stored(
                5,
                7,
                "node.cpu_percent",
            )]]),
        );

        let result = svc
            .update(
                7,
                5,
                NodeAlertRuleChanges {
                    metric_name: Some("node.fd_percent".to_string()),
                    ..changes()
                },
            )
            .await;

        assert_invalid(result, "cannot be alerted on");
    }

    #[tokio::test]
    async fn a_bad_comparator_or_empty_name_is_rejected_on_update() {
        for (change, needle) in [
            (
                NodeAlertRuleChanges {
                    comparator: Some("!=".to_string()),
                    ..changes()
                },
                "comparator",
            ),
            (
                NodeAlertRuleChanges {
                    name: Some("   ".to_string()),
                    ..changes()
                },
                "name cannot be empty",
            ),
        ] {
            let svc =
                service(
                    MockDatabase::new(DatabaseBackend::Postgres)
                        .append_query_results([vec![stored(5, 7, "node.cpu_percent")]]),
                );
            assert_invalid(svc.update(7, 5, change).await, needle);
        }
    }

    #[tokio::test]
    async fn renaming_onto_a_metric_that_already_has_a_rule_names_that_rule() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored(5, 7, "node.cpu_percent")]])
                .append_query_results([vec![stored(9, 7, "node.disk_percent")]]),
        );

        let result = svc
            .update(
                7,
                5,
                NodeAlertRuleChanges {
                    metric_name: Some("node.disk_percent".to_string()),
                    ..changes()
                },
            )
            .await;

        match result {
            Err(NodeAlertRuleError::AlreadyExists {
                node_id: 7,
                existing_rule_id: 9,
                metric_name,
            }) => assert_eq!(metric_name, "node.disk_percent"),
            other => panic!("expected AlreadyExists for rule 9 on node 7, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn updating_a_rule_that_belongs_to_another_node_is_not_found() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<monitoring_alert_rules::Model>::new()]),
        );

        let result = svc
            .update(
                7,
                5,
                NodeAlertRuleChanges {
                    enabled: Some(false),
                    ..changes()
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(NodeAlertRuleError::RuleNotFound {
                node_id: 7,
                rule_id: 5
            })
        ));
    }

    #[tokio::test]
    async fn an_empty_update_returns_the_rule_without_writing() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![stored(
                5,
                7,
                "node.cpu_percent",
            )]]),
        );

        let unchanged = svc.update(7, 5, changes()).await.unwrap();

        assert_eq!(unchanged.id, 5);
    }

    #[tokio::test]
    async fn control_plane_rules_keep_non_percentage_metrics_and_large_thresholds() {
        // The seeded defaults include rates and counts, so a threshold above
        // 100 on a non-percentage metric is legitimate for node 0.
        let mut before = stored(3, 0, "otel.rate_limited_requests");
        before.threshold = 10.0;
        let mut after = before.clone();
        after.threshold = 500.0;
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![before]])
                .append_query_results([vec![after]]),
        );

        let updated = svc
            .update(
                0,
                3,
                NodeAlertRuleChanges {
                    threshold: Some(500.0),
                    ..changes()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.threshold, 500.0);
    }

    #[tokio::test]
    async fn control_plane_rules_still_reject_a_non_finite_threshold() {
        let svc = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![stored(
                3,
                0,
                "otel.rate_limited_requests",
            )]]),
        );

        let result = svc
            .update(
                0,
                3,
                NodeAlertRuleChanges {
                    threshold: Some(f64::NAN),
                    ..changes()
                },
            )
            .await;

        assert_invalid(result, "not a finite number");
    }
}
