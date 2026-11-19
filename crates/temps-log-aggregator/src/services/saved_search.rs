// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Saved log searches (ADR 073 D1).
//!
//! A saved search names a log query so an investigation does not start from
//! an empty box, and so something else can reference it: a log alert counts
//! the lines it matches, and a dashboard tile charts them. The query is the
//! same filter set the global log search accepts, minus the time window and
//! the cursor — the window belongs to whoever runs it (the console's picker,
//! the alert's evaluation window).
//!
//! The search is always scoped to its project. A query cannot name another
//! project, so a saved search can never be the way a caller reaches logs
//! they could not read directly; it narrows within one project and nothing
//! more.

use std::sync::Arc;

use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_entities::log_saved_searches::{ActiveModel, Column, Entity, Model};

use crate::error::LogAggregatorError;
use crate::index::analytics::{parse_attr_predicates, AttrPredicate};
use crate::store::LogSourceKind;
use crate::types::LogLevel;

/// Saved searches per project. A project-level configuration list, not a
/// data table: a hundred named queries is already more than anyone reads.
pub const MAX_SAVED_SEARCHES_PER_PROJECT: u64 = 100;

const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 1_000;
const MAX_FILTER_VALUES: usize = 100;
const MAX_FILTER_VALUE_LEN: usize = 256;
const MAX_TEXT_LEN: usize = 1_024;

/// Relative windows the console offers for a saved search. Stored as the
/// label, resolved to a window by whoever runs the search.
pub const SAVED_SEARCH_TIME_RANGES: &[&str] = &["15m", "1h", "6h", "24h", "7d", "30d"];

/// The stored filter set of a saved search.
///
/// Mirrors `GlobalLogSearchRequest` without `start_time`/`end_time`,
/// `cursor`, `page_size` and the project selector: the window is the
/// caller's and the project is the row's. External services are held as ids
/// rather than names so counting a saved search needs no control-plane name
/// resolution.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SavedSearchQuery {
    /// Which family of sources to search: everything the project has
    /// (`collected`), its application containers, or its databases.
    #[serde(default)]
    pub source: LogSourceKind,
    /// Narrow to these of the project's external services (databases,
    /// imported services). Empty means every source allowed by `source`.
    #[serde(default)]
    pub external_service_ids: Vec<i32>,
    #[serde(default)]
    pub levels: Vec<LogLevel>,
    /// Environment identifiers as the store holds them.
    #[serde(default)]
    pub envs: Vec<String>,
    /// Container service labels (`web`, `worker`).
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub container_ids: Vec<String>,
    #[serde(default)]
    pub node_ids: Vec<i32>,
    pub deploy_id: Option<i32>,
    /// Case-insensitive substring match over the message.
    pub text: Option<String>,
    /// Attribute predicates in the wire form the search endpoints take:
    /// `<key><op><value>` with `op` one of `=`, `!=`, `^=`, `>`, `<`, or
    /// `<key>?` for "exists".
    #[serde(default)]
    pub attrs: Vec<String>,
}

impl SavedSearchQuery {
    /// Everything the store would reject later, rejected at write time, plus
    /// the attribute syntax: a saved search that cannot be run is not worth
    /// storing.
    pub fn validate(&self) -> Result<(), LogAggregatorError> {
        for values in [&self.envs, &self.services, &self.container_ids, &self.attrs] {
            if values.len() > MAX_FILTER_VALUES
                || values.iter().any(|v| v.len() > MAX_FILTER_VALUE_LEN)
            {
                return Err(LogAggregatorError::Validation {
                    message: format!(
                        "A saved search takes at most {MAX_FILTER_VALUES} values per filter, \
                         each at most {MAX_FILTER_VALUE_LEN} characters"
                    ),
                });
            }
        }
        if self.node_ids.len() > MAX_FILTER_VALUES
            || self.external_service_ids.len() > MAX_FILTER_VALUES
        {
            return Err(LogAggregatorError::Validation {
                message: format!(
                    "A saved search takes at most {MAX_FILTER_VALUES} node or service ids"
                ),
            });
        }
        if self.text.as_ref().is_some_and(|t| t.len() > MAX_TEXT_LEN) {
            return Err(LogAggregatorError::Validation {
                message: format!("Saved search text must be {MAX_TEXT_LEN} characters or fewer"),
            });
        }
        if self
            .node_ids
            .iter()
            .chain(&self.external_service_ids)
            .any(|id| *id <= 0)
        {
            return Err(LogAggregatorError::Validation {
                message: "Saved search node and service ids must be positive".to_string(),
            });
        }
        if self.deploy_id.is_some_and(|id| id <= 0) {
            return Err(LogAggregatorError::Validation {
                message: "Saved search deploy_id must be positive".to_string(),
            });
        }
        self.predicates()?;
        Ok(())
    }

