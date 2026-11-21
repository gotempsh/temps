// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversion goals (ADR 109 D2): definitions and the reports built on them.
//!
//! Goals are matched at query time, never on the ingest path, so they apply
//! to history as soon as they are saved and cost nothing per event.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, Set, Statement,
    TransactionTrait,
};
use temps_entities::analytics_goals;

use super::query::{self, percentage, validate_json_key};
use super::types::*;
use crate::services::SmartFilter;

pub struct AnalyticsGoalService {
    db: Arc<DatabaseConnection>,
}

#[derive(Debug, FromQueryResult)]
struct StatsRow {
    goal_id: i32,
    total_conversions: i64,
    unique_conversions: i64,
    revenue_total: Option<f64>,
    revenue_count: i64,
}

#[derive(Debug, FromQueryResult)]
struct VisitorsRow {
    visitors: i64,
}

#[derive(Debug, FromQueryResult)]
struct BreakdownRow {
    value: Option<String>,
    visitors: i64,
    unique_conversions: i64,
    total_conversions: i64,
    revenue_total: Option<f64>,
    revenue_count: i64,
}

fn revenue(goal: &AnalyticsGoal, total: Option<f64>, count: i64) -> Option<AnalyticsGoalRevenue> {
    let currency = goal.revenue_currency.clone()?;
    goal.revenue_property.as_ref()?;
    let total = total.unwrap_or(0.0);
    Some(AnalyticsGoalRevenue {
        currency,
        total,
        average: if count > 0 { total / count as f64 } else { 0.0 },
    })
}

/// A validated, normalised goal ready to store.
#[derive(Debug, PartialEq)]
struct ValidGoal {
    name: String,
    kind: AnalyticsGoalKind,
    event_name: Option<String>,
    event_filter: serde_json::Value,
    page_path: Option<String>,
    revenue_property: Option<String>,
    revenue_currency: Option<String>,
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn validate(project_id: i32, input: AnalyticsGoalInput) -> Result<ValidGoal, AnalyticsGoalError> {
    let invalid = |message: String| AnalyticsGoalError::Validation {
        project_id,
        message,
    };

    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(invalid("name must not be empty".into()));
    }
    if name.chars().count() > MAX_GOAL_NAME_LEN {
        return Err(invalid(format!(
            "name must be at most {MAX_GOAL_NAME_LEN} characters"
        )));
    }

    let event_name = trimmed(input.event_name);
    let page_path = trimmed(input.page_path);
    match input.kind {
        AnalyticsGoalKind::Event => {
            let Some(event_name) = &event_name else {
                return Err(invalid("an event goal needs event_name".into()));
            };
            if event_name.chars().count() > MAX_GOAL_EVENT_NAME_LEN {
                return Err(invalid(format!(
                    "event_name must be at most {MAX_GOAL_EVENT_NAME_LEN} characters"
                )));
            }
            if page_path.is_some() {
                return Err(invalid(
                    "an event goal matches by event_name; leave page_path empty".into(),
                ));
            }
        }
        AnalyticsGoalKind::Page => {
            let Some(page_path) = &page_path else {
                return Err(invalid("a page goal needs page_path".into()));
            };
            if !page_path.starts_with('/') {
                return Err(invalid(format!(
                    "page_path must start with '/', got '{page_path}'"
                )));
            }
            if page_path.chars().count() > MAX_GOAL_PAGE_PATH_LEN {
                return Err(invalid(format!(
                    "page_path must be at most {MAX_GOAL_PAGE_PATH_LEN} characters"
                )));
            }
            if event_name.is_some() {
                return Err(invalid(
                    "a page goal matches page views; leave event_name empty".into(),
                ));
            }
        }
    }

