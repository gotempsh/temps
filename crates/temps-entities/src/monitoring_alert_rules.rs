use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A monitoring alert rule that the [`AlertEvaluator`] background task
/// evaluates every 30 seconds.
///
/// Exactly one of `service_id`, `deployment_id`, or `node_id` must be
/// non-null (enforced by a DB-level CHECK constraint).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "monitoring_alert_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// FK to `external_services.id` — set for database-level metric rules.
    pub service_id: Option<i32>,
    /// FK to `deployments.id` — set for container/OTLP metric rules.
    pub deployment_id: Option<i32>,
    /// Node ID — set for node-scoped metric rules (e.g. `proxy.*` metrics).
    /// No FK: the control plane uses the synthetic node ID `0`, which has no
    /// row in `nodes`.
    pub node_id: Option<i32>,
    /// Human-readable rule label (e.g. "High active connections").
    pub name: String,
    /// Dotted metric name to evaluate, e.g. `"pg.connections_active"`.
    pub metric_name: String,
    /// Numeric threshold.
    pub threshold: f64,
    /// Comparison operator: one of `'>'`, `'<'`, `'>='`, `'<='`.
    pub comparator: String,
    /// Severity: `'warning'` or `'critical'`.
    pub severity: String,
    /// Consecutive seconds the breach must persist before the alarm fires.
    /// `0` means fire immediately on the first evaluation.
    pub for_duration_secs: i32,
    /// Whether the rule is active.  Set to `false` to soft-disable without
    /// deleting the rule.
    pub enabled: bool,
    /// Optional silence window.  The evaluator skips the rule while
    /// `silenced_until > NOW()`.
    pub silenced_until: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::external_services::Entity",
        from = "Column::ServiceId",
        to = "super::external_services::Column::Id",
        on_delete = "Cascade"
    )]
    ExternalServices,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::DeploymentId",
        to = "super::deployments::Column::Id",
        on_delete = "Cascade"
    )]
    Deployments,
}

impl Related<super::external_services::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalServices.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deployments.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
