// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Management of project edge rules (ADR-071).
//!
//! The service owns validation, ordering and persistence. Enforcement is
//! [`crate::edge_rules::EdgeRuleGate`], which picks writes up from the change
//! feed on its own poll, so a save is enforced by every proxy within seconds
//! and the service never talks to the proxy.
//!
//! Writes to one project's rules are serialised by a transaction-scoped
//! advisory lock keyed on the project, so the 100-rule limit and the
//! positions cannot be raced. Edits carry the rule's `revision`; a stale one is
//! refused with [`EdgeRuleServiceError::RevisionConflict`] instead of
//! overwriting someone else's change.

use crate::edge_rules::definition_from_model;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, EntityTrait, FromQueryResult, QueryFilter, QueryOrder, Set, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use temps_edge_rules::describe::describe_rule;
use temps_edge_rules::{
    auto_name, limits, prepare_expression, validate_rule, Action, Decision, Group, Mode,
    ParsedSample, Program, RuleDefinition, RuleIssue, RuleOutcome, SampleRequest,
};
use temps_entities::{environments, project_edge_rules, projects};
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum EdgeRuleServiceError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Edge rule {rule_id} not found in project {project_id}")]
    RuleNotFound { project_id: i32, rule_id: i32 },

    #[error(
        "Edge rule '{rule}' in project {project_id} is invalid: {}",
        summarize(issues)
    )]
    Invalid {
        project_id: i32,
        rule: String,
        issues: Vec<RuleIssue>,
    },

    #[error("Project {project_id} already has {max} edge rules, the maximum")]
    TooManyRules { project_id: i32, max: usize },

    #[error(
        "Edge rule {rule_id} in project {project_id} was changed by someone else (you edited \
         revision {expected}, it is now at {current}); reload it and apply your change again"
    )]
    RevisionConflict {
        project_id: i32,
        rule_id: i32,
        expected: i64,
        current: i64,
    },

    #[error("Cannot reorder the edge rules of project {project_id}: {reason}")]
    InvalidOrder { project_id: i32, reason: String },

    #[error(
        "Edge rule {rule_id} in project {project_id} is stored in a form this version cannot \
         read ({reason}); delete it or save it again"
    )]
    Unreadable {
        project_id: i32,
        rule_id: i32,
        reason: String,
    },

    #[error("Database error while working on the edge rules of project {project_id}: {source}")]
    Database {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

fn summarize(issues: &[RuleIssue]) -> String {
    issues
        .iter()
        .map(|i| format!("{}: {}", i.target, i.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn db_error(project_id: i32) -> impl FnOnce(sea_orm::DbErr) -> EdgeRuleServiceError {
    move |source| EdgeRuleServiceError::Database { project_id, source }
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ---- API types ---------------------------------------------------------

/// One stored rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EdgeRuleResponse {
    pub id: i32,
    pub project_id: i32,
    /// `null` applies the rule to every environment of the project.
    pub environment_id: Option<i32>,
    /// 0-based evaluation order. The first rule that decides wins.
    pub position: i32,
    pub name: String,
    pub enabled: bool,
    pub mode: Mode,
    pub expression: Group,
    pub action: Action,
    /// Edit counter. Send it back on update so a concurrent change is
    /// refused instead of overwritten.
    pub revision: i64,
    /// The rule in one sentence, e.g. "Block with 403 when path starts with
    /// /admin".
    pub summary: String,
    /// Requests this rule has matched, across every proxy. Counters are
    /// flushed every few seconds.
    pub hits: i64,
    #[schema(value_type = Option<String>, format = "date-time", example = "2026-10-31T09:30:00Z")]
    pub last_hit_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = "date-time", example = "2026-10-31T09:30:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = "date-time", example = "2026-10-31T09:30:00Z")]
    pub updated_at: DateTime<Utc>,
}

/// A project's rules in evaluation order. The list is bounded by
/// `max_rules`, so it is returned whole rather than paginated: order is the
/// point, and a page of an ordered rule set cannot be reasoned about.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EdgeRuleListResponse {
    pub project_id: i32,
    pub rules: Vec<EdgeRuleResponse>,
    /// Maximum number of rules a project may hold.
    pub max_rules: usize,
}

/// Body for creating a rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateEdgeRuleRequest {
    /// Up to 80 characters. Left empty, a name is generated from the rule,
    /// e.g. "Block when path starts with /admin".
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `enforce` acts on matches; `log_only` records them without acting.
    #[serde(default = "default_mode")]
    pub mode: Mode,
    #[serde(default)]
    pub environment_id: Option<i32>,
    pub expression: Group,
    pub action: Action,
    /// Where to insert the rule (0 = first). Defaults to the end.
    #[serde(default)]
    pub position: Option<i32>,
}

fn default_true() -> bool {
    true
}

fn default_mode() -> Mode {
    Mode::Enforce
}

/// Body for changing a rule. Absent fields keep their value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateEdgeRuleRequest {
    /// The revision the change was made against. When given and the rule has
    /// moved on, the update is refused with 409.
    #[serde(default)]
    pub revision: Option<i64>,
    /// An empty name regenerates it from the rule.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub mode: Option<Mode>,
    /// `null` applies the rule to every environment.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<i32>)]
    pub environment_id: Option<Option<i32>>,
    #[serde(default)]
    pub expression: Option<Group>,
    #[serde(default)]
    pub action: Option<Action>,
}

/// Body for reordering: every rule id of the project, in the new order.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReorderEdgeRulesRequest {
    pub rule_ids: Vec<i32>,
}