    /// The parsed attribute predicates, or a validation error naming the
    /// malformed one.
    pub fn predicates(&self) -> Result<Vec<AttrPredicate>, LogAggregatorError> {
        parse_attr_predicates(&self.attrs)
    }

    /// Whether running this query has to read message bytes. A text needle
    /// cannot be answered by the line index, which holds no messages.
    pub fn needs_message_scan(&self) -> bool {
        self.text.as_ref().is_some_and(|t| !t.is_empty())
    }
}

/// A saved search to create.
#[derive(Debug)]
pub struct NewSavedSearch {
    pub name: String,
    pub description: Option<String>,
    pub query: SavedSearchQuery,
    /// One of [`SAVED_SEARCH_TIME_RANGES`].
    pub time_range: String,
    pub pinned: bool,
    /// The user saving it, `None` for a non-human caller.
    pub created_by: Option<i32>,
}

/// What a write changes. `None` leaves the field alone.
#[derive(Debug, Default)]
pub struct SavedSearchUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub query: Option<SavedSearchQuery>,
    pub time_range: Option<String>,
    pub pinned: Option<bool>,
}

/// CRUD over `log_saved_searches`, scoped to a project on every call.
pub struct LogSavedSearchService {
    db: Arc<DatabaseConnection>,
}

