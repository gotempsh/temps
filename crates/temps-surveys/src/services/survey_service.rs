// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Survey definitions, delivery and results.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    TransactionTrait,
};
use temps_entities::surveys;
use tracing::{info, warn};

use super::delivery::{Lookup, ProjectSurveys, SurveyDeliveryCache, MAX_ACTIVE_SURVEYS};
use super::results::{
    sanitize_answers, summarize, AnswerRow, AnswerValue, QuestionResults, DISMISSED_EVENT,
    MAX_DISTRIBUTION_ROWS, SENT_EVENT, SHOWN_EVENT,
};
use crate::error::SurveyError;
use crate::model::{validate, SurveyDefinition, SurveyQuestion, SurveyStatus, SurveyTargeting};
use crate::selection::DeliveredSurvey;

pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;
/// Results cover the last 30 days unless asked otherwise, and at most a year.
pub const DEFAULT_RESULTS_DAYS: i64 = 30;
pub const MAX_RESULTS_DAYS: i64 = 366;

pub fn normalize_pagination(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

/// A stored survey, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Survey {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub description: Option<String>,
    pub status: SurveyStatus,
    pub questions: Vec<SurveyQuestion>,
    pub targeting: SurveyTargeting,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<surveys::Model> for Survey {
    type Error = SurveyError;

    fn try_from(model: surveys::Model) -> Result<Self, Self::Error> {
        let corrupt = |field: &'static str, reason: String| SurveyError::Corrupt {
            survey_id: model.id,
            field,
            reason,
        };
        let status = SurveyStatus::parse(&model.status)
            .ok_or_else(|| corrupt("status", format!("unknown status '{}'", model.status)))?;
        let questions = serde_json::from_value(model.questions.clone())
            .map_err(|e| corrupt("questions", e.to_string()))?;
        let targeting = serde_json::from_value(model.targeting.clone())
            .map_err(|e| corrupt("targeting", e.to_string()))?;
        Ok(Survey {
            id: model.id,
            project_id: model.project_id,
            name: model.name,
            description: model.description,
            status,
            questions,
            targeting,
            created_at: model.created_at,
            updated_at: model.updated_at,
        })
    }
}

impl Survey {
    fn definition(&self) -> SurveyDefinition {
        SurveyDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            status: self.status,
            questions: self.questions.clone(),
            targeting: self.targeting.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateSurvey {
    pub name: String,
    pub description: Option<String>,
    pub status: SurveyStatus,
    pub questions: Vec<SurveyQuestion>,
    pub targeting: SurveyTargeting,
}

/// A partial update. `None` leaves a field as it is; `description:
/// Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct UpdateSurvey {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub status: Option<SurveyStatus>,
    pub questions: Option<Vec<SurveyQuestion>>,
    pub targeting: Option<SurveyTargeting>,
    /// `updated_at` as last read. When set, the write is refused if the
    /// survey changed since.
    pub expected_updated_at: Option<DateTime<Utc>>,
}

/// Time window and environment of a results query.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResultsWindow {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub environment_id: Option<i32>,
}

/// A resolved window: both ends set and checked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub environment_id: Option<i32>,
}

