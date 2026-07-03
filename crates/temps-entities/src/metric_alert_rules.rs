use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// A first-class metric alert rule for a project.
///
/// A rule is defined on a *signal* (project + metric + aggregation) plus a
/// polymorphic, versionable detector (`detection_config`, jsonb) — independent of
/// any dashboard. The background `MetricAlertEvaluator` evaluates every enabled
/// rule on an interval, reduces the latest window to a scalar via `aggregation`,
/// asks the detector whether it breaches, and fires/resolves a notification (via
/// the reused `temps-monitoring` alarm system) once a breach has persisted for at
/// least `for_duration_secs`. This table holds config/metadata only — it is
/// Postgres-backed, never ClickHouse.
///
/// The detector is stored as a typed-in-Rust enum
/// (`temps_otel::detectors::DetectionConfig`) serialized to jsonb on this column
/// only; the service/DTO layers are fully typed. Adding a new detector family is
/// code-only (new enum variant + evaluator branch) — never a migration —
/// because `detection_kind` is a plain string and the params live in the blob.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "metric_alert_rules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// FK to `projects(id)`. Indexed; rules are scoped by project.
    pub project_id: i32,
    /// Human-readable rule name.
    pub name: String,
    /// The metric name the rule evaluates (e.g. `http.server.duration`).
    pub metric_name: String,
    /// Aggregation applied over the window: one of
    /// `avg|sum|min|max|count|rate|p50|p90|p95|p99`. Cross-cutting: every
    /// detector family reduces the metric to a scalar through this first.
    pub aggregation: String,
    /// Coarse detector discriminator mirroring `detection_config`'s `kind` tag:
    /// one of `static|anomaly|forecast|outlier|auto_watch`. Kept as a column (not
    /// in the blob) so the evaluator/UI can filter by kind without a JSON probe.
    pub detection_kind: String,
    /// Typed detector definition (`temps_otel::detectors::DetectionConfig`),
    /// stored as jsonb. Raw `Value` on the Model only — the service/DTO layers
    /// are fully typed. For a static rule this is `{kind:static,comparator,threshold}`.
    #[sea_orm(column_type = "JsonBinary")]
    pub detection_config: serde_json::Value,
    /// AND-combined label equality filters as `[["key","value"],…]`. Empty array
    /// means no filtering (today's behaviour). Stored as jsonb; the service/DTO
    /// layers convert through `Vec<(String,String)>` so the entity stays raw.
    #[sea_orm(column_type = "JsonBinary")]
    pub label_filters: serde_json::Value,
    /// Label keys to break the metric down by as `["endpoint","region"]` (ADR-026
    /// Phase 3). Empty array = one aggregate stream (today's behaviour). Stored as
    /// jsonb; the service/DTO layers convert through `Vec<String>`.
    #[sea_orm(column_type = "JsonBinary")]
    pub group_by: serde_json::Value,
    /// When true (and `group_by` is set), the evaluator fires one independent
    /// alarm per breaching series keyed by `(rule_id, series_key)`. When false
    /// (the default) a set `group_by` collapses to a single "any series breaches"
    /// aggregate alarm. Dynamic mode is static-detector-only in this phase.
    pub dynamic_alerts: bool,
    /// Cardinality cap for dynamic alerting: at most this many series (top by
    /// `|value|`) are evaluated/tracked per tick. Hard-capped at 100 by the service.
    pub max_series: i32,
    /// Notification-grouping threshold for dynamic alerting: when more than this
    /// many series transition to firing in the same tick, only the first gets the
    /// expensive chart/AI enrichment. Range 1–1000, default 5 (validated by the
    /// service). A per-rule column so operators can tune it without a rebuild.
    pub grouped_notification_threshold: i32,
    /// Aggregation/eval window in seconds (e.g. 300).
    pub window_secs: i32,
    /// How long (seconds) a breach must persist before the rule fires.
    pub for_duration_secs: i32,
    /// Severity used when firing: one of `info|warning|critical`.
    pub severity: String,
    /// Whether the evaluator considers this rule.
    pub enabled: bool,
    /// Last observed evaluator state: one of `ok|firing|unknown`.
    pub last_state: String,
    /// Last aggregated value the evaluator computed, when available.
    pub last_value: Option<f64>,
    /// Full per-series state snapshot persisted after every dynamic-rule tick,
    /// keyed by the human-readable series label. Shape:
    /// `{"method=GET": {"state":"firing","value":12.5,"alarm_id":259}}`. Only kept
    /// (evaluated) series appear; dropped/disappeared series don't linger. Stored
    /// as jsonb; the DTO layer decodes through `HashMap<String, SeriesStateEntry>`.
    /// Stays `{}` for static/aggregate rules, which never populate it.
    #[sea_orm(column_type = "JsonBinary")]
    pub series_states: serde_json::Value,
    /// Number of series dropped by the cardinality cap on the LATEST dynamic tick
    /// (0 when nothing was dropped or for static/aggregate rules). Lets a UI warn
    /// "N series were dropped this tick" without reading server logs.
    pub last_dropped_series_count: i32,
    /// When the evaluator last evaluated this rule.
    pub last_evaluated_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        // Always bump updated_at — callers that want to preserve it can
        // explicitly Set() before save, though no caller currently does.
        self.updated_at = Set(now);
        Ok(self)
    }
}