    if input.event_filter.len() > MAX_GOAL_FILTERS {
        return Err(invalid(format!(
            "a goal may have at most {MAX_GOAL_FILTERS} filters, got {}",
            input.event_filter.len()
        )));
    }
    for filter in &input.event_filter {
        if let SmartFilter::CustomData { path, .. } = filter {
            if !path.split('.').all(validate_json_key) {
                return Err(invalid(format!(
                    "custom property path '{path}' may only use letters, digits, '_' and '.' between keys"
                )));
            }
        }
    }
    let event_filter = serde_json::to_value(&input.event_filter)
        .map_err(|e| invalid(format!("filters could not be stored: {e}")))?;

    let revenue_property = trimmed(input.revenue_property);
    let revenue_currency = trimmed(input.revenue_currency).map(|c| c.to_ascii_uppercase());
    match (&revenue_property, &revenue_currency) {
        (None, None) => {}
        (Some(property), Some(currency)) => {
            if input.kind == AnalyticsGoalKind::Page {
                return Err(invalid(
                    "only event goals carry revenue; page views have no amount".into(),
                ));
            }
            if property.len() > MAX_GOAL_PROPERTY_LEN
                || !property
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(invalid(format!(
                    "revenue_property '{property}' must be at most {MAX_GOAL_PROPERTY_LEN} letters, digits, '_', '-' or '.'"
                )));
            }
            if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(invalid(format!(
                    "revenue_currency must be a three-letter ISO 4217 code, got '{currency}'"
                )));
            }
        }
        _ => {
            return Err(invalid(
                "revenue_property and revenue_currency must be set together".into(),
            ))
        }
    }

    Ok(ValidGoal {
        name,
        kind: input.kind,
        event_name,
        event_filter,
        page_path,
        revenue_property,
        revenue_currency,
    })
}

fn validate_period(
    project_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), AnalyticsGoalError> {
    if start >= end {
        return Err(AnalyticsGoalError::Validation {
            project_id,
            message: format!("start_date {start} must be before end_date {end}"),
        });
    }
    Ok(())
}

impl TryFrom<analytics_goals::Model> for AnalyticsGoal {
    type Error = AnalyticsGoalError;

    fn try_from(model: analytics_goals::Model) -> Result<Self, AnalyticsGoalError> {
        let kind =
            AnalyticsGoalKind::parse(&model.kind).ok_or_else(|| AnalyticsGoalError::Corrupt {
                goal_id: model.id,
                reason: format!("unknown kind '{}'", model.kind),
            })?;
        let event_filter: Vec<SmartFilter> =
            serde_json::from_value(model.event_filter).map_err(|e| {
                AnalyticsGoalError::Corrupt {
                    goal_id: model.id,
                    reason: format!("unreadable event_filter: {e}"),
                }
            })?;
        Ok(AnalyticsGoal {
            id: model.id,
            project_id: model.project_id,
            name: model.name,
            kind,
            event_name: model.event_name,
            event_filter,
            page_path: model.page_path,
            revenue_property: model.revenue_property,
            revenue_currency: model.revenue_currency,
            created_at: model.created_at,
            updated_at: model.updated_at,
        })
    }
}