impl ResultsWindow {
    pub fn resolve(
        self,
        survey_id: i32,
        now: DateTime<Utc>,
    ) -> Result<ResolvedWindow, SurveyError> {
        let end = self.end.unwrap_or(now);
        let start = self
            .start
            .unwrap_or(end - Duration::days(DEFAULT_RESULTS_DAYS));
        if start >= end {
            return Err(SurveyError::InvalidQuery {
                survey_id,
                message: format!(
                    "start_date {} must be before end_date {}",
                    start.to_rfc3339(),
                    end.to_rfc3339()
                ),
            });
        }
        if end - start > Duration::days(MAX_RESULTS_DAYS) {
            return Err(SurveyError::InvalidQuery {
                survey_id,
                message: format!("the window may span at most {MAX_RESULTS_DAYS} days"),
            });
        }
        Ok(ResolvedWindow {
            start,
            end,
            environment_id: self.environment_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SurveyResults {
    pub window: ResolvedWindow,
    pub shown: i64,
    pub dismissed: i64,
    pub sent: i64,
    pub questions: Vec<QuestionResults>,
}

impl SurveyResults {
    /// Responses per survey shown. `None` before anyone saw it.
    pub fn response_rate(&self) -> Option<f64> {
        (self.shown > 0).then(|| self.sent as f64 / self.shown as f64)
    }
}

/// One submitted response.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmittedResponse {
    pub event_id: i64,
    pub timestamp: DateTime<Utc>,
    pub environment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub session_id: Option<String>,
    pub pathname: String,
    pub answers: HashMap<String, AnswerValue>,
}

#[derive(Debug, FromQueryResult)]
struct EventCountRow {
    event_type: String,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct AnswerCountRow {
    question_id: String,
    answer: Option<String>,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct AnsweredRow {
    question_id: String,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct ResponseRow {
    id: i64,
    timestamp: DateTime<Utc>,
    environment_id: Option<i32>,
    visitor_id: Option<i32>,
    session_id: Option<String>,
    pathname: String,
    responses: Option<serde_json::Value>,
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    count: i64,
}

/// Event properties: `props` (jsonb) for current writers, the legacy text
/// `event_data` column otherwise. Mirrors the funnels and analytics readers.
const PROPS: &str = "COALESCE(e.props, e.event_data::jsonb, '{}'::jsonb)";

pub struct SurveyService {
    db: Arc<DatabaseConnection>,
    delivery: SurveyDeliveryCache,
}

impl SurveyService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            delivery: SurveyDeliveryCache::new(),
        }
    }

    // ---------------------------------------------------------------------
    // Definitions
    // ---------------------------------------------------------------------

    pub async fn list(
        &self,
        project_id: i32,
        status: Option<SurveyStatus>,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<Survey>, u64), SurveyError> {
        let (page, page_size) = normalize_pagination(page, page_size);
        let mut query = surveys::Entity::find().filter(surveys::Column::ProjectId.eq(project_id));
        if let Some(status) = status {
            query = query.filter(surveys::Column::Status.eq(status.as_str()));
        }
        let paginator = query
            .order_by_desc(surveys::Column::CreatedAt)
            .order_by_desc(surveys::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let models = paginator.fetch_page(page - 1).await?;
        let surveys = models
            .into_iter()
            .map(Survey::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((surveys, total))
    }

    pub async fn get(&self, project_id: i32, survey_id: i32) -> Result<Survey, SurveyError> {
        Survey::try_from(self.find_model(project_id, survey_id).await?)
    }

    async fn find_model(
        &self,
        project_id: i32,
        survey_id: i32,
    ) -> Result<surveys::Model, SurveyError> {
        surveys::Entity::find_by_id(survey_id)
            .filter(surveys::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(SurveyError::NotFound {
                project_id,
                survey_id,
            })
    }

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateSurvey,
    ) -> Result<Survey, SurveyError> {
        let definition = SurveyDefinition {
            name: request.name.trim().to_string(),
            description: normalize_description(request.description),
            status: request.status,
            questions: request.questions,
            targeting: request.targeting,
        };
        validate(&definition).map_err(|message| SurveyError::Validation {
            name: definition.name.clone(),
            message,
        })?;
        if definition.status == SurveyStatus::Active {
            self.ensure_room_for_active(project_id, None, &definition.name)
                .await?;
        }

        let model = surveys::ActiveModel {
            project_id: Set(project_id),
            name: Set(definition.name.clone()),
            description: Set(definition.description.clone()),
            status: Set(definition.status.as_str().to_string()),
            questions: Set(to_json(
                &definition.name,
                "questions",
                &definition.questions,
            )?),
            targeting: Set(to_json(
                &definition.name,
                "targeting",
                &definition.targeting,
            )?),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        self.delivery.invalidate_project(project_id);
        info!(project_id, survey_id = model.id, name = %model.name, "Created survey");
        Survey::try_from(model)
    }

    /// Apply a partial update. Returns the survey before and after.
    pub async fn update(
        &self,
        project_id: i32,
        survey_id: i32,
        request: UpdateSurvey,
    ) -> Result<(Survey, Survey), SurveyError> {
        let txn = self.db.begin().await?;
        // Locked for the whole read-check-write, so the revision check and
        // the active-survey cap cannot race a concurrent edit.
        let model = surveys::Entity::find_by_id(survey_id)
            .filter(surveys::Column::ProjectId.eq(project_id))
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(SurveyError::NotFound {
                project_id,
                survey_id,
            })?;
        if let Some(expected) = request.expected_updated_at {
            if model.updated_at.timestamp_micros() != expected.timestamp_micros() {
                return Err(SurveyError::StaleWrite {
                    project_id,
                    survey_id,
                });
            }
        }
        let before = Survey::try_from(model.clone())?;

        let mut definition = before.definition();
        if let Some(name) = request.name {
            definition.name = name.trim().to_string();
        }
        if let Some(description) = request.description {
            definition.description = normalize_description(description);
        }
        if let Some(status) = request.status {
            definition.status = status;
        }
        if let Some(questions) = request.questions {
            definition.questions = questions;
        }
        if let Some(targeting) = request.targeting {
            definition.targeting = targeting;
        }
        validate(&definition).map_err(|message| SurveyError::Validation {
            name: definition.name.clone(),
            message,
        })?;
        if definition.status == SurveyStatus::Active && before.status != SurveyStatus::Active {
            let active = surveys::Entity::find()
                .filter(surveys::Column::ProjectId.eq(project_id))
                .filter(surveys::Column::Status.eq(SurveyStatus::Active.as_str()))
                .filter(surveys::Column::Id.ne(survey_id))
                .count(&txn)
                .await?;
            check_active_room(active, &definition.name)?;
        }

        let mut active: surveys::ActiveModel = model.into();
        active.name = Set(definition.name.clone());
        active.description = Set(definition.description.clone());
        active.status = Set(definition.status.as_str().to_string());
        active.questions = Set(to_json(
            &definition.name,
            "questions",
            &definition.questions,
        )?);
        active.targeting = Set(to_json(
            &definition.name,
            "targeting",
            &definition.targeting,
        )?);
        let updated = active.update(&txn).await?;
        txn.commit().await?;
        self.delivery.invalidate_project(project_id);
        info!(project_id, survey_id, status = %updated.status, "Updated survey");
        Ok((before, Survey::try_from(updated)?))
    }

    /// Delete a survey definition. Its answers stay in analytics events.
    pub async fn delete(&self, project_id: i32, survey_id: i32) -> Result<Survey, SurveyError> {
        let model = self.find_model(project_id, survey_id).await?;
        let survey = Survey::try_from(model.clone())?;
        // Scoped by project as well as id, and a row removed by a concurrent
        // delete is reported as not found rather than as success.
        let result = surveys::Entity::delete_many()
            .filter(surveys::Column::Id.eq(model.id))
            .filter(surveys::Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(SurveyError::NotFound {
                project_id,
                survey_id,
            });
        }
        self.delivery.invalidate_project(project_id);
        info!(project_id, survey_id, "Deleted survey");
        Ok(survey)
    }

    async fn ensure_room_for_active(
        &self,
        project_id: i32,
        except: Option<i32>,
        name: &str,
    ) -> Result<(), SurveyError> {
        let mut query = surveys::Entity::find()
            .filter(surveys::Column::ProjectId.eq(project_id))
            .filter(surveys::Column::Status.eq(SurveyStatus::Active.as_str()));
        if let Some(id) = except {
            query = query.filter(surveys::Column::Id.ne(id));
        }
        check_active_room(query.count(self.db.as_ref()).await?, name)
    }

    // ---------------------------------------------------------------------
    // Delivery
    // ---------------------------------------------------------------------

    /// The project's active surveys, from memory. The read path of
    /// `/api/_temps/surveys`; see [`super::delivery`] for the bounds.
    pub async fn active_surveys(
        &self,
        project_id: i32,
    ) -> Result<Arc<ProjectSurveys>, SurveyError> {
        match self.delivery.lookup(project_id) {
            Lookup::Hit {
                entry,
                refresh: false,
            } => Ok(entry),
            Lookup::Hit {
                entry,
                refresh: true,
            } => {
                let generation = self.delivery.generation();
                match self.load_active(project_id).await {
                    Ok(fresh) => {
                        let fresh = self.delivery.insert(fresh, generation);
                        self.delivery.release_refresh(&entry);
                        Ok(fresh)
                    }
                    Err(error) => {
                        warn!(
                            project_id,
                            %error,
                            "Failed to refresh cached surveys; serving the previous set"
                        );
                        self.delivery.release_refresh(&entry);
                        Ok(entry)
                    }
                }
            }
            Lookup::Miss => {
                let _permit = self.delivery.load_permit().acquire().await.map_err(|_| {
                    SurveyError::Validation {
                        name: "<delivery>".to_string(),
                        message: format!(
                            "survey delivery for project {project_id} is shutting down"
                        ),
                    }
                })?;
                match self.delivery.lookup(project_id) {
                    Lookup::Hit { entry, .. } => Ok(entry),
                    Lookup::Miss => {
                        let generation = self.delivery.generation();
                        let fresh = self.load_active(project_id).await?;
                        Ok(self.delivery.insert(fresh, generation))
                    }
                }
            }
        }
    }

    async fn load_active(&self, project_id: i32) -> Result<ProjectSurveys, SurveyError> {
        let models = surveys::Entity::find()
            .filter(surveys::Column::ProjectId.eq(project_id))
            .filter(surveys::Column::Status.eq(SurveyStatus::Active.as_str()))
            .order_by_asc(surveys::Column::CreatedAt)
            .order_by_asc(surveys::Column::Id)
            .limit(MAX_ACTIVE_SURVEYS as u64)
            .all(self.db.as_ref())
            .await?;
        let mut delivered = Vec::with_capacity(models.len());
        for model in models {
            // One unreadable row must not take every other survey down.
            match Survey::try_from(model) {
                Ok(survey) => delivered.push(DeliveredSurvey {
                    id: survey.id,
                    name: survey.name,
                    questions: survey.questions,
                    targeting: survey.targeting,
                }),
                Err(error) => warn!(project_id, %error, "Skipping unreadable survey"),
            }
        }
        Ok(ProjectSurveys::new(project_id, delivered))
    }

    // ---------------------------------------------------------------------
    // Results
    // ---------------------------------------------------------------------

    pub async fn results(
        &self,
        project_id: i32,
        survey_id: i32,
        window: ResultsWindow,
    ) -> Result<(Survey, SurveyResults), SurveyError> {
        let survey = self.get(project_id, survey_id).await?;
        let window = window.resolve(survey_id, Utc::now())?;
        let db = self.db.as_ref();

        let (filter, values) = event_filter(project_id, survey_id, &window, 4);
        let mut count_values = vec![
            SHOWN_EVENT.into(),
            DISMISSED_EVENT.into(),
            SENT_EVENT.into(),
        ];
        count_values.extend(values.clone());
        let counts = EventCountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT e.event_type, COUNT(*)::bigint AS count FROM events e \
                 WHERE e.event_type IN ($1, $2, $3) AND {filter} GROUP BY e.event_type"
            ),
            count_values,
        ))
        .all(db)
        .await?;
        let count_of = |name: &str| {
            counts
                .iter()
                .find(|row| row.event_type == name)
                .map_or(0, |row| row.count)
        };

        let question_ids = survey
            .questions
            .iter()
            .map(|q| q.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (filter, values) = event_filter(project_id, survey_id, &window, 3);
        let mut sent_values = vec![SENT_EVENT.into(), question_ids.into()];
        sent_values.extend(values);
        // Each answer is one row; a multiple-choice answer is one row per
        // selection. `#>> '{}'` renders a JSON scalar as text (`"Pro"` ->
        // `Pro`, `9` -> `9`).
        let responses = format!(
            "jsonb_each(CASE WHEN jsonb_typeof({PROPS}->'responses') = 'object' \
             THEN {PROPS}->'responses' ELSE '{{}}'::jsonb END) AS r(key, value)"
        );
        let answers = AnswerCountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT r.key AS question_id, \
                        (CASE WHEN jsonb_typeof(r.value) = 'array' THEN a.elem ELSE r.value END) #>> '{{}}' AS answer, \
                        COUNT(*)::bigint AS count \
                 FROM events e \
                 CROSS JOIN LATERAL {responses} \
                 LEFT JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(r.value) = 'array' \
                     THEN r.value ELSE '[]'::jsonb END) AS a(elem) ON true \
                 WHERE e.event_type = $1 AND r.key = ANY(string_to_array($2, ',')) \
                   AND NOT (jsonb_typeof(r.value) = 'array' AND a.elem IS NULL) \
                   AND {filter} \
                 GROUP BY 1, 2 ORDER BY 3 DESC LIMIT {MAX_DISTRIBUTION_ROWS}"
            ),
            sent_values.clone(),
        ))
        .all(db)
        .await?;
        let answered = AnsweredRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT r.key AS question_id, COUNT(*)::bigint AS count \
                 FROM events e CROSS JOIN LATERAL {responses} \
                 WHERE e.event_type = $1 AND r.key = ANY(string_to_array($2, ',')) \
                   AND r.value NOT IN ('null'::jsonb, '\"\"'::jsonb, '[]'::jsonb) \
                   AND {filter} \
                 GROUP BY 1"
            ),
            sent_values,
        ))
        .all(db)
        .await?;

        let rows: Vec<AnswerRow> = answers
            .into_iter()
            .filter_map(|row| {
                row.answer.map(|answer| AnswerRow {
                    question_id: row.question_id,
                    answer,
                    count: row.count,
                })
            })
            .collect();
        let answered: HashMap<String, i64> = answered
            .into_iter()
            .map(|row| (row.question_id, row.count))
            .collect();
        let results = SurveyResults {
            window,
            shown: count_of(SHOWN_EVENT),
            dismissed: count_of(DISMISSED_EVENT),
            sent: count_of(SENT_EVENT),
            questions: summarize(&survey.questions, &answered, &rows),
        };
        Ok((survey, results))
    }

    /// Submitted responses, newest first.
    pub async fn responses(
        &self,
        project_id: i32,
        survey_id: i32,
        window: ResultsWindow,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<SubmittedResponse>, u64), SurveyError> {
        let survey = self.get(project_id, survey_id).await?;
        let window = window.resolve(survey_id, Utc::now())?;
        let (page, page_size) = normalize_pagination(page, page_size);
        let db = self.db.as_ref();

        let (filter, values) = event_filter(project_id, survey_id, &window, 2);
        let mut all_values = vec![SENT_EVENT.into()];
        all_values.extend(values);
        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!("SELECT COUNT(*)::bigint AS count FROM events e WHERE e.event_type = $1 AND {filter}"),
            all_values.clone(),
        ))
        .one(db)
        .await?
        .map_or(0, |row| row.count);

        let offset = (page - 1) * page_size;
        let rows = ResponseRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT e.id, e.timestamp, e.environment_id, e.visitor_id, e.session_id, e.pathname, \
                        {PROPS}->'responses' AS responses \
                 FROM events e WHERE e.event_type = $1 AND {filter} \
                 ORDER BY e.timestamp DESC, e.id DESC LIMIT {page_size} OFFSET {offset}"
            ),
            all_values,
        ))
        .all(db)
        .await?;
        let responses = rows
            .into_iter()
            .map(|row| SubmittedResponse {
                event_id: row.id,
                timestamp: row.timestamp,
                environment_id: row.environment_id,
                visitor_id: row.visitor_id,
                session_id: row.session_id,
                pathname: row.pathname,
                answers: sanitize_answers(&survey.questions, row.responses.as_ref()),
            })
            .collect();
        Ok((responses, u64::try_from(total).unwrap_or(0)))
    }

    #[cfg(test)]
    pub(crate) fn delivery(&self) -> &SurveyDeliveryCache {
        &self.delivery
    }
}