impl LogSavedSearchService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Pinned first, then newest first, paginated.
    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<Model>, u64), LogAggregatorError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let paginator = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .order_by_desc(Column::Pinned)
            .order_by_desc(Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;
        Ok((items, total))
    }

    /// Every saved search of a project, for pickers that need the whole
    /// (already bounded by [`MAX_SAVED_SEARCHES_PER_PROJECT`]) list.
    pub async fn list_all(&self, project_id: i32) -> Result<Vec<Model>, LogAggregatorError> {
        Ok(Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .order_by_desc(Column::Pinned)
            .order_by_asc(Column::Name)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn get(
        &self,
        project_id: i32,
        saved_search_id: i32,
    ) -> Result<Model, LogAggregatorError> {
        Entity::find_by_id(saved_search_id)
            .filter(Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(LogAggregatorError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            })
    }

    /// The query of a saved search, decoded.
    ///
    /// A blob that no longer decodes is a corrupt row, not an empty query:
    /// returning `Validation` keeps a log alert from quietly counting *every*
    /// line of the project because its filters vanished.
    pub async fn query_for(
        &self,
        project_id: i32,
        saved_search_id: i32,
    ) -> Result<SavedSearchQuery, LogAggregatorError> {
        let model = self.get(project_id, saved_search_id).await?;
        decode_query(&model)
    }

    pub async fn create(
        &self,
        project_id: i32,
        new: NewSavedSearch,
    ) -> Result<Model, LogAggregatorError> {
        let NewSavedSearch {
            name,
            description,
            query,
            time_range,
            pinned,
            created_by,
        } = new;
        let name = validate_name(&name)?;
        let description = validate_description(description)?;
        let time_range = validate_time_range(&time_range)?;
        query.validate()?;

        let txn = self.db.begin().await?;
        // Counted and inserted under one transaction so two concurrent
        // creations cannot both pass the limit check.
        let existing = Entity::find()
            .filter(Column::ProjectId.eq(project_id))
            .count(&txn)
            .await?;
        if existing >= MAX_SAVED_SEARCHES_PER_PROJECT {
            txn.rollback().await?;
            return Err(LogAggregatorError::SavedSearchLimitReached {
                project_id,
                existing,
                limit: MAX_SAVED_SEARCHES_PER_PROJECT,
            });
        }
        let model = ActiveModel {
            project_id: Set(project_id),
            name: Set(name.clone()),
            description: Set(description),
            query: Set(serde_json::to_value(&query)?),
            time_range: Set(time_range),
            pinned: Set(pinned),
            created_by: Set(created_by),
            ..Default::default()
        }
        .insert(&txn)
        .await;
        let model = match model {
            Ok(model) => model,
            Err(e) => {
                txn.rollback().await?;
                // The unique index on (project_id, name) is what actually
                // enforces the claim; a read-then-write check would let two
                // requests both pass.
                return Err(name_conflict(e, project_id, &name));
            }
        };
        txn.commit().await?;
        Ok(model)
    }

    pub async fn update(
        &self,
        project_id: i32,
        saved_search_id: i32,
        update: SavedSearchUpdate,
    ) -> Result<Model, LogAggregatorError> {
        let existing = self.get(project_id, saved_search_id).await?;
        let mut active: ActiveModel = existing.into();

        let mut new_name = None;
        if let Some(name) = update.name {
            let name = validate_name(&name)?;
            new_name = Some(name.clone());
            active.name = Set(name);
        }
        if let Some(description) = update.description {
            active.description = Set(validate_description(description)?);
        }
        if let Some(query) = update.query {
            query.validate()?;
            active.query = Set(serde_json::to_value(&query)?);
        }
        if let Some(time_range) = update.time_range {
            active.time_range = Set(validate_time_range(&time_range)?);
        }
        if let Some(pinned) = update.pinned {
            active.pinned = Set(pinned);
        }

        active
            .update(self.db.as_ref())
            .await
            .map_err(|e| match new_name {
                Some(name) => name_conflict(e, project_id, &name),
                None => LogAggregatorError::Database(e),
            })
    }

    /// Delete a saved search, refusing while a log alert counts it.
    ///
    /// The foreign key cascades (so a project delete takes its log rules with
    /// it); deleting a search directly must not silently delete the alerts
    /// built on it, so this refuses and says how many to detach first.
    pub async fn delete(
        &self,
        project_id: i32,
        saved_search_id: i32,
    ) -> Result<(), LogAggregatorError> {
        self.get(project_id, saved_search_id).await?;
        let alert_count = self.alert_count(saved_search_id).await?;
        if alert_count > 0 {
            return Err(LogAggregatorError::SavedSearchInUse {
                saved_search_id,
                alert_count,
            });
        }
        let deleted = Entity::delete_many()
            .filter(Column::Id.eq(saved_search_id))
            .filter(Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected == 0 {
            return Err(LogAggregatorError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            });
        }
        Ok(())
    }

    /// How many log alert rules count each of `saved_search_ids`, in one
    /// grouped query rather than one per row.
    pub async fn alert_counts(
        &self,
        saved_search_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, u64>, LogAggregatorError> {
        use sea_orm::QuerySelect;
        use temps_entities::metric_alert_rules;
        if saved_search_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<(Option<i32>, i64)> = metric_alert_rules::Entity::find()
            .select_only()
            .column(metric_alert_rules::Column::SavedSearchId)
            .column_as(metric_alert_rules::Column::Id.count(), "rules")
            .filter(metric_alert_rules::Column::SavedSearchId.is_in(saved_search_ids.to_vec()))
            .group_by(metric_alert_rules::Column::SavedSearchId)
            .into_tuple()
            .all(self.db.as_ref())
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, count)| id.map(|id| (id, u64::try_from(count).unwrap_or(0))))
            .collect())
    }

    /// Log alert rules counting this saved search.
    pub async fn alert_count(&self, saved_search_id: i32) -> Result<u64, LogAggregatorError> {
        use temps_entities::metric_alert_rules;
        Ok(metric_alert_rules::Entity::find()
            .filter(metric_alert_rules::Column::SavedSearchId.eq(saved_search_id))
            .count(self.db.as_ref())
            .await?)
    }
}

/// Decode a stored query blob.
pub fn decode_query(model: &Model) -> Result<SavedSearchQuery, LogAggregatorError> {
    serde_json::from_value(model.query.clone()).map_err(|e| LogAggregatorError::Validation {
        message: format!(
            "Saved search {} ('{}') has a query that no longer decodes: {e}",
            model.id, model.name
        ),
    })
}

fn name_conflict(error: sea_orm::DbErr, project_id: i32, name: &str) -> LogAggregatorError {
    if is_unique_violation(&error) {
        return LogAggregatorError::SavedSearchNameTaken {
            project_id,
            name: name.to_string(),
        };
    }
    LogAggregatorError::Database(error)
}

