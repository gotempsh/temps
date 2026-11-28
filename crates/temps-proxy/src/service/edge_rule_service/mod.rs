// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Management of project edge rules (ADR-089).
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
    auto_name, limits, prepare_expression, validate_rule_with, Action, Decision, EmptyExpression,
    Group, Mode, ParsedSample, PatternUsage, Program, RuleDefinition, RuleIssue, RuleOutcome,
    SampleRequest,
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
        "Edge rule '{}' in project {project_id} is invalid: {}",
        echoed_name(rule),
        summarize(issues)
    )]
    Invalid {
        project_id: i32,
        rule: String,
        issues: Vec<RuleIssue>,
    },

    #[error(
        "Position {position} is outside the edge rules of project {project_id}: use 0 (first) \
         to {max} (after the last rule), or leave position out to add the rule last"
    )]
    InvalidPosition {
        project_id: i32,
        position: i32,
        max: i32,
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

/// Longest part of a rule name an error repeats. A refused name can be as long
/// as the request body, and the error must not echo all of it.
const MAX_ECHOED_NAME_CHARS: usize = limits::NAME_LENGTH;

/// `name` as an error repeats it: whole up to the name limit, otherwise cut
/// there and followed by `…`.
fn echoed_name(name: &str) -> String {
    if name.chars().count() <= MAX_ECHOED_NAME_CHARS {
        return name.to_string();
    }
    let mut shown: String = name.chars().take(MAX_ECHOED_NAME_CHARS).collect();
    shown.push('…');
    shown
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
    /// `enforce` acts on matches; `log_only` (the default) records them
    /// without acting, so a new rule can be watched before it enforces.
    #[serde(default = "default_create_mode")]
    pub mode: Mode,
    #[serde(default)]
    pub environment_id: Option<i32>,
    pub expression: Group,
    pub action: Action,
    /// Where to insert the rule: 0 is first, and the number of rules the
    /// project has is after the last one. Defaults to the end. A position
    /// outside that range is refused.
    #[serde(default)]
    pub position: Option<i32>,
}

fn default_true() -> bool {
    true
}

/// Dry-run drafts act as they would once enforced.
fn default_mode() -> Mode {
    Mode::Enforce
}

/// New rules start by only recording what they match.
fn default_create_mode() -> Mode {
    Mode::LogOnly
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
    /// `null` applies the rule to every environment; omit to keep the current
    /// one. A new environment must be one of the project's; the current one
    /// is kept as is, even if it has since been deleted.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<i32>, nullable)]
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

    /// Refuses a write that would take the project's rules over their
    /// pattern budget ([`PatternUsage`]): each regex, glob or
    /// case-insensitive substring condition is its own automaton in every
    /// proxy's memory. `except` is the rule being replaced. Runs under the
    /// project lock, so concurrent writes cannot both pass.
    /// Wakes the worker route sync when a write may change whether the
    /// project has an enabled rule (ADR-089 §8): workers serve no public
    /// route while any project has one, and the NOTIFY makes them pick that
    /// up now rather than at the end of their current long poll (up to 25 s
    /// later). Sent inside the write's transaction, so it is delivered only
    /// if the write commits.
    async fn announce_route_change(
        txn: &DatabaseTransaction,
        project_id: i32,
    ) -> Result<(), EdgeRuleServiceError> {
        let payload = serde_json::json!({
            "action": "UPDATE",
            "project_id": project_id,
            "field": "edge_rules.enabled",
        })
        .to_string();
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_notify('project_route_change', $1)",
            [payload.into()],
        ))
        .await
        .map_err(db_error(project_id))?;
        Ok(())
    }

    async fn check_pattern_budget(
        txn: &DatabaseTransaction,
        project_id: i32,
        except: Option<i32>,
        rule: &str,
        expression: &Group,
    ) -> Result<(), EdgeRuleServiceError> {
        let adding = PatternUsage::of(expression);
        if adding.is_empty() {
            return Ok(());
        }
        // At most `limits::RULES_PER_PROJECT` rows, read on a write only.
        let mut total = adding;
        for model in Self::rules(txn, project_id).await? {
            if Some(model.id) == except {
                continue;
            }
            let other =
                definition_from_model(&model).map_err(|e| EdgeRuleServiceError::Unreadable {
                    project_id,
                    rule_id: model.id,
                    reason: e.to_string(),
                })?;
            total = total.plus(PatternUsage::of(&other.expression));
        }
        Self::refuse_over_budget(project_id, rule, total)
    }

    fn refuse_over_budget(
        project_id: i32,
        rule: &str,
        usage: PatternUsage,
    ) -> Result<(), EdgeRuleServiceError> {
        match usage.over_budget() {
            None => Ok(()),
            Some(message) => Err(EdgeRuleServiceError::Invalid {
                project_id,
                rule: rule.to_string(),
                issues: vec![RuleIssue {
                    target: "expression".into(),
                    message,
                }],
            }),
        }
    }

    fn prepare(
        project_id: i32,
        name: Option<&str>,
        mut expression: Group,
        action: Action,
        empty: EmptyExpression,
    ) -> Result<Prepared, EdgeRuleServiceError> {
        prepare_expression(&mut expression);
        let name = match name.map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => auto_name(&expression, &action),
        };
        let issues = validate_rule_with(&name, &expression, &action, empty);
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
        let prepared = Self::prepare(
            project_id,
            name,
            expression,
            action,
            EmptyExpression::Refuse,
        )?;
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
            EmptyExpression::Refuse,
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
        Self::check_pattern_budget(&txn, project_id, None, &prepared.name, &prepared.expression)
            .await?;
        let end = i32::try_from(count).unwrap_or(i32::MAX);
        let position = match request.position {
            None => end,
            Some(position) if (0..=end).contains(&position) => position,
            Some(position) => {
                return Err(EdgeRuleServiceError::InvalidPosition {
                    project_id,
                    position,
                    max: end,
                })
            }
        };
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
        if model.enabled {
            Self::announce_route_change(&txn, project_id).await?;
        }
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

        // An editor that saves the whole rule resends its expression; one
        // equal to the stored expression is not a change.
        let expression_changed = request
            .expression
            .as_ref()
            .is_some_and(|expression| expression != &before.expression);
        let expression = request
            .expression
            .unwrap_or_else(|| before.expression.clone());
        let action = request.action.unwrap_or_else(|| before.action.clone());
        let name = request.name.as_deref().or(Some(before.name.as_str()));
        let enabled = request.enabled.unwrap_or(current.enabled);
        // A rule stored before empty expressions were refused can still be
        // turned off or renamed while off; enabling it, or writing a new
        // expression, needs a condition.
        let empty = if expression_changed || enabled {
            EmptyExpression::Refuse
        } else {
            EmptyExpression::Allow
        };
        let prepared = Self::prepare(project_id, name, expression, action, empty)?;
        if expression_changed {
            Self::check_pattern_budget(
                &txn,
                project_id,
                Some(rule_id),
                &prepared.name,
                &prepared.expression,
            )
            .await?;
        }
        let environment_id = request.environment_id.unwrap_or(current.environment_id);
        // Only an environment this request moves the rule to is checked. A
        // rule whose environment was deleted keeps that binding (it matches
        // no traffic) and can still be renamed, disabled or edited (#2111);
        // a client that sends the stored value back unchanged is not refused.
        if environment_id != current.environment_id {
            self.ensure_environment(&txn, project_id, environment_id)
                .await?;
        }

        let mut active: project_edge_rules::ActiveModel = current.clone().into();
        active.name = Set(prepared.name);
        active.enabled = Set(enabled);
        active.mode = Set(request.mode.unwrap_or(before.mode).as_str().to_string());
        active.environment_id = Set(environment_id);
        active.expression = Set(to_json(project_id, &prepared.expression)?);
        active.action = Set(to_json(project_id, &prepared.action)?);
        active.revision = Set(current.revision + 1);
        active.updated_by = Set(user_id);
        active.updated_at = Set(Utc::now());
        let model = active.update(&txn).await.map_err(db_error(project_id))?;
        if model.enabled != current.enabled {
            Self::announce_route_change(&txn, project_id).await?;
        }
        txn.commit().await.map_err(db_error(project_id))?;
        let after = Self::to_response(&model, stats.get(&rule_id))?;
        Ok((before, after))
    }

    /// Deletes a rule and closes the gap it leaves in the order. Returns the
    /// deleted rule. With `expected_revision`, a rule that has changed since
    /// is refused with [`EdgeRuleServiceError::RevisionConflict`], so a
    /// confirmed deletion removes the rule that was reviewed.
    pub async fn delete(
        &self,
        project_id: i32,
        rule_id: i32,
        expected_revision: Option<i64>,
    ) -> Result<EdgeRuleResponse, EdgeRuleServiceError> {
        let txn = self.db.begin().await.map_err(db_error(project_id))?;
        self.ensure_project(&txn, project_id).await?;
        Self::lock_project(&txn, project_id).await?;
        let current = Self::find_rule(&txn, project_id, rule_id).await?;
        if let Some(expected) = expected_revision {
            if expected != current.revision {
                return Err(EdgeRuleServiceError::RevisionConflict {
                    project_id,
                    rule_id,
                    expected,
                    current: current.revision,
                });
            }
        }
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
        if current.enabled {
            Self::announce_route_change(&txn, project_id).await?;
        }
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
                // Before anything is compiled: a dry run is open to readers,
                // so it gets the same budget as a write.
                let usage = inputs
                    .iter()
                    .map(|input| PatternUsage::of(&input.expression))
                    .fold(PatternUsage::default(), PatternUsage::plus);
                Self::refuse_over_budget(project_id, "rule set", usage)?;
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
                        EmptyExpression::Refuse,
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
pub(crate) mod tests;
