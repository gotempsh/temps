//! Service for per-project saved metric dashboards.
//!
//! Dashboards are Postgres config/metadata (the `metric_dashboards` table), so
//! this service owns its own `Arc<DatabaseConnection>` rather than going through
//! the ClickHouse/TimescaleDB-backed `OtelStorage` trait used by `OtelService`.
//!
//! The persisted `layout` column is a `serde_json::Value`, but the API surface
//! and validation always go through the typed [`DashboardLayout`] struct: the
//! service converts at the boundary via `serde_json::to_value` /
//! `serde_json::from_value`, exactly as `restore_runs` does for its typed
//! payloads. No bare `serde_json::Value` ever crosses the handler boundary.

use std::sync::Arc;

use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_entities::metric_dashboards::{ActiveModel, Column, Entity, Model};

use crate::error::OtelError;

/// The allowlisted aggregations a tile may request.
///
/// Mirrors the keyword/quantile forms accepted by `MetricAggregation::parse`
/// (`avg|sum|min|max|count|rate` plus `p50|p90|p95|p99`).
const ALLOWED_AGGREGATIONS: &[&str] = &[
    "avg", "sum", "min", "max", "count", "rate", "p50", "p90", "p95", "p99",
];

/// Upper bounds to prevent unbounded payloads (storage / render DoS).
const MAX_NAME_LEN: usize = 200;
const MAX_SECTIONS: usize = 50;
const MAX_TILES_PER_SECTION: usize = 50;
const MAX_METRIC_NAME_LEN: usize = 256;

/// A single metric tile within a dashboard section.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DashboardTile {
    /// Stable client-generated tile id (used as a React key / for reordering).
    pub id: String,
    /// The metric name to chart (e.g. `http.server.duration`).
    pub metric_name: String,
    /// Aggregation applied per bucket: one of
    /// `avg|sum|min|max|count|rate|p50|p90|p95|p99`.
    pub aggregation: String,
    /// Optional display title; falls back to the metric name in the UI.
    pub title: Option<String>,
}

/// A titled group of tiles within a dashboard.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DashboardSection {
    /// Stable client-generated section id.
    pub id: String,
    /// Section heading.
    pub title: String,
    /// Tiles rendered within this section.
    pub tiles: Vec<DashboardTile>,
}

/// The typed layout persisted (as JSONB) in `metric_dashboards.layout`.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DashboardLayout {
    /// Ordered sections that make up the dashboard.
    pub sections: Vec<DashboardSection>,
}