/// Whether a database error is the unique-index violation on
/// `(project_id, name)`, the only unique index on the table besides the key.
fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

fn validate_name(name: &str) -> Result<String, LogAggregatorError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LogAggregatorError::Validation {
            message: "A saved search needs a name".to_string(),
        });
    }
    if name.len() > MAX_NAME_LEN {
        return Err(LogAggregatorError::Validation {
            message: format!("Saved search name must be {MAX_NAME_LEN} characters or fewer"),
        });
    }
    Ok(name.to_string())
}

fn validate_description(description: Option<String>) -> Result<Option<String>, LogAggregatorError> {
    let Some(description) = description else {
        return Ok(None);
    };
    let description = description.trim();
    if description.is_empty() {
        return Ok(None);
    }
    if description.len() > MAX_DESCRIPTION_LEN {
        return Err(LogAggregatorError::Validation {
            message: format!(
                "Saved search description must be {MAX_DESCRIPTION_LEN} characters or fewer"
            ),
        });
    }
    Ok(Some(description.to_string()))
}

fn validate_time_range(time_range: &str) -> Result<String, LogAggregatorError> {
    let time_range = time_range.trim();
    if !SAVED_SEARCH_TIME_RANGES.contains(&time_range) {
        return Err(LogAggregatorError::Validation {
            message: format!(
                "Invalid saved search time range '{time_range}' (allowed: {})",
                SAVED_SEARCH_TIME_RANGES.join(", ")
            ),
        });
    }
    Ok(time_range.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::analytics::AttrOp;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn query() -> SavedSearchQuery {
        SavedSearchQuery {
            text: Some("payment failed".to_string()),
            levels: vec![LogLevel::Error],
            ..Default::default()
        }
    }

    fn model(id: i32, project_id: i32, query: &SavedSearchQuery) -> Model {
        Model {
            id,
            project_id,
            name: format!("search-{id}"),
            description: None,
            query: serde_json::to_value(query).expect("query serializes"),
            time_range: "1h".to_string(),
            pinned: false,
            created_by: Some(1),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn service(db: sea_orm::DatabaseConnection) -> LogSavedSearchService {
        LogSavedSearchService::new(Arc::new(db))
    }

    // ── The query shape ─────────────────────────────────────────────────

    #[test]
    fn an_empty_query_is_valid_and_needs_no_message_scan() {
        let query = SavedSearchQuery::default();
        assert!(query.validate().is_ok());
        assert!(!query.needs_message_scan());
    }

    #[test]
    fn a_text_needle_forces_a_message_scan() {
        assert!(query().needs_message_scan());
        // An empty needle is not a needle: it would otherwise push every
        // count onto the expensive path for no filtering at all.
        let blank = SavedSearchQuery {
            text: Some(String::new()),
            ..Default::default()
        };
        assert!(!blank.needs_message_scan());
    }

    #[test]
    fn attribute_predicates_are_parsed_at_write_time() {
        let query = SavedSearchQuery {
            attrs: vec!["status_code=500".to_string(), "worker?".to_string()],
            ..Default::default()
        };
        let predicates = query.predicates().expect("predicates parse");
        assert_eq!(predicates.len(), 2);
        assert_eq!(predicates[0].key, "status_code");
        assert_eq!(predicates[0].op, AttrOp::Eq);
        assert_eq!(predicates[1].op, AttrOp::Exists);

        let broken = SavedSearchQuery {
            attrs: vec!["=500".to_string()],
            ..Default::default()
        };
        let error = broken
            .validate()
            .expect_err("a keyless predicate is rejected");
        assert!(
            matches!(error, LogAggregatorError::Validation { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn oversized_and_negative_filters_are_rejected() {
        let too_many = SavedSearchQuery {
            services: (0..MAX_FILTER_VALUES + 1).map(|i| i.to_string()).collect(),
            ..Default::default()
        };
        assert!(too_many.validate().is_err());

        let too_long = SavedSearchQuery {
            envs: vec!["e".repeat(MAX_FILTER_VALUE_LEN + 1)],
            ..Default::default()
        };
        assert!(too_long.validate().is_err());

        let long_text = SavedSearchQuery {
            text: Some("x".repeat(MAX_TEXT_LEN + 1)),
            ..Default::default()
        };
        assert!(long_text.validate().is_err());

        let bad_id = SavedSearchQuery {
            node_ids: vec![0],
            ..Default::default()
        };
        assert!(bad_id.validate().is_err());

        let bad_deploy = SavedSearchQuery {
            deploy_id: Some(-1),
            ..Default::default()
        };
        assert!(bad_deploy.validate().is_err());
    }

    // ── Field validation ────────────────────────────────────────────────

    #[test]
    fn a_name_is_trimmed_and_must_not_be_blank_or_huge() {
        assert_eq!(
            validate_name("  Payment errors  ").unwrap(),
            "Payment errors"
        );
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"n".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn a_blank_description_is_stored_as_absent() {
        assert_eq!(validate_description(Some("  ".to_string())).unwrap(), None);
        assert_eq!(
            validate_description(Some(" why ".to_string())).unwrap(),
            Some("why".to_string())
        );
        assert!(validate_description(Some("d".repeat(MAX_DESCRIPTION_LEN + 1))).is_err());
    }

    #[test]
    fn only_the_offered_time_ranges_are_accepted() {
        for range in SAVED_SEARCH_TIME_RANGES {
            assert_eq!(&validate_time_range(range).unwrap(), range);
        }
        let error = validate_time_range("90s").expect_err("an unlisted range is rejected");
        // The message names what is allowed: an operator should not have to
        // guess the vocabulary from a 400.
        assert!(error.to_string().contains("15m"), "unexpected: {error}");
    }

    // ── Service behaviour on a mock database ────────────────────────────

    #[tokio::test]
    async fn get_is_scoped_to_the_project_and_reports_the_id_it_looked_for() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<Model>::new()])
            .into_connection();
        let error = service(db)
            .get(7, 42)
            .await
            .expect_err("a search of another project is not found");
        match error {
            LogAggregatorError::SavedSearchNotFound {
                project_id,
                saved_search_id,
            } => {
                assert_eq!((project_id, saved_search_id), (7, 42));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn query_for_decodes_the_stored_filters() {
        let expected = query();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![model(1, 7, &expected)]])
            .into_connection();
        let decoded = service(db).query_for(7, 1).await.expect("query decodes");
        assert_eq!(decoded, expected);
    }

    #[tokio::test]
    async fn a_query_blob_that_no_longer_decodes_is_an_error_not_an_empty_filter_set() {
        // The dangerous failure mode: an unreadable blob silently becoming
        // "match everything", which would make a log alert count every line
        // in the project.
        let mut row = model(1, 7, &query());
        row.query = serde_json::json!({ "levels": "not-a-list" });
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();
        let error = service(db)
            .query_for(7, 1)
            .await
            .expect_err("a corrupt query is refused");
        assert!(
            matches!(error, LogAggregatorError::Validation { .. }),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn create_refuses_once_the_per_project_limit_is_reached() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(
                i64::try_from(MAX_SAVED_SEARCHES_PER_PROJECT).expect("limit fits in i64"),
            )]])
            .into_connection();
        let error = service(db)
            .create(
                7,
                NewSavedSearch {
                    name: "one more".to_string(),
                    description: None,
                    query: SavedSearchQuery::default(),
                    time_range: "1h".to_string(),
                    pinned: false,
                    created_by: Some(1),
                },
            )
            .await
            .expect_err("the limit is enforced");
        match error {
            LogAggregatorError::SavedSearchLimitReached {
                project_id,
                existing,
                limit,
            } => {
                assert_eq!(project_id, 7);
                assert_eq!(existing, MAX_SAVED_SEARCHES_PER_PROJECT);
                assert_eq!(limit, MAX_SAVED_SEARCHES_PER_PROJECT);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn create_validates_before_touching_the_database() {
        // No query results queued: reaching the database at all would make
        // this test fail, which is the point.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert!(service(db)
            .create(
                7,
                NewSavedSearch {
                    name: String::new(),
                    description: None,
                    query: SavedSearchQuery::default(),
                    time_range: "1h".to_string(),
                    pinned: false,
                    created_by: None,
                },
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn delete_refuses_while_a_log_alert_counts_the_search() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // get()
            .append_query_results([vec![model(1, 7, &query())]])
            // alert_count()
            .append_query_results([vec![count_row(2)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let error = service(db)
            .delete(7, 1)
            .await
            .expect_err("a search with alerts is kept");
        match error {
            LogAggregatorError::SavedSearchInUse {
                saved_search_id,
                alert_count,
            } => assert_eq!((saved_search_id, alert_count), (1, 2)),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn a_database_failure_surfaces_as_a_database_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let error = service(db)
            .get(7, 1)
            .await
            .expect_err("the error propagates");
        assert!(
            matches!(error, LogAggregatorError::Database(_)),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_failure_that_is_not_a_unique_violation_stays_a_database_error() {
        // A conflict is a claim about the unique index, not a catch-all for
        // write failures. The conflict itself is asserted against a real
        // database below, where the driver produces the violation.
        let other = sea_orm::DbErr::Custom("disk full".to_string());
        assert!(matches!(
            name_conflict(other, 7, "Payment errors"),
            LogAggregatorError::Database(_)
        ));
    }

    // ── Against a real database ─────────────────────────────────────────

    async fn seed_project(db: &sea_orm::DatabaseConnection) -> i32 {
        use temps_entities::preset::Preset;
        temps_entities::projects::ActiveModel {
            name: Set("Saved search project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("saved-search-{}", uuid::Uuid::new_v4())),
            preset: Set(Preset::NextJs),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("project row inserts")
        .id
    }

    async fn seed_log_alert(
        db: &sea_orm::DatabaseConnection,
        project_id: i32,
        saved_search_id: i32,
    ) {
        use sea_orm::ConnectionTrait;
        db.execute(sea_orm::Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO metric_alert_rules \
             (project_id, name, metric_name, aggregation, detection_kind, detection_config, \
              window_secs, for_duration_secs, severity, source, saved_search_id) \
             VALUES ($1, 'Payment failures', 'logs.matched_lines', 'count', 'static', \
              '{\"kind\":\"static\",\"comparator\":\"gt\",\"threshold\":20}'::jsonb, \
              300, 0, 'warning', 'logs', $2)",
            [project_id.into(), saved_search_id.into()],
        ))
        .await
        .expect("log alert row inserts");
    }

    #[tokio::test]
    async fn a_search_an_alert_counts_is_kept_but_its_project_can_still_be_deleted() {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Docker/DB not available, skipping test: {e}");
                return;
            }
        };
        let conn = db.connection_arc();
        let project_id = seed_project(conn.as_ref()).await;
        let service = LogSavedSearchService::new(conn.clone());
        let saved = service
            .create(
                project_id,
                NewSavedSearch {
                    name: "Payment failures".to_string(),
                    description: None,
                    query: query(),
                    time_range: "1h".to_string(),
                    pinned: true,
                    created_by: None,
                },
            )
            .await
            .expect("saved search is created");

        // The name is unique per project.
        let duplicate = service
            .create(
                project_id,
                NewSavedSearch {
                    name: "Payment failures".to_string(),
                    description: None,
                    query: SavedSearchQuery::default(),
                    time_range: "1h".to_string(),
                    pinned: false,
                    created_by: None,
                },
            )
            .await
            .expect_err("a second search with the same name is refused");
        assert!(
            matches!(duplicate, LogAggregatorError::SavedSearchNameTaken { .. }),
            "unexpected error: {duplicate}"
        );

        seed_log_alert(conn.as_ref(), project_id, saved.id).await;
        assert_eq!(service.alert_count(saved.id).await.expect("counted"), 1);
        let in_use = service
            .delete(project_id, saved.id)
            .await
            .expect_err("a search with an alert is kept");
        assert!(
            matches!(
                in_use,
                LogAggregatorError::SavedSearchInUse { alert_count: 1, .. }
            ),
            "unexpected error: {in_use}"
        );

        // Deleting the project removes the search, and the log rule with it.
        // `metric_alert_rules` has no key to `projects`, so a blocking key
        // to the search would make this delete fail.
        temps_entities::projects::Entity::delete_by_id(project_id)
            .exec(conn.as_ref())
            .await
            .expect("the project delete cascades through the alert and the search");
        assert!(matches!(
            service.get(project_id, saved.id).await,
            Err(LogAggregatorError::SavedSearchNotFound { .. })
        ));
        assert_eq!(
            service.alert_count(saved.id).await.expect("counted"),
            0,
            "the log rule went with its search"
        );
    }

    /// A `COUNT(*) AS num_items` row as Sea-ORM's mock database returns it.
    fn count_row(count: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([("num_items".to_string(), Value::BigInt(Some(count)))])
    }
}