/// `WHERE` clauses shared by every results query, numbered from `first`.
fn event_filter(
    project_id: i32,
    survey_id: i32,
    window: &ResolvedWindow,
    first: usize,
) -> (String, Vec<sea_orm::Value>) {
    let mut clauses = vec![
        format!("e.project_id = ${first}"),
        format!("e.timestamp >= ${}", first + 1),
        format!("e.timestamp < ${}", first + 2),
        format!("{PROPS}->>'survey_id' = ${}", first + 3),
    ];
    let mut values: Vec<sea_orm::Value> = vec![
        project_id.into(),
        window.start.into(),
        window.end.into(),
        survey_id.to_string().into(),
    ];
    if let Some(environment_id) = window.environment_id {
        clauses.push(format!("e.environment_id = ${}", first + 4));
        values.push(environment_id.into());
    }
    (clauses.join(" AND "), values)
}

fn check_active_room(active: u64, name: &str) -> Result<(), SurveyError> {
    if active >= MAX_ACTIVE_SURVEYS as u64 {
        return Err(SurveyError::Validation {
            name: name.to_string(),
            message: format!(
                "the project already has {active} active surveys, the maximum is \
                 {MAX_ACTIVE_SURVEYS}; pause or complete one first"
            ),
        });
    }
    Ok(())
}