impl DashboardLayout {
    /// Validate every tile's aggregation against the allowlist.
    ///
    /// Returns [`OtelError::Validation`] on the first invalid aggregation so the
    /// caller surfaces a 400 rather than persisting an un-renderable tile.
    pub fn validate(&self) -> Result<(), OtelError> {
        if self.sections.len() > MAX_SECTIONS {
            return Err(OtelError::Validation {
                message: format!(
                    "Too many sections ({}, max {MAX_SECTIONS})",
                    self.sections.len()
                ),
            });
        }
        for section in &self.sections {
            if section.tiles.len() > MAX_TILES_PER_SECTION {
                return Err(OtelError::Validation {
                    message: format!(
                        "Section '{}' has too many tiles ({}, max {MAX_TILES_PER_SECTION})",
                        section.title,
                        section.tiles.len()
                    ),
                });
            }
            for tile in &section.tiles {
                if tile.metric_name.len() > MAX_METRIC_NAME_LEN {
                    return Err(OtelError::Validation {
                        message: format!("Tile metric_name exceeds {MAX_METRIC_NAME_LEN} chars"),
                    });
                }
                let agg = tile.aggregation.trim().to_ascii_lowercase();
                if !ALLOWED_AGGREGATIONS.contains(&agg.as_str()) {
                    return Err(OtelError::Validation {
                        message: format!(
                            "Invalid tile aggregation '{}' (allowed: {})",
                            tile.aggregation,
                            ALLOWED_AGGREGATIONS.join(", ")
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Validate a dashboard name (non-empty after trim, within the length cap).
fn validate_name(name: &str) -> Result<(), OtelError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(OtelError::Validation {
            message: "Dashboard name cannot be empty".to_string(),
        });
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err(OtelError::Validation {
            message: format!("Dashboard name exceeds {MAX_NAME_LEN} characters"),
        });
    }
    Ok(())
}

/// Service managing CRUD over `metric_dashboards`.
pub struct MetricDashboardService {
    db: Arc<DatabaseConnection>,
}

impl MetricDashboardService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// List dashboards for a project, newest first, paginated.
    ///
    /// Returns `(items, total)` where `total` is the unpaginated count.
    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<Model>, u64), OtelError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = std::cmp::min(page_size.unwrap_or(20), 100);
        let paginator = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .order_by_desc(Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator.fetch_page(page - 1).await?;
        Ok((items, total))
    }

    /// Create a dashboard. Validates the layout before persisting.
    pub async fn create(
        &self,
        project_id: i32,
        name: String,
        layout: DashboardLayout,
    ) -> Result<Model, OtelError> {
        validate_name(&name)?;
        layout.validate()?;

        let layout_value = serde_json::to_value(&layout)?;
        let model = ActiveModel {
            project_id: Set(project_id),
            name: Set(name.trim().to_string()),
            layout: Set(layout_value),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(model)
    }

    /// Fetch a single dashboard by id, SCOPED to `project_id`.
    ///
    /// Filtering by project_id (not a bare primary key) prevents a caller
    /// operating in one project from reading another project's dashboard by
    /// guessing its id (cross-tenant IDOR).
    pub async fn get(&self, project_id: i32, id: i32) -> Result<Model, OtelError> {
        Entity::find_by_id(id)
            .filter(Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(OtelError::DashboardNotFound { dashboard_id: id })
    }

    /// Update a dashboard (scoped to `project_id`). Validates supplied fields.
    pub async fn update(
        &self,
        project_id: i32,
        id: i32,
        name: Option<String>,
        layout: Option<DashboardLayout>,
    ) -> Result<Model, OtelError> {
        // Ensure the row exists AND belongs to the project (typed 404 otherwise).
        let existing = self.get(project_id, id).await?;

        if let Some(ref n) = name {
            validate_name(n)?;
        }
        if let Some(ref l) = layout {
            l.validate()?;
        }

        let mut active: ActiveModel = existing.into();
        if let Some(n) = name {
            active.name = Set(n.trim().to_string());
        }
        if let Some(l) = layout {
            active.layout = Set(serde_json::to_value(&l)?);
        }
        let model = active.update(self.db.as_ref()).await?;
        Ok(model)
    }

    /// Delete a dashboard (scoped to `project_id`). Returns
    /// [`OtelError::DashboardNotFound`] when no matching row was removed.
    pub async fn delete(&self, project_id: i32, id: i32) -> Result<(), OtelError> {
        let result = Entity::delete_many()
            .filter(Column::Id.eq(id))
            .filter(Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(OtelError::DashboardNotFound { dashboard_id: id });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;
    use temps_core::DBDateTime;

    /// Build a MockRow representing a `COUNT(*) AS num_items` result for the
    /// sea-orm paginator. `num_items()` reads `try_get::<i64>("", "num_items")`.
    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("num_items".to_string(), Value::BigInt(Some(n)));
        m
    }

    fn sample_layout() -> DashboardLayout {
        DashboardLayout {
            sections: vec![DashboardSection {
                id: "s1".to_string(),
                title: "Overview".to_string(),
                tiles: vec![DashboardTile {
                    id: "t1".to_string(),
                    metric_name: "http.server.duration".to_string(),
                    aggregation: "p95".to_string(),
                    title: Some("Latency".to_string()),
                }],
            }],
        }
    }

    fn sample_model(id: i32) -> Model {
        let now: DBDateTime = chrono::Utc::now();
        Model {
            id,
            project_id: 7,
            name: "My Dashboard".to_string(),
            layout: serde_json::to_value(sample_layout()).unwrap(),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn test_create_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_model(1)]])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service
            .create(7, "My Dashboard".to_string(), sample_layout())
            .await;

        assert!(result.is_ok());
        let model = result.unwrap();
        assert_eq!(model.id, 1);
        assert_eq!(model.project_id, 7);
        assert_eq!(model.name, "My Dashboard");
    }

    #[tokio::test]
    async fn test_create_empty_name_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service.create(7, "   ".to_string(), sample_layout()).await;

        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_create_bad_aggregation_validation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let mut layout = sample_layout();
        layout.sections[0].tiles[0].aggregation = "median".to_string();

        let result = service.create(7, "Dash".to_string(), layout).await;
        assert!(matches!(result.unwrap_err(), OtelError::Validation { .. }));
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service.get(7, 999).await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::DashboardNotFound { dashboard_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_update_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<Model>::new()])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service
            .update(7, 999, Some("New Name".to_string()), None)
            .await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::DashboardNotFound { dashboard_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service.delete(7, 999).await;
        assert!(matches!(
            result.unwrap_err(),
            OtelError::DashboardNotFound { dashboard_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_delete_success() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        let result = service.delete(7, 1).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_pagination_caps_page_size() {
        // Sea-ORM paginator: num_items (COUNT) first, then fetch_page (SELECT).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(2)]])
            .append_query_results(vec![vec![sample_model(1), sample_model(2)]])
            .into_connection();
        let service = MetricDashboardService::new(Arc::new(db));

        // Request an over-cap page_size; should be clamped to 100 internally.
        let result = service.list(7, Some(1), Some(10_000)).await;
        assert!(result.is_ok());
        let (items, total) = result.unwrap();
        assert_eq!(total, 2);
        assert_eq!(items.len(), 2);
    }
}