/// A rule supplied to the dry run instead of the saved ones.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = EvaluateEdgeRuleInput)]
pub struct EvaluateRuleInput {
    /// Saved rule id, or omit for an unsaved draft.
    #[serde(default)]
    pub id: Option<i32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    #[serde(default)]
    pub environment_id: Option<i32>,
    pub expression: Group,
    pub action: Action,
}

/// Body for the dry run.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluateEdgeRulesRequest {
    pub request: SampleRequest,
    /// Rules to evaluate in order instead of the saved ones, e.g. an edited
    /// rule set before saving it.
    #[serde(default)]
    pub rules: Option<Vec<EvaluateRuleInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = EvaluateEdgeRuleTraceEntry)]
pub struct EvaluateTraceEntry {
    /// Saved rule id, or a negative placeholder for an unsaved draft
    /// (`-1` for the first draft, `-2` for the second, ...).
    pub rule_id: i32,
    pub name: String,
    pub outcome: RuleOutcome,
}

/// What the proxy would do with the request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluateEdgeRulesResponse {
    pub decision: Decision,
    /// The rule the proxy log would record: the deciding rule, or the first
    /// rule that matched in log-only mode.
    pub matched_rule_id: Option<i32>,
    pub matched_mode: Option<Mode>,
    /// The decision in one sentence.
    pub summary: String,
    pub trace: Vec<EvaluateTraceEntry>,
}

// ---- service -----------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct StatsRow {
    rule_id: i32,
    hits: i64,
    last_hit_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    count: i64,
}

pub struct EdgeRuleService {
    db: Arc<DatabaseConnection>,
}

/// A validated rule, ready to store.
struct Prepared {
    name: String,
    expression: Group,
    action: Action,
}