fn normalize_description(description: Option<String>) -> Option<String> {
    description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

fn to_json<T: serde::Serialize>(
    name: &str,
    field: &'static str,
    value: &T,
) -> Result<serde_json::Value, SurveyError> {
    serde_json::to_value(value).map_err(|e| SurveyError::Serialization {
        name: name.to_string(),
        field,
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QuestionKind;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn question(id: &str, kind: QuestionKind) -> SurveyQuestion {
        SurveyQuestion {
            id: id.into(),
            kind,
            prompt: "How likely are you to recommend us?".into(),
            optional: false,
            choices: Vec::new(),
            scale: None,
            lower_label: None,
            upper_label: None,
        }
    }

    fn model(id: i32, project_id: i32, status: &str) -> surveys::Model {
        surveys::Model {
            id,
            project_id,
            name: format!("Survey {id}"),
            description: None,
            status: status.into(),
            questions: serde_json::to_value(vec![question("nps", QuestionKind::Nps)]).unwrap(),
            targeting: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn service(db: MockDatabase) -> SurveyService {
        SurveyService::new(Arc::new(db.into_connection()))
    }

    /// The statements the service ran. Consumes it so the mock connection
    /// can be unwrapped.
    fn transaction_log(service: SurveyService) -> Vec<sea_orm::Transaction> {
        let db = Arc::try_unwrap(service.db)
            .unwrap_or_else(|_| panic!("the test still shares the mock connection"));
        db.into_transaction_log()
    }

    /// The SQL of every statement the service ran, in order.
    fn statements(service: SurveyService) -> Vec<String> {
        transaction_log(service)
            .iter()
            .flat_map(|t| t.statements().iter().map(|s| s.sql.clone()))
            .collect()
    }

    fn count(n: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([("num_items".to_string(), Value::BigInt(Some(n)))])
    }

    fn create_request(status: SurveyStatus) -> CreateSurvey {
        CreateSurvey {
            name: "  NPS  ".into(),
            description: Some("   ".into()),
            status,
            questions: vec![question("nps", QuestionKind::Nps)],
            targeting: SurveyTargeting::default(),
        }
    }

    #[tokio::test]
    async fn create_trims_and_stores_a_valid_survey() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model(1, 7, "draft")]]);
        let service = service(db);
        let survey = service
            .create(7, create_request(SurveyStatus::Draft))
            .await
            .unwrap();
        assert_eq!(survey.id, 1);
        let log = transaction_log(service);
        let rendered = format!("{log:?}");
        assert!(rendered.contains("\"NPS\""), "{rendered}");
        // A blank description is stored as NULL, not as spaces.
        assert!(!rendered.contains("\"   \""), "{rendered}");
    }

    #[tokio::test]
    async fn create_rejects_an_invalid_survey_without_touching_the_database() {
        let service = service(MockDatabase::new(DatabaseBackend::Postgres));
        let mut request = create_request(SurveyStatus::Active);
        request.questions.clear();
        let error = service.create(7, request).await.unwrap_err();
        assert!(
            matches!(&error, SurveyError::Validation { name, message }
                if name == "NPS" && message.contains("at least one question")),
            "{error}"
        );
        assert!(transaction_log(service).is_empty());
    }

    #[tokio::test]
    async fn activating_beyond_the_cap_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count(MAX_ACTIVE_SURVEYS as i64)]]);
        let error = service(db)
            .create(7, create_request(SurveyStatus::Active))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maximum is 50"), "{error}");
    }

    #[tokio::test]
    async fn get_is_scoped_to_the_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<surveys::Model>::new()]);
        let service = service(db);
        let error = service.get(8, 1).await.unwrap_err();
        assert!(matches!(
            error,
            SurveyError::NotFound {
                project_id: 8,
                survey_id: 1
            }
        ));
        let sql = statements(service);
        assert!(sql[0].contains("\"project_id\" = $"), "{sql:?}");
    }

    #[tokio::test]
    async fn a_stale_update_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model(1, 7, "draft")]]);
        let error = service(db)
            .update(
                7,
                1,
                UpdateSurvey {
                    name: Some("Renamed".into()),
                    expected_updated_at: Some(Utc::now() - Duration::hours(1)),
                    ..UpdateSurvey::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SurveyError::StaleWrite {
                project_id: 7,
                survey_id: 1
            }
        ));
    }

    #[tokio::test]
    async fn an_update_returns_before_and_after_and_drops_the_cache() {
        let before = model(1, 7, "draft");
        let mut after = before.clone();
        after.status = "active".into();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![before.clone()]])
            .append_query_results(vec![vec![count(0)]])
            .append_query_results(vec![vec![after]]);
        let service = service(db);
        let generation = service.delivery().generation();
        let (old, new) = service
            .update(
                7,
                1,
                UpdateSurvey {
                    status: Some(SurveyStatus::Active),
                    expected_updated_at: Some(before.updated_at),
                    ..UpdateSurvey::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(old.status, SurveyStatus::Draft);
        assert_eq!(new.status, SurveyStatus::Active);
        assert!(service.delivery().generation() > generation);
        let log = transaction_log(service);
        assert!(format!("{log:?}").contains("FOR UPDATE"));
    }

    #[tokio::test]
    async fn active_surveys_are_cached_and_skip_unreadable_rows() {
        let mut broken = model(2, 7, "active");
        broken.questions = serde_json::json!("not a list");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model(1, 7, "active"), broken]]);
        let service = service(db);
        let first = service.active_surveys(7).await.unwrap();
        assert_eq!(
            first.surveys.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1]
        );
        // Served from memory: the mock has no second result to give.
        let second = service.active_surveys(7).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn delete_removes_only_a_survey_of_the_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model(1, 7, "paused")]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);
        let service = service(db);
        let deleted = service.delete(7, 1).await.unwrap();
        assert_eq!(deleted.status, SurveyStatus::Paused);
        let sql = statements(service);
        assert_eq!(sql.len(), 2, "{sql:?}");
        assert!(sql[1].starts_with("DELETE FROM \"surveys\""), "{sql:?}");
        assert!(sql[1].contains("\"project_id\" = $"), "{sql:?}");
    }

    #[test]
    fn a_window_defaults_to_thirty_days_and_is_bounded() {
        let now = Utc::now();
        let resolved = ResultsWindow::default().resolve(1, now).unwrap();
        assert_eq!(resolved.end, now);
        assert_eq!(resolved.end - resolved.start, Duration::days(30));
        let backwards = ResultsWindow {
            start: Some(now),
            end: Some(now - Duration::days(1)),
            environment_id: None,
        };
        assert!(matches!(
            backwards.resolve(1, now),
            Err(SurveyError::InvalidQuery { survey_id: 1, .. })
        ));
        let too_long = ResultsWindow {
            start: Some(now - Duration::days(400)),
            end: Some(now),
            environment_id: None,
        };
        assert!(too_long.resolve(1, now).is_err());
    }

    #[test]
    fn the_event_filter_binds_every_value() {
        let window = ResolvedWindow {
            start: Utc::now() - Duration::days(1),
            end: Utc::now(),
            environment_id: Some(3),
        };
        let (sql, values) = event_filter(7, 12, &window, 2);
        assert_eq!(values.len(), 5);
        assert!(sql.contains("e.project_id = $2"), "{sql}");
        assert!(sql.contains("->>'survey_id' = $5"), "{sql}");
        assert!(sql.contains("e.environment_id = $6"), "{sql}");
        assert!(!sql.contains('7') && !sql.contains("12"), "{sql}");
    }

    #[tokio::test]
    async fn results_combine_counts_and_answers() {
        let mut survey = model(1, 7, "active");
        survey.questions = serde_json::to_value(vec![question("nps", QuestionKind::Nps)]).unwrap();
        let row = |pairs: Vec<(&str, Value)>| -> BTreeMap<String, Value> {
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![survey]])
            .append_query_results(vec![vec![
                row(vec![
                    ("event_type", "survey_shown".into()),
                    ("count", Value::BigInt(Some(10))),
                ]),
                row(vec![
                    ("event_type", "survey_sent".into()),
                    ("count", Value::BigInt(Some(4))),
                ]),
            ]])
            .append_query_results(vec![vec![
                row(vec![
                    ("question_id", "nps".into()),
                    ("answer", Value::String(Some(Box::new("10".into())))),
                    ("count", Value::BigInt(Some(3))),
                ]),
                row(vec![
                    ("question_id", "nps".into()),
                    ("answer", Value::String(Some(Box::new("2".into())))),
                    ("count", Value::BigInt(Some(1))),
                ]),
            ]])
            .append_query_results(vec![vec![row(vec![
                ("question_id", "nps".into()),
                ("count", Value::BigInt(Some(4))),
            ])]]);
        let (_, results) = service(db)
            .results(7, 1, ResultsWindow::default())
            .await
            .unwrap();
        assert_eq!((results.shown, results.dismissed, results.sent), (10, 0, 4));
        assert_eq!(results.response_rate(), Some(0.4));
        let nps = results.questions[0].nps.as_ref().unwrap();
        assert_eq!((nps.promoters, nps.detractors), (3, 1));
        assert_eq!(results.questions[0].answered, 4);
    }
}