impl AnalyticsGoalService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    fn db_error(
        project_id: i32,
        operation: &'static str,
    ) -> impl FnOnce(sea_orm::DbErr) -> AnalyticsGoalError {
        move |source| AnalyticsGoalError::Database {
            project_id,
            operation,
            source,
        }
    }

    /// Map a write failure, turning the case-insensitive unique name index
    /// into [`AnalyticsGoalError::DuplicateName`].
    fn write_error(
        project_id: i32,
        name: &str,
        operation: &'static str,
        error: sea_orm::DbErr,
    ) -> AnalyticsGoalError {
        match error.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                AnalyticsGoalError::DuplicateName {
                    project_id,
                    name: name.to_string(),
                }
            }
            _ => AnalyticsGoalError::Database {
                project_id,
                operation,
                source: error,
            },
        }
    }

    /// Goals of a project, newest first.
    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<AnalyticsGoalListResponse, AnalyticsGoalError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let paginator = analytics_goals::Entity::find()
            .filter(analytics_goals::Column::ProjectId.eq(project_id))
            .order_by_desc(analytics_goals::Column::CreatedAt)
            .order_by_desc(analytics_goals::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator
            .num_items()
            .await
            .map_err(Self::db_error(project_id, "count goals"))?;
        let items = paginator
            .fetch_page(page - 1)
            .await
            .map_err(Self::db_error(project_id, "list goals"))?
            .into_iter()
            .map(AnalyticsGoal::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AnalyticsGoalListResponse {
            items,
            total,
            page,
            page_size,
        })
    }

    async fn find_model(
        &self,
        project_id: i32,
        goal_id: i32,
    ) -> Result<analytics_goals::Model, AnalyticsGoalError> {
        analytics_goals::Entity::find_by_id(goal_id)
            .filter(analytics_goals::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(Self::db_error(project_id, "load goal"))?
            .ok_or(AnalyticsGoalError::NotFound {
                project_id,
                goal_id,
            })
    }

    pub async fn get(
        &self,
        project_id: i32,
        goal_id: i32,
    ) -> Result<AnalyticsGoal, AnalyticsGoalError> {
        AnalyticsGoal::try_from(self.find_model(project_id, goal_id).await?)
    }

    pub async fn create(
        &self,
        project_id: i32,
        input: AnalyticsGoalInput,
    ) -> Result<AnalyticsGoal, AnalyticsGoalError> {
        let goal = validate(project_id, input)?;
        // Concurrent creates for one project are serialised on the project
        // row, so two of them cannot both pass the limit check.
        let txn = self
            .db
            .begin()
            .await
            .map_err(Self::db_error(project_id, "begin goal creation"))?;
        let locked = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id FROM projects WHERE id = $1 FOR UPDATE",
                [project_id.into()],
            ))
            .await
            .map_err(Self::db_error(project_id, "lock project for goal creation"))?;
        if locked.is_none() {
            return Err(AnalyticsGoalError::Validation {
                project_id,
                message: format!("project {project_id} does not exist"),
            });
        }
        let existing = analytics_goals::Entity::find()
            .filter(analytics_goals::Column::ProjectId.eq(project_id))
            .count(&txn)
            .await
            .map_err(Self::db_error(project_id, "count goals"))?;
        if existing >= MAX_GOALS_PER_PROJECT {
            return Err(AnalyticsGoalError::LimitReached {
                project_id,
                limit: MAX_GOALS_PER_PROJECT,
            });
        }
        let now = Utc::now();
        let name = goal.name.clone();
        let model = analytics_goals::ActiveModel {
            project_id: Set(project_id),
            name: Set(goal.name),
            kind: Set(goal.kind.as_str().to_string()),
            event_name: Set(goal.event_name),
            event_filter: Set(goal.event_filter),
            page_path: Set(goal.page_path),
            revenue_property: Set(goal.revenue_property),
            revenue_currency: Set(goal.revenue_currency),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(|e| Self::write_error(project_id, &name, "create goal", e))?;
        txn.commit()
            .await
            .map_err(Self::db_error(project_id, "commit goal creation"))?;
        AnalyticsGoal::try_from(model)
    }

    pub async fn update(
        &self,
        project_id: i32,
        goal_id: i32,
        input: AnalyticsGoalInput,
    ) -> Result<AnalyticsGoal, AnalyticsGoalError> {
        let goal = validate(project_id, input)?;
        let mut model: analytics_goals::ActiveModel =
            self.find_model(project_id, goal_id).await?.into();
        let name = goal.name.clone();
        model.name = Set(goal.name);
        model.kind = Set(goal.kind.as_str().to_string());
        model.event_name = Set(goal.event_name);
        model.event_filter = Set(goal.event_filter);
        model.page_path = Set(goal.page_path);
        model.revenue_property = Set(goal.revenue_property);
        model.revenue_currency = Set(goal.revenue_currency);
        model.updated_at = Set(Utc::now());
        let model = model
            .update(self.db.as_ref())
            .await
            .map_err(|e| Self::write_error(project_id, &name, "update goal", e))?;
        AnalyticsGoal::try_from(model)
    }

    /// Delete a goal and return what it was, for the audit log. Events are
    /// untouched: a goal is only a way of reading them.
    pub async fn delete(
        &self,
        project_id: i32,
        goal_id: i32,
    ) -> Result<AnalyticsGoal, AnalyticsGoalError> {
        let model = self.find_model(project_id, goal_id).await?;
        analytics_goals::Entity::delete_by_id(goal_id)
            .filter(analytics_goals::Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await
            .map_err(Self::db_error(project_id, "delete goal"))?;
        AnalyticsGoal::try_from(model)
    }

    /// Every goal of the project, oldest first; bounded by
    /// [`MAX_GOALS_PER_PROJECT`].
    async fn all(&self, project_id: i32) -> Result<Vec<AnalyticsGoal>, AnalyticsGoalError> {
        analytics_goals::Entity::find()
            .filter(analytics_goals::Column::ProjectId.eq(project_id))
            .order_by_asc(analytics_goals::Column::CreatedAt)
            .order_by_asc(analytics_goals::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(Self::db_error(project_id, "list goals"))?
            .into_iter()
            .map(AnalyticsGoal::try_from)
            .collect()
    }

    /// Conversions of every goal in a period, with the period's visitors as
    /// the rate denominator. Two queries whatever the number of goals.
    pub async fn stats(
        &self,
        project_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        environment_id: Option<i32>,
    ) -> Result<AnalyticsGoalStatsResponse, AnalyticsGoalError> {
        validate_period(project_id, start, end)?;
        let goals = self.all(project_id).await?;

        let (sql, values) = query::visitors_query(project_id, start, end, environment_id);
        let visitors = VisitorsRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .one(self.db.as_ref())
        .await
        .map_err(Self::db_error(project_id, "count visitors for goals"))?
        .map(|row| row.visitors)
        .unwrap_or(0);

        let mut by_goal: HashMap<i32, StatsRow> = HashMap::new();
        if let Some((sql, values)) =
            query::stats_query(&goals, project_id, start, end, environment_id)
        {
            let rows = StatsRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .all(self.db.as_ref())
            .await
            .map_err(Self::db_error(project_id, "count goal conversions"))?;
            by_goal = rows.into_iter().map(|row| (row.goal_id, row)).collect();
        }

        let goals = goals
            .iter()
            .map(|goal| {
                let row = by_goal.remove(&goal.id);
                let unique = row.as_ref().map_or(0, |r| r.unique_conversions);
                AnalyticsGoalStats {
                    goal_id: goal.id,
                    name: goal.name.clone(),
                    kind: goal.kind,
                    unique_conversions: unique,
                    total_conversions: row.as_ref().map_or(0, |r| r.total_conversions),
                    conversion_rate: percentage(unique, visitors),
                    revenue: revenue(
                        goal,
                        row.as_ref().and_then(|r| r.revenue_total),
                        row.as_ref().map_or(0, |r| r.revenue_count),
                    ),
                }
            })
            .collect();
        Ok(AnalyticsGoalStatsResponse { visitors, goals })
    }

    /// One goal's conversions grouped by a dimension: "which sources bring
    /// the visitors who convert".
    pub async fn breakdown(
        &self,
        project_id: i32,
        goal_id: i32,
        request: AnalyticsGoalBreakdownQuery,
    ) -> Result<AnalyticsGoalBreakdownResponse, AnalyticsGoalError> {
        validate_period(project_id, request.start_date, request.end_date)?;
        let limit = request.limit.unwrap_or(10);
        if limit == 0 || limit > MAX_GOAL_BREAKDOWN_LIMIT {
            return Err(AnalyticsGoalError::Validation {
                project_id,
                message: format!(
                    "limit must be between 1 and {MAX_GOAL_BREAKDOWN_LIMIT}, got {limit}"
                ),
            });
        }
        let goal = self.get(project_id, goal_id).await?;
        let (sql, values) = query::breakdown_query(
            &goal,
            request.dimension,
            project_id,
            request.start_date,
            request.end_date,
            request.environment_id,
            limit,
        )
        .ok_or_else(|| AnalyticsGoalError::Corrupt {
            goal_id,
            reason: "its stored definition cannot be matched".into(),
        })?;
        let rows = BreakdownRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(Self::db_error(project_id, "break down goal conversions"))?;
        Ok(AnalyticsGoalBreakdownResponse {
            goal_id,
            dimension: request.dimension,
            items: rows
                .into_iter()
                .map(|row| AnalyticsGoalBreakdownRow {
                    conversion_rate: percentage(row.unique_conversions, row.visitors),
                    revenue: revenue(&goal, row.revenue_total, row.revenue_count),
                    value: row.value,
                    visitors: row.visitors,
                    unique_conversions: row.unique_conversions,
                    total_conversions: row.total_conversions,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::{environments, events, projects, upstream_config::UpstreamList, visitor};
    use temps_test_utils::TestDatabase;

    fn event_input(name: &str, event_name: &str) -> AnalyticsGoalInput {
        AnalyticsGoalInput {
            name: name.into(),
            kind: AnalyticsGoalKind::Event,
            event_name: Some(event_name.into()),
            event_filter: vec![],
            page_path: None,
            revenue_property: None,
            revenue_currency: None,
        }
    }

    fn page_input(name: &str, path: &str) -> AnalyticsGoalInput {
        AnalyticsGoalInput {
            name: name.into(),
            kind: AnalyticsGoalKind::Page,
            event_name: None,
            event_filter: vec![],
            page_path: Some(path.into()),
            revenue_property: None,
            revenue_currency: None,
        }
    }

    fn validation_message(result: Result<ValidGoal, AnalyticsGoalError>) -> String {
        match result {
            Err(AnalyticsGoalError::Validation { message, .. }) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_goal_is_trimmed_and_its_currency_upper_cased() {
        let mut input = event_input("  Purchase ", " purchase ");
        input.revenue_property = Some("amount".into());
        input.revenue_currency = Some("eur".into());
        let goal = validate(1, input).expect("valid");
        assert_eq!(goal.name, "Purchase");
        assert_eq!(goal.event_name.as_deref(), Some("purchase"));
        assert_eq!(goal.revenue_currency.as_deref(), Some("EUR"));
        assert_eq!(goal.event_filter, serde_json::json!([]));
    }

    #[test]
    fn each_kind_needs_its_own_target_and_refuses_the_other() {
        let mut input = event_input("Signup", "signup");
        input.event_name = None;
        assert!(validation_message(validate(1, input)).contains("needs event_name"));

        let mut input = event_input("Signup", "signup");
        input.page_path = Some("/thanks".into());
        assert!(validation_message(validate(1, input)).contains("leave page_path empty"));

        let mut input = page_input("Thanks", "/thanks");
        input.event_name = Some("signup".into());
        assert!(validation_message(validate(1, input)).contains("leave event_name empty"));

        assert!(
            validation_message(validate(1, page_input("Thanks", "thanks")))
                .contains("must start with '/'")
        );
    }

    #[test]
    fn names_and_filters_are_bounded() {
        assert!(
            validation_message(validate(1, event_input("   ", "signup")))
                .contains("must not be empty")
        );
        assert!(
            validation_message(validate(1, event_input(&"x".repeat(101), "signup")))
                .contains("at most 100")
        );
        let mut input = event_input("Signup", "signup");
        input.event_filter = (0..11)
            .map(|i| SmartFilter::UtmSource(format!("s{i}")))
            .collect();
        assert!(validation_message(validate(1, input)).contains("at most 10 filters"));

        let mut input = event_input("Signup", "signup");
        input.event_filter = vec![SmartFilter::CustomData {
            path: "plan-tier".into(),
            value: "pro".into(),
        }];
        assert!(validation_message(validate(1, input)).contains("custom property path"));
    }

    #[test]
    fn revenue_needs_a_currency_a_safe_key_and_an_event_goal() {
        let mut input = event_input("Purchase", "purchase");
        input.revenue_property = Some("amount".into());
        assert!(validation_message(validate(1, input)).contains("set together"));

        let mut input = event_input("Purchase", "purchase");
        input.revenue_property = Some("amount".into());
        input.revenue_currency = Some("EURO".into());
        assert!(validation_message(validate(1, input)).contains("ISO 4217"));

        let mut input = event_input("Purchase", "purchase");
        input.revenue_property = Some("amount'--".into());
        input.revenue_currency = Some("EUR".into());
        assert!(validation_message(validate(1, input)).contains("revenue_property"));

        let mut input = page_input("Thanks", "/thanks");
        input.revenue_property = Some("amount".into());
        input.revenue_currency = Some("EUR".into());
        assert!(validation_message(validate(1, input)).contains("only event goals"));
    }

    async fn test_db() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(e) => {
                println!("Test database unavailable ({e}), skipping");
                None
            }
        }
    }

    async fn create_project(db: &DatabaseConnection, slug: &str) -> (i32, i32) {
        let project = projects::ActiveModel {
            name: Set(slug.to_string()),
            slug: Set(slug.to_string()),
            repo_owner: Set("owner".to_string()),
            repo_name: Set(slug.to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            main_branch: Set("main".to_string()),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("project");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set(format!("{slug}-production")),
            subdomain: Set(format!("{slug}.temps.localhost")),
            host: Set("localhost".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("environment");
        (project.id, environment.id)
    }

    async fn create_visitor(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        key: &str,
        channel: &str,
    ) -> i32 {
        visitor::ActiveModel {
            visitor_id: Set(key.to_string()),
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            first_seen: Set(Utc::now()),
            last_seen: Set(Utc::now()),
            is_crawler: Set(false),
            has_activity: Set(true),
            first_channel: Set(Some(channel.to_string())),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("visitor")
        .id
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test fixture: one argument per event column the scenarios vary"
    )]
    async fn create_event(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        visitor_id: i32,
        event_type: &str,
        event_name: Option<&str>,
        pathname: &str,
        props: Option<serde_json::Value>,
        is_crawler: bool,
    ) {
        events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            session_id: Set(Some(format!("s-{visitor_id}"))),
            visitor_id: Set(Some(visitor_id)),
            event_type: Set(event_type.to_string()),
            event_name: Set(event_name.map(str::to_string)),
            pathname: Set(pathname.to_string()),
            page_path: Set(pathname.to_string()),
            hostname: Set("site.example".to_string()),
            href: Set(format!("https://site.example{pathname}")),
            timestamp: Set(Utc::now() - chrono::Duration::hours(1)),
            props: Set(props),
            is_crawler: Set(is_crawler),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("event");
    }

    /// An event flagged at ingest: `crawler_name` is `unknown` for a UA-less
    /// server-side event, or the reason a traffic filter excluded it.
    async fn create_flagged_purchase(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        visitor_id: i32,
        amount: &str,
        crawler_name: &str,
    ) {
        events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            session_id: Set(Some(format!("s-{visitor_id}"))),
            visitor_id: Set(Some(visitor_id)),
            event_type: Set("custom".to_string()),
            event_name: Set(Some("purchase".to_string())),
            pathname: Set("/checkout".to_string()),
            page_path: Set("/checkout".to_string()),
            hostname: Set("site.example".to_string()),
            href: Set("https://site.example/checkout".to_string()),
            timestamp: Set(Utc::now() - chrono::Duration::hours(1)),
            props: Set(Some(serde_json::json!({ "amount": amount }))),
            is_crawler: Set(true),
            crawler_name: Set(Some(crawler_name.to_string())),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("flagged event");
    }

    #[tokio::test]
    async fn goals_count_conversions_rates_and_revenue_per_source() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        let (project_id, env) = create_project(db.as_ref(), "goals-report").await;
        let service = AnalyticsGoalService::new(db.clone());

        let ads = create_visitor(db.as_ref(), project_id, env, "v-ads", "Paid Search").await;
        let mail = create_visitor(db.as_ref(), project_id, env, "v-mail", "Email").await;
        let idle = create_visitor(db.as_ref(), project_id, env, "v-idle", "Email").await;
        let bot = create_visitor(db.as_ref(), project_id, env, "v-bot", "Direct").await;
        for v in [ads, mail, idle] {
            create_event(
                db.as_ref(),
                project_id,
                env,
                v,
                "page_view",
                None,
                "/",
                None,
                false,
            )
            .await;
        }
        create_event(
            db.as_ref(),
            project_id,
            env,
            ads,
            "page_view",
            None,
            "/thank-you",
            None,
            false,
        )
        .await;
        // Three purchases by one visitor: one with an unparseable amount and
        // one whose amount would overflow the sum, both counted as purchases
        // without revenue rather than failing the report.
        let purchase = |amount: &str| Some(serde_json::json!({ "amount": amount }));
        create_event(
            db.as_ref(),
            project_id,
            env,
            ads,
            "custom",
            Some("purchase"),
            "/checkout",
            purchase("30.5"),
            false,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            env,
            ads,
            "custom",
            Some("purchase"),
            "/checkout",
            purchase("n/a"),
            false,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            env,
            mail,
            "custom",
            Some("purchase"),
            "/checkout",
            purchase("19.5"),
            false,
        )
        .await;
        let huge = "9".repeat(400);
        create_event(
            db.as_ref(),
            project_id,
            env,
            ads,
            "custom",
            Some("purchase"),
            "/checkout",
            purchase(&huge),
            false,
        )
        .await;
        // A purchase the backend recorded (no user agent, stored as the bot
        // `unknown`) converts; one a traffic filter excluded does not.
        create_flagged_purchase(db.as_ref(), project_id, env, mail, "10", "unknown").await;
        create_flagged_purchase(db.as_ref(), project_id, env, mail, "500", "datacenter").await;
        // Crawlers never convert.
        create_event(
            db.as_ref(),
            project_id,
            env,
            bot,
            "custom",
            Some("purchase"),
            "/checkout",
            purchase("999"),
            true,
        )
        .await;

        let mut input = event_input("Purchase", "purchase");
        input.revenue_property = Some("amount".into());
        input.revenue_currency = Some("usd".into());
        let purchase_goal = service.create(project_id, input).await.expect("create");
        let thanks = service
            .create(project_id, page_input("Thank you", "/thank-*"))
            .await
            .expect("create");
        let never = service
            .create(project_id, event_input("Never", "never_sent"))
            .await
            .expect("create");

        let start = Utc::now() - chrono::Duration::days(1);
        let end = Utc::now();
        let stats = service
            .stats(project_id, start, end, Some(env))
            .await
            .expect("stats");
        assert_eq!(stats.visitors, 3);
        let by_id: HashMap<i32, &AnalyticsGoalStats> =
            stats.goals.iter().map(|g| (g.goal_id, g)).collect();

        let p = by_id[&purchase_goal.id];
        assert_eq!((p.unique_conversions, p.total_conversions), (2, 5));
        assert!((p.conversion_rate - 66.666).abs() < 0.01);
        assert_eq!(
            p.revenue,
            Some(AnalyticsGoalRevenue {
                currency: "USD".into(),
                total: 60.0,
                average: 20.0
            })
        );

        let t = by_id[&thanks.id];
        assert_eq!((t.unique_conversions, t.total_conversions), (1, 1));
        assert!(t.revenue.is_none());

        let n = by_id[&never.id];
        assert_eq!(
            (n.unique_conversions, n.total_conversions, n.conversion_rate),
            (0, 0, 0.0)
        );
        assert_eq!(stats.goals[0].goal_id, purchase_goal.id, "creation order");

        let breakdown = service
            .breakdown(
                project_id,
                purchase_goal.id,
                AnalyticsGoalBreakdownQuery {
                    dimension: AnalyticsGoalBreakdownDimension::Channel,
                    start_date: start,
                    end_date: end,
                    environment_id: None,
                    limit: None,
                },
            )
            .await
            .expect("breakdown");
        let rows: Vec<_> = breakdown
            .items
            .iter()
            .map(|r| {
                (
                    r.value.as_deref(),
                    r.visitors,
                    r.unique_conversions,
                    r.total_conversions,
                    r.conversion_rate,
                )
            })
            .collect();
        assert_eq!(
            rows,
            // Equal unique conversions; the second purchase puts Paid Search first.
            vec![
                (Some("Paid Search"), 1, 1, 3, 100.0),
                (Some("Email"), 2, 1, 2, 50.0),
            ]
        );
        assert_eq!(
            breakdown.items[0].revenue.as_ref().map(|r| r.total),
            Some(30.5)
        );
    }

    #[tokio::test]
    async fn goal_crud_is_scoped_to_its_project_and_names_are_unique() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        let (project_id, _) = create_project(db.as_ref(), "goals-crud").await;
        let (other_project, _) = create_project(db.as_ref(), "goals-other").await;
        let service = AnalyticsGoalService::new(db.clone());

        let goal = service
            .create(project_id, event_input("Signup", "signup"))
            .await
            .expect("create");
        assert!(matches!(
            service
                .create(project_id, event_input("signup", "other"))
                .await,
            Err(AnalyticsGoalError::DuplicateName { .. })
        ));
        // Same name in another project is fine.
        service
            .create(other_project, event_input("Signup", "signup"))
            .await
            .expect("other project");

        assert!(matches!(
            service.get(other_project, goal.id).await,
            Err(AnalyticsGoalError::NotFound { goal_id, project_id: p }) if goal_id == goal.id && p == other_project
        ));

        let updated = service
            .update(project_id, goal.id, page_input("Signed up", "/welcome"))
            .await
            .expect("update");
        assert_eq!(updated.kind, AnalyticsGoalKind::Page);
        assert_eq!(updated.event_name, None);

        let list = service.list(project_id, None, None).await.expect("list");
        assert_eq!((list.total, list.page, list.page_size), (1, 1, 20));

        assert!(matches!(
            service.delete(other_project, goal.id).await,
            Err(AnalyticsGoalError::NotFound { .. })
        ));
        service.delete(project_id, goal.id).await.expect("delete");
        assert_eq!(
            service
                .list(project_id, None, None)
                .await
                .expect("list")
                .total,
            0
        );
    }

    #[tokio::test]
    async fn a_project_cannot_exceed_the_goal_limit() {
        let Some(test_db) = test_db().await else {
            return;
        };
        let db = test_db.db.clone();
        let (project_id, _) = create_project(db.as_ref(), "goals-limit").await;
        let service = AnalyticsGoalService::new(db.clone());
        for i in 0..MAX_GOALS_PER_PROJECT {
            service
                .create(project_id, event_input(&format!("Goal {i}"), "e"))
                .await
                .expect("create");
        }
        assert!(matches!(
            service
                .create(project_id, event_input("One more", "e"))
                .await,
            Err(AnalyticsGoalError::LimitReached { limit: 50, .. })
        ));
    }
}