impl EdgeRuleService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn ensure_project<C: ConnectionTrait>(
        &self,
        conn: &C,
        project_id: i32,
    ) -> Result<(), EdgeRuleServiceError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(conn)
            .await
            .map_err(db_error(project_id))?;
        match project {
            Some(p) if !p.is_deleted => Ok(()),
            _ => Err(EdgeRuleServiceError::ProjectNotFound { project_id }),
        }
    }

    async fn ensure_environment<C: ConnectionTrait>(
        &self,
        conn: &C,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<(), EdgeRuleServiceError> {
        let Some(environment_id) = environment_id else {
            return Ok(());
        };
        let env = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(conn)
            .await
            .map_err(db_error(project_id))?;
        match env {
            Some(_) => Ok(()),
            None => Err(EdgeRuleServiceError::EnvironmentNotFound {
                project_id,
                environment_id,
            }),
        }
    }

    /// Serialises writers to one project's rules until the transaction ends.
    async fn lock_project(
        txn: &DatabaseTransaction,
        project_id: i32,
    ) -> Result<(), EdgeRuleServiceError> {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('project_edge_rules'), $1)",
            [project_id.into()],
        ))
        .await
        .map_err(db_error(project_id))?;
        Ok(())
    }

    fn prepare(
        project_id: i32,
        name: Option<&str>,
        mut expression: Group,
        action: Action,
    ) -> Result<Prepared, EdgeRuleServiceError> {
        prepare_expression(&mut expression);
        let name = match name.map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => auto_name(&expression, &action),
        };
        let issues = validate_rule(&name, &expression, &action);
        if !issues.is_empty() {
            return Err(EdgeRuleServiceError::Invalid {
                project_id,
                rule: name,
                issues,
            });
        }
        // Validation and compilation share their checks; compiling here as
        // well means a rule the proxy could not load is never stored.
        let probe = RuleDefinition {
            id: 0,
            revision: 0,
            name: name.clone(),
            enabled: true,
            mode: Mode::Enforce,
            environment_id: None,
            expression: expression.clone(),
            action: action.clone(),
        };
        if let Err(e) = Program::compile(std::slice::from_ref(&probe)) {
            return Err(EdgeRuleServiceError::Invalid {
                project_id,
                rule: name,
                issues: vec![RuleIssue {
                    target: "expression".into(),
                    message: e.to_string(),
                }],
            });
        }
        Ok(Prepared {
            name,
            expression,
            action,
        })
    }

    fn to_response(
        model: &project_edge_rules::Model,
        stats: Option<&StatsRow>,
    ) -> Result<EdgeRuleResponse, EdgeRuleServiceError> {
        let definition =
            definition_from_model(model).map_err(|e| EdgeRuleServiceError::Unreadable {
                project_id: model.project_id,
                rule_id: model.id,
                reason: e.to_string(),
            })?;
        Ok(EdgeRuleResponse {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            position: model.position,
            name: model.name.clone(),
            enabled: model.enabled,
            mode: definition.mode,
            summary: describe_rule(&definition.expression, &definition.action),
            expression: definition.expression,
            action: definition.action,
            revision: model.revision,
            hits: stats.map_or(0, |s| s.hits),
            last_hit_at: stats.and_then(|s| s.last_hit_at),
            created_at: model.created_at,
            updated_at: model.updated_at,
        })
    }

    async fn stats<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
    ) -> Result<HashMap<i32, StatsRow>, EdgeRuleServiceError> {
        let rows = StatsRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT s.rule_id, s.hits, s.last_hit_at FROM project_edge_rule_stats s \
             JOIN project_edge_rules r ON r.id = s.rule_id WHERE r.project_id = $1",
            [project_id.into()],
        ))
        .all(conn)
        .await
        .map_err(db_error(project_id))?;
        Ok(rows.into_iter().map(|r| (r.rule_id, r)).collect())
    }

    async fn rules<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
    ) -> Result<Vec<project_edge_rules::Model>, EdgeRuleServiceError> {
        project_edge_rules::Entity::find()
            .filter(project_edge_rules::Column::ProjectId.eq(project_id))
            .order_by_asc(project_edge_rules::Column::Position)
            .order_by_asc(project_edge_rules::Column::Id)
            .all(conn)
            .await
            .map_err(db_error(project_id))
    }

    async fn find_rule<C: ConnectionTrait>(
        conn: &C,
        project_id: i32,
        rule_id: i32,
    ) -> Result<project_edge_rules::Model, EdgeRuleServiceError> {
        project_edge_rules::Entity::find_by_id(rule_id)
            .filter(project_edge_rules::Column::ProjectId.eq(project_id))
            .one(conn)
            .await
            .map_err(db_error(project_id))?
            .ok_or(EdgeRuleServiceError::RuleNotFound {
                project_id,
                rule_id,
            })
    }

    /// Checks a rule without storing it. Returns the name it would be saved
    /// with and its one-line summary.
    pub fn validate(
        &self,
        project_id: i32,
        name: Option<&str>,
        expression: Group,
        action: Action,
    ) -> Result<(String, String), EdgeRuleServiceError> {
        let prepared = Self::prepare(project_id, name, expression, action)?;
        let summary = describe_rule(&prepared.expression, &prepared.action);
        Ok((prepared.name, summary))
    }

    /// Every rule of the project in evaluation order.
    pub async fn list(
        &self,
        project_id: i32,
    ) -> Result<EdgeRuleListResponse, EdgeRuleServiceError> {
        let conn = self.db.as_ref();
        self.ensure_project(conn, project_id).await?;
        let rules = Self::rules(conn, project_id).await?;
        let stats = Self::stats(conn, project_id).await?;
        let rules = rules
            .iter()
            .map(|m| Self::to_response(m, stats.get(&m.id)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EdgeRuleListResponse {
            project_id,
            rules,
            max_rules: limits::RULES_PER_PROJECT,
        })
    }

    pub async fn get(
        &self,
        project_id: i32,
        rule_id: i32,
    ) -> Result<EdgeRuleResponse, EdgeRuleServiceError> {
        let conn = self.db.as_ref();
        self.ensure_project(conn, project_id).await?;
        let model = Self::find_rule(conn, project_id, rule_id).await?;
        let stats = Self::stats(conn, project_id).await?;
        Self::to_response(&model, stats.get(&rule_id))
    }

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateEdgeRuleRequest,
        user_id: Option<i32>,
    ) -> Result<EdgeRuleResponse, EdgeRuleServiceError> {
        let prepared = Self::prepare(
            project_id,
            request.name.as_deref(),
            request.expression,
            request.action,
        )?;
        let txn = self.db.begin().await.map_err(db_error(project_id))?;
        self.ensure_project(&txn, project_id).await?;
        self.ensure_environment(&txn, project_id, request.environment_id)
            .await?;
        Self::lock_project(&txn, project_id).await?;

        let count = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS count FROM project_edge_rules WHERE project_id = $1",
            [project_id.into()],
        ))
        .one(&txn)
        .await
        .map_err(db_error(project_id))?
        .map_or(0, |r| r.count);
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        if count >= limits::RULES_PER_PROJECT {
            return Err(EdgeRuleServiceError::TooManyRules {
                project_id,
                max: limits::RULES_PER_PROJECT,
            });
        }
        let end = i32::try_from(count).unwrap_or(i32::MAX);
        let position = request.position.map_or(end, |p| p.clamp(0, end));
        if position < end {
            // Make room. The position constraint is deferred, so the
            // transient duplicates are fine until commit.
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE project_edge_rules SET position = position + 1 \
                 WHERE project_id = $1 AND position >= $2",
                [project_id.into(), position.into()],
            ))
            .await
            .map_err(db_error(project_id))?;
        }

        let model = project_edge_rules::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(request.environment_id),
            position: Set(position),
            name: Set(prepared.name),
            enabled: Set(request.enabled),
            mode: Set(request.mode.as_str().to_string()),
            expression: Set(to_json(project_id, &prepared.expression)?),
            action: Set(to_json(project_id, &prepared.action)?),
            revision: Set(1),
            created_by: Set(user_id),
            updated_by: Set(user_id),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(db_error(project_id))?;
        txn.commit().await.map_err(db_error(project_id))?;
        Self::to_response(&model, None)
    }

    /// Applies `request` and returns the rule before and after.
    pub async fn update(
        &self,
        project_id: i32,
        rule_id: i32,
        request: UpdateEdgeRuleRequest,
        user_id: Option<i32>,
    ) -> Result<(EdgeRuleResponse, EdgeRuleResponse), EdgeRuleServiceError> {
        let txn = self.db.begin().await.map_err(db_error(project_id))?;
        self.ensure_project(&txn, project_id).await?;
        Self::lock_project(&txn, project_id).await?;
        let current = Self::find_rule(&txn, project_id, rule_id).await?;
        if let Some(expected) = request.revision {
            if expected != current.revision {
                return Err(EdgeRuleServiceError::RevisionConflict {
                    project_id,
                    rule_id,
                    expected,
                    current: current.revision,
                });
            }
        }
        let stats = Self::stats(&txn, project_id).await?;
        let before = Self::to_response(&current, stats.get(&rule_id))?;

        let expression = request
            .expression
            .unwrap_or_else(|| before.expression.clone());
        let action = request.action.unwrap_or_else(|| before.action.clone());
        let name = request.name.as_deref().or(Some(before.name.as_str()));
        let prepared = Self::prepare(project_id, name, expression, action)?;
        let environment_id = request.environment_id.unwrap_or(current.environment_id);
        self.ensure_environment(&txn, project_id, environment_id)
            .await?;

        let mut active: project_edge_rules::ActiveModel = current.clone().into();
        active.name = Set(prepared.name);
        active.enabled = Set(request.enabled.unwrap_or(current.enabled));
        active.mode = Set(request.mode.unwrap_or(before.mode).as_str().to_string());
        active.environment_id = Set(environment_id);
        active.expression = Set(to_json(project_id, &prepared.expression)?);
        active.action = Set(to_json(project_id, &prepared.action)?);
        active.revision = Set(current.revision + 1);
        active.updated_by = Set(user_id);
        active.updated_at = Set(Utc::now());
        let model = active.update(&txn).await.map_err(db_error(project_id))?;
        txn.commit().await.map_err(db_error(project_id))?;
        let after = Self::to_response(&model, stats.get(&rule_id))?;
        Ok((before, after))
    }

    /// Deletes a rule and closes the gap it leaves in the order. Returns the
    /// deleted rule.
    pub async fn delete(
        &self,
        project_id: i32,
        rule_id: i32,
    ) -> Result<EdgeRuleResponse, EdgeRuleServiceError> {
        let txn = self.db.begin().await.map_err(db_error(project_id))?;
        self.ensure_project(&txn, project_id).await?;
        Self::lock_project(&txn, project_id).await?;
        let current = Self::find_rule(&txn, project_id, rule_id).await?;
        // A rule stored in an unreadable form must still be deletable.
        let response = Self::to_response(&current, None).unwrap_or_else(|e| {
            warn!(project_id, rule_id, error = %e, "Deleting an unreadable edge rule");
            unreadable_placeholder(&current)
        });
        project_edge_rules::Entity::delete_by_id(rule_id)
            .exec(&txn)
            .await
            .map_err(db_error(project_id))?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE project_edge_rules SET position = position - 1 \
             WHERE project_id = $1 AND position > $2",
            [project_id.into(), current.position.into()],
        ))
        .await
        .map_err(db_error(project_id))?;
        txn.commit().await.map_err(db_error(project_id))?;
        Ok(response)
    }

    /// Puts the project's rules in the order of `rule_ids`, which must name
    /// every rule exactly once.
    pub async fn reorder(
        &self,
        project_id: i32,
        request: ReorderEdgeRulesRequest,
    ) -> Result<EdgeRuleListResponse, EdgeRuleServiceError> {
        let txn = self.db.begin().await.map_err(db_error(project_id))?;
        self.ensure_project(&txn, project_id).await?;
        Self::lock_project(&txn, project_id).await?;
        let rules = Self::rules(&txn, project_id).await?;
        let existing: HashSet<i32> = rules.iter().map(|r| r.id).collect();
        let mut seen = HashSet::new();
        for id in &request.rule_ids {
            if !seen.insert(*id) {
                return Err(EdgeRuleServiceError::InvalidOrder {
                    project_id,
                    reason: format!("rule {id} is listed more than once"),
                });
            }
            if !existing.contains(id) {
                return Err(EdgeRuleServiceError::InvalidOrder {
                    project_id,
                    reason: format!("rule {id} does not belong to this project"),
                });
            }
        }
        if let Some(missing) = rules.iter().find(|r| !seen.contains(&r.id)) {
            return Err(EdgeRuleServiceError::InvalidOrder {
                project_id,
                reason: format!(
                    "rule {} is missing; list every rule of the project in its new order",
                    missing.id
                ),
            });
        }
        let current: HashMap<i32, i32> = rules.iter().map(|r| (r.id, r.position)).collect();
        for (index, id) in request.rule_ids.iter().enumerate() {
            let position = i32::try_from(index).unwrap_or(i32::MAX);
            if current.get(id) == Some(&position) {
                continue;
            }
            txn.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE project_edge_rules SET position = $1 WHERE id = $2 AND project_id = $3",
                [position.into(), (*id).into(), project_id.into()],
            ))
            .await
            .map_err(db_error(project_id))?;
        }
        txn.commit().await.map_err(db_error(project_id))?;
        self.list(project_id).await
    }

    /// What the proxy would do with `request.request`, against the saved
    /// rules or the ones supplied.
    pub async fn evaluate(
        &self,
        project_id: i32,
        request: EvaluateEdgeRulesRequest,
    ) -> Result<EvaluateEdgeRulesResponse, EdgeRuleServiceError> {
        let conn = self.db.as_ref();
        self.ensure_project(conn, project_id).await?;
        let definitions: Vec<RuleDefinition> = match request.rules {
            Some(inputs) => {
                let mut next_draft = -1;
                let mut out = Vec::with_capacity(inputs.len());
                if inputs.len() > limits::RULES_PER_PROJECT {
                    return Err(EdgeRuleServiceError::TooManyRules {
                        project_id,
                        max: limits::RULES_PER_PROJECT,
                    });
                }
                for input in inputs {
                    let id = input.id.unwrap_or_else(|| {
                        let id = next_draft;
                        next_draft -= 1;
                        id
                    });
                    let prepared = Self::prepare(
                        project_id,
                        input.name.as_deref(),
                        input.expression,
                        input.action,
                    )?;
                    out.push(RuleDefinition {
                        id,
                        revision: 0,
                        name: prepared.name,
                        enabled: input.enabled,
                        mode: input.mode,
                        environment_id: input.environment_id,
                        expression: prepared.expression,
                        action: prepared.action,
                    });
                }
                out
            }
            None => Self::rules(conn, project_id)
                .await?
                .iter()
                .map(|m| {
                    definition_from_model(m).map_err(|e| EdgeRuleServiceError::Unreadable {
                        project_id,
                        rule_id: m.id,
                        reason: e.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        let names: HashMap<i32, String> =
            definitions.iter().map(|d| (d.id, d.name.clone())).collect();
        let program =
            Program::compile(&definitions).map_err(|e| EdgeRuleServiceError::Invalid {
                project_id,
                rule: "rule set".into(),
                issues: vec![RuleIssue {
                    target: "expression".into(),
                    message: e.to_string(),
                }],
            })?;
        let sample = ParsedSample::new(request.request);
        let (evaluation, trace) = program.evaluate_traced(&sample);
        let summary = summarize_decision(&evaluation.decision, &names);
        Ok(EvaluateEdgeRulesResponse {
            summary,
            matched_rule_id: evaluation.first_match.map(|m| m.rule_id),
            matched_mode: evaluation.first_match.map(|m| m.mode),
            decision: evaluation.decision,
            trace: trace
                .into_iter()
                .map(|t| EvaluateTraceEntry {
                    name: names.get(&t.rule_id).cloned().unwrap_or_default(),
                    rule_id: t.rule_id,
                    outcome: t.outcome,
                })
                .collect(),
        })
    }
}

fn to_json<T: Serialize>(
    project_id: i32,
    value: &T,
) -> Result<serde_json::Value, EdgeRuleServiceError> {
    serde_json::to_value(value).map_err(|e| EdgeRuleServiceError::Invalid {
        project_id,
        rule: "rule".into(),
        issues: vec![RuleIssue {
            target: "expression".into(),
            message: format!("could not be encoded: {e}"),
        }],
    })
}

fn unreadable_placeholder(model: &project_edge_rules::Model) -> EdgeRuleResponse {
    EdgeRuleResponse {
        id: model.id,
        project_id: model.project_id,
        environment_id: model.environment_id,
        position: model.position,
        name: model.name.clone(),
        enabled: model.enabled,
        mode: Mode::parse(&model.mode).unwrap_or(Mode::LogOnly),
        expression: Group {
            id: String::new(),
            combinator: temps_edge_rules::Combinator::All,
            children: Vec::new(),
        },
        action: Action::Log,
        revision: model.revision,
        summary: "Unreadable rule".into(),
        hits: 0,
        last_hit_at: None,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

fn summarize_decision(decision: &Decision, names: &HashMap<i32, String>) -> String {
    let rule = |id: &i32| match names.get(id) {
        Some(name) if !name.is_empty() => format!("rule '{name}'"),
        _ => format!("rule {id}"),
    };
    match decision {
        Decision::Forward => "No rule decided; the request continues to the app.".into(),
        Decision::Block {
            rule_id, status, ..
        } => format!("Blocked with {status} by {}.", rule(rule_id)),
        Decision::Allow { rule_id } => format!(
            "Allowed by {}; the remaining edge rules are skipped.",
            rule(rule_id)
        ),
        Decision::Challenge { rule_id } => {
            format!("Challenged by {} before reaching the app.", rule(rule_id))
        }
        Decision::Redirect {
            rule_id,
            status,
            location,
        } => format!(
            "Redirected with {status} to {location} by {}.",
            rule(rule_id)
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

    pub(crate) async fn database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker not available, skipping edge rule database test");
                None
            }
            Err(error) => panic!("test database: {error}"),
        }
    }

    /// A project with one environment; returns `(project_id, environment_id)`.
    pub(crate) async fn project(db: &TestDatabase, slug: &str) -> (i32, i32) {
        let rows = db
            .query_sql(&format!(
                "WITH p AS (INSERT INTO projects (name, repo_name, repo_owner, directory, \
                 main_branch, preset, slug, created_at, updated_at) VALUES ('{slug}', '{slug}', \
                 'owner', '.', 'main', 'dockerfile', '{slug}', NOW(), NOW()) RETURNING id) \
                 INSERT INTO environments (name, slug, subdomain, host, upstreams, project_id, \
                 created_at, updated_at) SELECT 'production', 'production', '{slug}', \
                 '{slug}.example.test', '[]'::json, p.id, NOW(), NOW() FROM p \
                 RETURNING project_id, id"
            ))
            .await
            .expect("insert project and environment");
        let row = rows.first().expect("one environment row");
        (
            row.try_get("", "project_id").expect("project_id"),
            row.try_get("", "id").expect("environment id"),
        )
    }

    pub(crate) fn block_admin() -> CreateEdgeRuleRequest {
        serde_json::from_value(serde_json::json!({
            "expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "starts_with",
                 "values": ["/admin"], "case_insensitive": true}
            ]},
            "action": {"type": "block", "status": 403}
        }))
        .expect("valid request")
    }

    fn rule_on(path: &str, action: serde_json::Value) -> CreateEdgeRuleRequest {
        serde_json::from_value(serde_json::json!({
            "expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "starts_with", "values": [path]}
            ]},
            "action": action
        }))
        .expect("valid request")
    }

    fn sample(url: &str) -> SampleRequest {
        SampleRequest {
            url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn invalid_rules_are_refused_with_every_issue() {
        let request: CreateEdgeRuleRequest = serde_json::from_value(serde_json::json!({
            "expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "ip", "op": "in_cidr", "values": ["not-an-ip"]},
                {"kind": "condition", "field": "path", "op": "regex", "values": ["("]}
            ]},
            "action": {"type": "block", "status": 200}
        }))
        .expect("parses");
        match EdgeRuleService::prepare(7, None, request.expression, request.action) {
            Err(EdgeRuleServiceError::Invalid {
                project_id: 7,
                issues,
                ..
            }) => {
                assert!(issues.len() >= 3, "{issues:?}");
                assert!(issues.iter().any(|i| i.target == "action"));
            }
            Err(other) => panic!("expected Invalid, got {other}"),
            Ok(_) => panic!("expected Invalid, got a prepared rule"),
        }
    }

    #[test]
    fn an_empty_name_is_generated_from_the_rule() {
        let request = block_admin();
        let prepared = EdgeRuleService::prepare(1, Some("  "), request.expression, request.action)
            .unwrap_or_else(|e| panic!("valid rule: {e}"));
        assert_eq!(
            prepared.name,
            "Block when path starts with /admin (ignoring case)"
        );
        // Node ids are assigned so issues and the editor can address them.
        assert!(!prepared.expression.id.is_empty());
    }

    #[test]
    fn update_distinguishes_a_null_environment_from_an_absent_one() {
        let absent: UpdateEdgeRuleRequest =
            serde_json::from_str(r#"{"enabled": false}"#).expect("parses");
        assert_eq!(absent.environment_id, None);
        let null: UpdateEdgeRuleRequest =
            serde_json::from_str(r#"{"environment_id": null}"#).expect("parses");
        assert_eq!(null.environment_id, Some(None));
        let set: UpdateEdgeRuleRequest =
            serde_json::from_str(r#"{"environment_id": 4}"#).expect("parses");
        assert_eq!(set.environment_id, Some(Some(4)));
    }

    #[tokio::test]
    async fn rules_are_created_ordered_updated_and_deleted() {
        let Some(test_db) = database().await else {
            return;
        };
        let service = EdgeRuleService::new(test_db.connection_arc());
        let (project_id, environment_id) = project(&test_db, "edge-crud").await;

        let first = service
            .create(project_id, block_admin(), None)
            .await
            .unwrap_or_else(|e| panic!("create: {e}"));
        assert_eq!(first.position, 0);
        assert_eq!(first.revision, 1);
        assert_eq!(
            first.summary,
            "Block with 403 when path starts with /admin (ignoring case)"
        );

        let mut second = rule_on("/api", serde_json::json!({"type": "log"}));
        second.position = Some(0);
        second.environment_id = Some(environment_id);
        let second = service
            .create(project_id, second, None)
            .await
            .unwrap_or_else(|e| panic!("create at the top: {e}"));
        let listed = service
            .list(project_id)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let order: Vec<i32> = listed.rules.iter().map(|r| r.id).collect();
        assert_eq!(order, vec![second.id, first.id]);
        assert_eq!(listed.max_rules, 100);

        // A stale revision is refused; the current one applies.
        let stale = service
            .update(
                project_id,
                first.id,
                UpdateEdgeRuleRequest {
                    revision: Some(0),
                    enabled: Some(false),
                    ..Default::default()
                },
                None,
            )
            .await;
        assert!(matches!(
            stale,
            Err(EdgeRuleServiceError::RevisionConflict {
                expected: 0,
                current: 1,
                ..
            })
        ));
        let (before, after) = service
            .update(
                project_id,
                first.id,
                UpdateEdgeRuleRequest {
                    revision: Some(1),
                    mode: Some(Mode::LogOnly),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("update: {e}"));
        assert_eq!(before.mode, Mode::Enforce);
        assert_eq!(after.mode, Mode::LogOnly);
        assert_eq!(after.revision, 2);
        assert_eq!(after.name, before.name);

        let reordered = service
            .reorder(
                project_id,
                ReorderEdgeRulesRequest {
                    rule_ids: vec![first.id, second.id],
                },
            )
            .await
            .unwrap_or_else(|e| panic!("reorder: {e}"));
        let order: Vec<(i32, i32)> = reordered.rules.iter().map(|r| (r.id, r.position)).collect();
        assert_eq!(order, vec![(first.id, 0), (second.id, 1)]);

        let incomplete = service
            .reorder(
                project_id,
                ReorderEdgeRulesRequest {
                    rule_ids: vec![first.id],
                },
            )
            .await;
        assert!(matches!(
            incomplete,
            Err(EdgeRuleServiceError::InvalidOrder { .. })
        ));

        service
            .delete(project_id, first.id)
            .await
            .unwrap_or_else(|e| panic!("delete: {e}"));
        let listed = service
            .list(project_id)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(listed.rules.len(), 1);
        assert_eq!(listed.rules[0].position, 0, "the gap is closed");

        let missing = service.delete(project_id, first.id).await;
        assert!(matches!(
            missing,
            Err(EdgeRuleServiceError::RuleNotFound { rule_id, .. }) if rule_id == first.id
        ));
    }

    #[tokio::test]
    async fn writes_are_scoped_to_the_project_and_its_environments() {
        let Some(test_db) = database().await else {
            return;
        };
        let service = EdgeRuleService::new(test_db.connection_arc());
        let (project_a, _) = project(&test_db, "edge-scope-a").await;
        let (project_b, environment_b) = project(&test_db, "edge-scope-b").await;

        let mut foreign_env = block_admin();
        foreign_env.environment_id = Some(environment_b);
        assert!(matches!(
            service.create(project_a, foreign_env, None).await,
            Err(EdgeRuleServiceError::EnvironmentNotFound { .. })
        ));

        let rule = service
            .create(project_b, block_admin(), None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            service.get(project_a, rule.id).await,
            Err(EdgeRuleServiceError::RuleNotFound { .. })
        ));
        assert!(matches!(
            service.list(999_999).await,
            Err(EdgeRuleServiceError::ProjectNotFound {
                project_id: 999_999
            })
        ));
    }

    #[tokio::test]
    async fn the_rule_limit_is_enforced() {
        let Some(test_db) = database().await else {
            return;
        };
        let service = EdgeRuleService::new(test_db.connection_arc());
        let (project_id, _) = project(&test_db, "edge-limit").await;
        for _ in 0..limits::RULES_PER_PROJECT {
            service
                .create(project_id, block_admin(), None)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
        }
        assert!(matches!(
            service.create(project_id, block_admin(), None).await,
            Err(EdgeRuleServiceError::TooManyRules { max: 100, .. })
        ));
    }

    #[tokio::test]
    async fn the_dry_run_evaluates_saved_rules_and_drafts() {
        let Some(test_db) = database().await else {
            return;
        };
        let service = EdgeRuleService::new(test_db.connection_arc());
        let (project_id, _) = project(&test_db, "edge-dry-run").await;
        let saved = service
            .create(project_id, block_admin(), None)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        // Paths ignore case by default for this rule.
        let result = service
            .evaluate(
                project_id,
                EvaluateEdgeRulesRequest {
                    request: sample("https://app.example.test/ADMIN/users"),
                    rules: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            result.decision,
            Decision::Block {
                rule_id: saved.id,
                status: 403,
                body: String::new()
            }
        );
        assert_eq!(result.matched_rule_id, Some(saved.id));
        assert!(
            result.summary.starts_with("Blocked with 403"),
            "{}",
            result.summary
        );

        let drafts: Vec<EvaluateRuleInput> = serde_json::from_value(serde_json::json!([
            {"expression": {"combinator": "all", "children": [
                {"kind": "condition", "field": "path", "op": "eq", "values": ["/old"]}
            ]}, "action": {"type": "redirect", "target": "/new?{query}", "status": 308}}
        ]))
        .expect("drafts parse");
        let result = service
            .evaluate(
                project_id,
                EvaluateEdgeRulesRequest {
                    request: sample("https://app.example.test/old?x=1"),
                    rules: Some(drafts),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            result.decision,
            Decision::Redirect {
                rule_id: -1,
                status: 308,
                location: "/new?x=1".into()
            }
        );
        assert_eq!(result.trace.len(), 1);
        assert_eq!(result.trace[0].outcome, RuleOutcome::Decided);
    }
}
