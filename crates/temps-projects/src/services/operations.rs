// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read-only feed of long-running operations for the console's operations tray.
//!
//! Every entry is *derived* from a table that already records the operation —
//! there is no operations table and no new write path:
//!
//! | Kind | Source table | Notes |
//! |---|---|---|
//! | `deployment` / `rollback` / `promotion` | `deployments` | A redeploy is indistinguishable from a deployment: no column records it, so it is reported as `deployment`. |
//! | `restore` | `restore_runs` | Scoped through the source service's `project_services` links. |
//! | `backup` | `backups` (+ `external_service_backups`) | Control-plane backups have no service link and are visible to instance administrators only. |
//! | `autofix` | `agent_runs` where `trigger_type = 'autofixer'` | |
//!
//! The four sources are merged with a single `UNION ALL` statement so the
//! result can be ordered and paginated in the database (`created_at DESC`,
//! then the stable entry id). Rows are restricted to a recency window
//! ([`OPERATIONS_RECENCY_WINDOW_DAYS`]) plus anything still active, which keeps
//! the union small no matter how much history the instance has.
//!
//! Every user-controlled value (window start, project ids, hidden project ids,
//! kind, limit, offset) is a bound parameter. The only text interpolated into
//! the SQL is fixed vocabulary owned by this module (state names, kind names).

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{DatabaseBackend, DatabaseConnection, DbErr, FromQueryResult, Statement, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::error;
use utoipa::ToSchema;

/// Finished operations older than this are left out of the feed. Anything
/// still active is always included, however old, so a stuck operation can't
/// silently drop out of view.
pub const OPERATIONS_RECENCY_WINDOW_DAYS: i64 = 7;

/// Default page size for `GET /operations`.
pub const OPERATIONS_DEFAULT_PAGE_SIZE: u64 = 20;

/// Maximum page size for `GET /operations`.
pub const OPERATIONS_MAX_PAGE_SIZE: u64 = 100;

/// What kind of operation an entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// A deployment (including redeploys, which are not distinguishable).
    Deployment,
    /// A rollback to an earlier deployment.
    Rollback,
    /// A promotion of a deployment into another environment.
    Promotion,
    /// A restore of a storage service from a backup.
    Restore,
    /// A backup run.
    Backup,
    /// An autofix (autofixer agent) run.
    Autofix,
}

impl OperationKind {
    /// The literal stored in the `kind` column of the derived union.
    pub const fn as_str(self) -> &'static str {
        match self {
            OperationKind::Deployment => "deployment",
            OperationKind::Rollback => "rollback",
            OperationKind::Promotion => "promotion",
            OperationKind::Restore => "restore",
            OperationKind::Backup => "backup",
            OperationKind::Autofix => "autofix",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "deployment" => Some(OperationKind::Deployment),
            "rollback" => Some(OperationKind::Rollback),
            "promotion" => Some(OperationKind::Promotion),
            "restore" => Some(OperationKind::Restore),
            "backup" => Some(OperationKind::Backup),
            "autofix" => Some(OperationKind::Autofix),
            _ => None,
        }
    }

    fn source(self) -> OperationSource {
        match self {
            OperationKind::Deployment | OperationKind::Rollback | OperationKind::Promotion => {
                OperationSource::Deployments
            }
            OperationKind::Restore => OperationSource::RestoreRuns,
            OperationKind::Backup => OperationSource::Backups,
            OperationKind::Autofix => OperationSource::AutofixRuns,
        }
    }
}

/// Normalised lifecycle state of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Accepted but not started yet.
    Queued,
    /// In progress.
    Running,
    /// Paused, waiting for a person to act (e.g. review an autofix analysis).
    Waiting,
    /// Finished successfully.
    Succeeded,
    /// Finished unsuccessfully.
    Failed,
    /// Stopped before finishing.
    Cancelled,
}

impl OperationStatus {
    /// The literal stored in the `status` column of the derived union.
    pub const fn as_str(self) -> &'static str {
        match self {
            OperationStatus::Queued => "queued",
            OperationStatus::Running => "running",
            OperationStatus::Waiting => "waiting",
            OperationStatus::Succeeded => "succeeded",
            OperationStatus::Failed => "failed",
            OperationStatus::Cancelled => "cancelled",
        }
    }

    /// Whether the operation is still in flight (counts toward `running_count`).
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            OperationStatus::Queued | OperationStatus::Running | OperationStatus::Waiting
        )
    }

    fn parse(value: &str) -> Option<Self> {
        ALL_STATUSES
            .iter()
            .copied()
            .find(|status| status.as_str() == value)
    }
}

const ALL_STATUSES: [OperationStatus; 6] = [
    OperationStatus::Queued,
    OperationStatus::Running,
    OperationStatus::Waiting,
    OperationStatus::Succeeded,
    OperationStatus::Failed,
    OperationStatus::Cancelled,
];

/// Which slice of the feed to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatusFilter {
    /// Queued, running and waiting operations.
    Running,
    /// Succeeded, failed and cancelled operations.
    Finished,
    /// Everything in the window.
    #[default]
    All,
}

/// One state table: every raw state a source table is known to write, and the
/// normalised status it maps to. Anything not listed maps to the source's
/// fallback, which is always terminal so an unknown state can never inflate
/// `running_count` or escape the recency window.
struct StateMap {
    entries: &'static [(&'static str, OperationStatus)],
    fallback: OperationStatus,
}

/// `deployments.state` is free text. Active states mirror the sets the job
/// processor uses for duplicate detection; `paused` is an operator-paused
/// *healthy* generation and `superseded` a replaced one, so both are successes.
const DEPLOYMENT_STATES: StateMap = StateMap {
    entries: &[
        ("pending", OperationStatus::Queued),
        ("creating", OperationStatus::Running),
        ("running", OperationStatus::Running),
        ("in_progress", OperationStatus::Running),
        ("built", OperationStatus::Running),
        ("ready", OperationStatus::Running),
        ("deploying", OperationStatus::Running),
        ("deployed", OperationStatus::Succeeded),
        ("completed", OperationStatus::Succeeded),
        ("success", OperationStatus::Succeeded),
        ("superseded", OperationStatus::Succeeded),
        ("paused", OperationStatus::Succeeded),
        ("failed", OperationStatus::Failed),
        ("cancelled", OperationStatus::Cancelled),
        ("stopped", OperationStatus::Cancelled),
    ],
    fallback: OperationStatus::Failed,
};

/// `restore_runs.status`. `interrupted` is written by startup reconciliation
/// when the worker died with the previous process — the restore did not finish.
const RESTORE_STATES: StateMap = StateMap {
    entries: &[
        ("pending", OperationStatus::Queued),
        ("running", OperationStatus::Running),
        ("completed", OperationStatus::Succeeded),
        ("failed", OperationStatus::Failed),
        ("interrupted", OperationStatus::Failed),
        ("cancelled", OperationStatus::Cancelled),
    ],
    fallback: OperationStatus::Failed,
};

/// `backups.state`.
const BACKUP_STATES: StateMap = StateMap {
    entries: &[
        ("pending", OperationStatus::Queued),
        ("running", OperationStatus::Running),
        ("completed", OperationStatus::Succeeded),
        ("failed", OperationStatus::Failed),
        ("cancelled", OperationStatus::Cancelled),
    ],
    fallback: OperationStatus::Failed,
};

/// `agent_runs.status` for autofixer runs. `analyzed` and `fix_ready` park the
/// run until someone reviews the analysis or the fix, hence `waiting`.
/// `no_fix` means the run ended without producing a fix: reported as `failed`
/// with a "no fix found" reason, because from the operator's point of view
/// the problem was not fixed.
const AUTOFIX_STATES: StateMap = StateMap {
    entries: &[
        ("pending", OperationStatus::Queued),
        ("cloning", OperationStatus::Running),
        ("analyzing", OperationStatus::Running),
        ("fixing", OperationStatus::Running),
        ("pushing", OperationStatus::Running),
        ("creating_pr", OperationStatus::Running),
        ("deploying", OperationStatus::Running),
        ("analyzed", OperationStatus::Waiting),
        ("fix_ready", OperationStatus::Waiting),
        ("completed", OperationStatus::Succeeded),
        ("no_fix", OperationStatus::Failed),
        ("failed", OperationStatus::Failed),
        ("cancelled", OperationStatus::Cancelled),
    ],
    fallback: OperationStatus::Failed,
};

impl StateMap {
    fn map(&self, raw: &str) -> OperationStatus {
        self.entries
            .iter()
            .find(|(state, _)| *state == raw)
            .map(|(_, status)| *status)
            .unwrap_or(self.fallback)
    }

    fn is_known(&self, raw: &str) -> bool {
        self.entries.iter().any(|(state, _)| *state == raw)
    }

    /// `CASE <column> WHEN 'pending' THEN 'queued' ... ELSE '<fallback>' END`.
    /// Only module-owned constants are interpolated.
    fn case_sql(&self, column: &str) -> String {
        let mut sql = format!("CASE {column}");
        for (state, status) in self.entries {
            sql.push_str(&format!(" WHEN '{state}' THEN '{}'", status.as_str()));
        }
        sql.push_str(&format!(" ELSE '{}' END", self.fallback.as_str()));
        sql
    }

    /// `'pending', 'running', ...` — the raw states that are still active.
    fn active_states_sql(&self) -> String {
        self.entries
            .iter()
            .filter(|(_, status)| status.is_active())
            .map(|(state, _)| format!("'{state}'"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Map a raw `deployments.state` to its normalised status.
pub fn map_deployment_state(state: &str) -> OperationStatus {
    DEPLOYMENT_STATES.map(state)
}

/// Map a raw `restore_runs.status` to its normalised status.
pub fn map_restore_status(status: &str) -> OperationStatus {
    RESTORE_STATES.map(status)
}

/// Map a raw `backups.state` to its normalised status.
pub fn map_backup_state(state: &str) -> OperationStatus {
    BACKUP_STATES.map(state)
}

/// Map a raw autofixer `agent_runs.status` to its normalised status.
pub fn map_autofix_status(status: &str) -> OperationStatus {
    AUTOFIX_STATES.map(status)
}

/// Classify a deployment row. A rollback is recorded either in the typed
/// metadata (`isRollback`) or in the workflow context (`trigger = rollback`);
/// a promotion always sets `promoted_from_deployment_id`. Everything else —
/// including redeploys, which leave no distinguishing trace — is a deployment.
pub fn classify_deployment(
    metadata_is_rollback: bool,
    context_trigger: Option<&str>,
    promoted_from_deployment_id: Option<i32>,
) -> OperationKind {
    if metadata_is_rollback || context_trigger == Some("rollback") {
        OperationKind::Rollback
    } else if promoted_from_deployment_id.is_some() {
        OperationKind::Promotion
    } else {
        OperationKind::Deployment
    }
}

/// SQL mirror of [`classify_deployment`] over a `deployments` row aliased `d`.
const DEPLOYMENT_KIND_SQL: &str = "CASE \
     WHEN COALESCE(d.metadata->>'isRollback', 'false') = 'true' \
       OR d.context_vars->>'trigger' = 'rollback' THEN 'rollback' \
     WHEN d.promoted_from_deployment_id IS NOT NULL THEN 'promotion' \
     ELSE 'deployment' END";

/// Errors raised while listing operations.
#[derive(Error, Debug)]
pub enum OperationsError {
    #[error("Failed to {operation} for the operations feed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Invalid operations query: {message}")]
    Validation { message: String },
}

impl From<DbErr> for OperationsError {
    fn from(source: DbErr) -> Self {
        OperationsError::Database {
            operation: "query operations",
            source,
        }
    }
}

/// Which rows the caller may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationsScope {
    /// Instance administrators: every row, including rows with no project
    /// (control-plane backups, restores of unlinked services).
    Instance,
    /// Regular principals: every project except the hidden ones. Rows that
    /// belong to no project are excluded. A service-scoped row (restore,
    /// backup) is excluded if *any* project the service is linked to is
    /// hidden — fail closed.
    Projects { hidden_project_ids: Vec<i32> },
    /// A principal confined to one project (deployment tokens). A
    /// service-scoped row is included only when that project is the
    /// service's sole link.
    SingleProject { project_id: i32 },
}

/// Which sources the caller holds the read permission for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSourceAccess {
    /// `deployments:read` — deployments, rollbacks, promotions.
    pub deployments: bool,
    /// `backups:read` — backups and restores.
    pub backups: bool,
    /// `projects:read` — autofix runs.
    pub autofix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationSource {
    Deployments,
    RestoreRuns,
    Backups,
    AutofixRuns,
}

/// A normalised `GET /operations` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationsQuery {
    pub page: u64,
    pub page_size: u64,
    pub status: OperationStatusFilter,
    pub kind: Option<OperationKind>,
    pub project_id: Option<i32>,
}

impl OperationsQuery {
    /// Apply defaults and bounds: page defaults to 1 (minimum 1), page size
    /// defaults to [`OPERATIONS_DEFAULT_PAGE_SIZE`] and is clamped to
    /// `1..=OPERATIONS_MAX_PAGE_SIZE`.
    pub fn normalize(
        page: Option<u64>,
        page_size: Option<u64>,
        status: Option<OperationStatusFilter>,
        kind: Option<OperationKind>,
        project_id: Option<i32>,
    ) -> Self {
        Self {
            page: page.unwrap_or(1).max(1),
            page_size: page_size
                .unwrap_or(OPERATIONS_DEFAULT_PAGE_SIZE)
                .clamp(1, OPERATIONS_MAX_PAGE_SIZE),
            status: status.unwrap_or_default(),
            kind,
            project_id,
        }
    }

    fn offset(&self) -> Result<i64, OperationsError> {
        self.page
            .checked_sub(1)
            .and_then(|pages| pages.checked_mul(self.page_size))
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or_else(|| OperationsError::Validation {
                message: format!(
                    "page {} with page_size {} is out of range",
                    self.page, self.page_size
                ),
            })
    }
}

/// One operation in the feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OperationEntry {
    /// Stable unique id, `<source>:<row id>` (e.g. `deployment:123`).
    pub id: String,
    pub kind: OperationKind,
    pub status: OperationStatus,
    /// Human-readable label, e.g. "Rollback to deployment #41".
    pub title: String,
    /// Owning project. For restores and backups this is set only when the
    /// service is linked to exactly one project.
    pub project_id: Option<i32>,
    pub project_slug: Option<String>,
    pub environment_id: Option<i32>,
    pub environment_name: Option<String>,
    pub deployment_id: Option<i32>,
    /// Storage service the operation acts on (the restore's source service,
    /// or the backed-up service).
    pub service_id: Option<i32>,
    pub service_name: Option<String>,
    pub backup_id: Option<i32>,
    pub restore_run_id: Option<i32>,
    pub agent_run_id: Option<i32>,
    /// Source-specific phase, when the source records one (restores, autofix).
    pub phase: Option<String>,
    /// Why the operation failed or was cancelled, when known.
    pub failure_reason: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
    /// User who started it, when the source records one. Deployments do not.
    pub triggered_by_user_id: Option<i32>,
    /// Console path of the resource's detail page.
    pub link: String,
}

/// A page of operations plus the counts the tray needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationsPage {
    pub operations: Vec<OperationEntry>,
    pub total: u64,
    pub running_count: u64,
}

/// A ready-to-run pair of statements sharing one parameter list prefix.
#[derive(Debug, Clone)]
pub(crate) struct BuiltOperationsQuery {
    pub page_sql: String,
    pub page_values: Vec<Value>,
    pub count_sql: String,
    pub count_values: Vec<Value>,
}

/// Collects bound values and hands out `$n` placeholders.
struct Params {
    values: Vec<Value>,
}

impl Params {
    fn push(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }
}

/// Columns every union branch emits, in order. Each branch must produce the
/// same column list with compatible types.
const SELECT_COLUMNS: &str = "op_id, kind, status, raw_state, project_id, project_slug, \
     environment_id, environment_name, deployment_id, service_id, service_name, backup_id, \
     backup_uuid, s3_source_id, restore_run_id, agent_run_id, phase, mode, target_service_name, \
     failure_reason, created_at, started_at, finished_at, triggered_by_user_id, \
     related_deployment_id, branch_ref, commit_sha";

/// Build the page and count statements.
pub(crate) fn build_operations_query(
    query: &OperationsQuery,
    scope: &OperationsScope,
    access: OperationSourceAccess,
    window_start: DateTime<Utc>,
) -> Result<Option<BuiltOperationsQuery>, OperationsError> {
    let offset = query.offset()?;
    let sources = included_sources(query.kind, access);
    if sources.is_empty() {
        return Ok(None);
    }

    let mut params = Params { values: Vec::new() };
    let window = params.push(window_start);

    let hidden_list = match scope {
        OperationsScope::Projects { hidden_project_ids } if !hidden_project_ids.is_empty() => Some(
            hidden_project_ids
                .iter()
                .map(|id| params.push(*id))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    };
    let scope_project = match scope {
        OperationsScope::SingleProject { project_id } => Some(params.push(*project_id)),
        _ => None,
    };
    let filter_project = query.project_id.map(|id| params.push(id));
    let filter_kind = query.kind.map(|kind| params.push(kind.as_str()));

    let filters = ProjectFilters {
        scope,
        hidden_list: hidden_list.as_deref(),
        scope_project: scope_project.as_deref(),
        filter_project: filter_project.as_deref(),
    };

    let branches: Vec<String> = sources
        .iter()
        .map(|source| match source {
            OperationSource::Deployments => deployments_branch(&window, &filters),
            OperationSource::RestoreRuns => restore_branch(&window, &filters),
            OperationSource::Backups => backups_branch(&window, &filters),
            OperationSource::AutofixRuns => autofix_branch(&window, &filters),
        })
        .collect();

    let kind_filter = match &filter_kind {
        Some(placeholder) => format!(" WHERE u.kind = {placeholder}"),
        None => String::new(),
    };
    let cte = format!(
        "WITH ops AS (SELECT * FROM ({}) u{kind_filter})",
        branches.join(" UNION ALL ")
    );

    let status_filter = match query.status {
        OperationStatusFilter::All => "TRUE".to_string(),
        OperationStatusFilter::Running => format!("status IN ({})", statuses_sql(true)),
        OperationStatusFilter::Finished => format!("status IN ({})", statuses_sql(false)),
    };

    let count_sql = format!(
        "{cte} SELECT COUNT(*) FILTER (WHERE {status_filter}) AS total, \
         COUNT(*) FILTER (WHERE status IN ({})) AS running_count FROM ops",
        statuses_sql(true)
    );
    let count_values = params.values.clone();

    let limit =
        params.push(i64::try_from(query.page_size).unwrap_or(OPERATIONS_MAX_PAGE_SIZE as i64));
    let offset = params.push(offset);
    let page_sql = format!(
        "{cte} SELECT {SELECT_COLUMNS} FROM ops WHERE {status_filter} \
         ORDER BY created_at DESC, op_id DESC LIMIT {limit} OFFSET {offset}"
    );

    Ok(Some(BuiltOperationsQuery {
        page_sql,
        page_values: params.values,
        count_sql,
        count_values,
    }))
}

fn included_sources(
    kind: Option<OperationKind>,
    access: OperationSourceAccess,
) -> Vec<OperationSource> {
    [
        (OperationSource::Deployments, access.deployments),
        (OperationSource::RestoreRuns, access.backups),
        (OperationSource::Backups, access.backups),
        (OperationSource::AutofixRuns, access.autofix),
    ]
    .into_iter()
    .filter(|(source, allowed)| *allowed && kind.is_none_or(|kind| kind.source() == *source))
    .map(|(source, _)| source)
    .collect()
}

fn statuses_sql(active: bool) -> String {
    ALL_STATUSES
        .iter()
        .filter(|status| status.is_active() == active)
        .map(|status| format!("'{}'", status.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Placeholders for the project restrictions, shared by every branch.
struct ProjectFilters<'a> {
    scope: &'a OperationsScope,
    hidden_list: Option<&'a str>,
    scope_project: Option<&'a str>,
    filter_project: Option<&'a str>,
}

impl ProjectFilters<'_> {
    /// Conditions for a row that carries its project id directly.
    fn direct(&self, column: &str) -> Vec<String> {
        let mut conditions = Vec::new();
        if let Some(hidden) = self.hidden_list {
            conditions.push(format!("{column} NOT IN ({hidden})"));
        }
        if let Some(project) = self.scope_project {
            conditions.push(format!("{column} = {project}"));
        }
        if let Some(project) = self.filter_project {
            conditions.push(format!("{column} = {project}"));
        }
        conditions
    }

    /// Conditions for a row scoped through a storage service's project links.
    fn via_service(&self, service_column: &str) -> Vec<String> {
        let linked = |extra: &str| {
            format!(
                "EXISTS (SELECT 1 FROM project_services ps \
                 JOIN projects lp ON lp.id = ps.project_id AND lp.is_deleted = false \
                 WHERE ps.service_id = {service_column}{extra})"
            )
        };
        let mut conditions = Vec::new();
        match self.scope {
            OperationsScope::Instance => {}
            OperationsScope::Projects { .. } => {
                conditions.push(linked(""));
                if let Some(hidden) = self.hidden_list {
                    conditions.push(format!(
                        "NOT EXISTS (SELECT 1 FROM project_services ps \
                         WHERE ps.service_id = {service_column} AND ps.project_id IN ({hidden}))"
                    ));
                }
            }
            OperationsScope::SingleProject { .. } => {
                if let Some(project) = self.scope_project {
                    conditions.push(linked(&format!(" AND ps.project_id = {project}")));
                    conditions.push(format!(
                        "NOT EXISTS (SELECT 1 FROM project_services ps \
                         WHERE ps.service_id = {service_column} AND ps.project_id <> {project})"
                    ));
                }
            }
        }
        if let Some(project) = self.filter_project {
            conditions.push(linked(&format!(" AND ps.project_id = {project}")));
        }
        conditions
    }
}

fn where_clause(base: Vec<String>, extra: Vec<String>) -> String {
    let all: Vec<String> = base.into_iter().chain(extra).collect();
    format!("WHERE {}", all.join(" AND "))
}

/// `int` from a JSON text value only when it is all digits, so a malformed
/// value can never fail the whole query on a cast.
fn json_int(expr: &str) -> String {
    format!("CASE WHEN ({expr}) ~ '^[0-9]{{1,9}}$' THEN ({expr})::int END")
}

fn deployments_branch(window: &str, filters: &ProjectFilters<'_>) -> String {
    let status = DEPLOYMENT_STATES.case_sql("d.state");
    let related = format!(
        "COALESCE({}, {})",
        json_int("d.metadata->>'rolledBackFromId'"),
        json_int("d.context_vars->>'source_deployment_id'")
    );
    let failure =
        format!("CASE WHEN ({status}) IN ('failed', 'cancelled') THEN d.cancelled_reason END");
    let base = vec![format!(
        "(d.created_at >= {window} OR d.state IN ({}))",
        DEPLOYMENT_STATES.active_states_sql()
    )];
    format!(
        "SELECT 'deployment:' || d.id::text AS op_id, {DEPLOYMENT_KIND_SQL} AS kind, \
         {status} AS status, d.state AS raw_state, d.project_id AS project_id, \
         p.slug AS project_slug, d.environment_id AS environment_id, \
         e.name AS environment_name, d.id AS deployment_id, NULL::int AS service_id, \
         NULL::text AS service_name, NULL::int AS backup_id, NULL::text AS backup_uuid, \
         NULL::int AS s3_source_id, NULL::int AS restore_run_id, NULL::int AS agent_run_id, \
         NULL::text AS phase, NULL::text AS mode, NULL::text AS target_service_name, \
         {failure} AS failure_reason, d.created_at AS created_at, d.started_at AS started_at, \
         d.finished_at AS finished_at, NULL::int AS triggered_by_user_id, \
         CASE WHEN d.promoted_from_deployment_id IS NOT NULL \
           THEN d.promoted_from_deployment_id ELSE {related} END AS related_deployment_id, \
         d.branch_ref AS branch_ref, d.commit_sha AS commit_sha \
         FROM deployments d \
         JOIN projects p ON p.id = d.project_id AND p.is_deleted = false \
         LEFT JOIN environments e ON e.id = d.environment_id \
         {}",
        where_clause(base, filters.direct("d.project_id"))
    )
}

/// Project id/slug for a service-scoped row: only when the service is linked
/// to exactly one live project.
fn sole_project_join(service_column: &str) -> String {
    format!(
        "LEFT JOIN LATERAL (SELECT MIN(ps.project_id) AS project_id, COUNT(*) AS links \
         FROM project_services ps JOIN projects lp ON lp.id = ps.project_id AND lp.is_deleted = false \
         WHERE ps.service_id = {service_column}) sp ON true \
         LEFT JOIN projects p ON p.id = sp.project_id AND sp.links = 1"
    )
}

fn restore_branch(window: &str, filters: &ProjectFilters<'_>) -> String {
    let status = RESTORE_STATES.case_sql("r.status");
    let base = vec![format!(
        "(r.created_at >= {window} OR r.status IN ({}))",
        RESTORE_STATES.active_states_sql()
    )];
    format!(
        "SELECT 'restore:' || r.id::text AS op_id, 'restore' AS kind, {status} AS status, \
         r.status AS raw_state, p.id AS project_id, p.slug AS project_slug, \
         NULL::int AS environment_id, NULL::text AS environment_name, NULL::int AS deployment_id, \
         r.source_service_id AS service_id, es.name AS service_name, \
         r.source_backup_id AS backup_id, NULL::text AS backup_uuid, NULL::int AS s3_source_id, \
         r.id AS restore_run_id, NULL::int AS agent_run_id, r.phase AS phase, r.mode AS mode, \
         r.target_service_name AS target_service_name, \
         CASE WHEN ({status}) IN ('failed', 'cancelled') THEN r.error_message END AS failure_reason, \
         r.created_at AS created_at, r.started_at AS started_at, r.finished_at AS finished_at, \
         NULLIF(r.created_by, 0) AS triggered_by_user_id, NULL::int AS related_deployment_id, \
         NULL::text AS branch_ref, NULL::text AS commit_sha \
         FROM restore_runs r \
         LEFT JOIN external_services es ON es.id = r.source_service_id \
         {} {}",
        sole_project_join("r.source_service_id"),
        where_clause(base, filters.via_service("r.source_service_id"))
    )
}

fn backups_branch(window: &str, filters: &ProjectFilters<'_>) -> String {
    let status = BACKUP_STATES.case_sql("b.state");
    let base = vec![format!(
        "(b.started_at >= {window} OR b.state IN ({}))",
        BACKUP_STATES.active_states_sql()
    )];
    format!(
        "SELECT 'backup:' || b.id::text AS op_id, 'backup' AS kind, {status} AS status, \
         b.state AS raw_state, p.id AS project_id, p.slug AS project_slug, \
         NULL::int AS environment_id, NULL::text AS environment_name, NULL::int AS deployment_id, \
         esb.service_id AS service_id, COALESCE(es.name, esb.service_name_snapshot) AS service_name, \
         b.id AS backup_id, b.backup_id AS backup_uuid, b.s3_source_id AS s3_source_id, \
         NULL::int AS restore_run_id, NULL::int AS agent_run_id, NULL::text AS phase, \
         NULL::text AS mode, NULL::text AS target_service_name, \
         CASE WHEN ({status}) IN ('failed', 'cancelled') THEN b.error_message END AS failure_reason, \
         b.started_at AS created_at, b.started_at AS started_at, b.finished_at AS finished_at, \
         NULLIF(b.created_by, 0) AS triggered_by_user_id, NULL::int AS related_deployment_id, \
         NULL::text AS branch_ref, NULL::text AS commit_sha \
         FROM backups b \
         LEFT JOIN LATERAL (SELECT x.service_id, x.service_name_snapshot \
           FROM external_service_backups x WHERE x.backup_id = b.id ORDER BY x.id LIMIT 1) esb ON true \
         LEFT JOIN external_services es ON es.id = esb.service_id \
         {} {}",
        sole_project_join("esb.service_id"),
        where_clause(base, filters.via_service("esb.service_id"))
    )
}

fn autofix_branch(window: &str, filters: &ProjectFilters<'_>) -> String {
    let status = AUTOFIX_STATES.case_sql("ar.status");
    let base = vec![
        "ar.trigger_type = 'autofixer'".to_string(),
        format!(
            "(ar.created_at >= {window} OR ar.status IN ({}))",
            AUTOFIX_STATES.active_states_sql()
        ),
    ];
    format!(
        "SELECT 'autofix:' || ar.id::text AS op_id, 'autofix' AS kind, {status} AS status, \
         ar.status AS raw_state, ar.project_id AS project_id, p.slug AS project_slug, \
         NULL::int AS environment_id, NULL::text AS environment_name, \
         ar.preview_deployment_id AS deployment_id, NULL::int AS service_id, \
         NULL::text AS service_name, NULL::int AS backup_id, NULL::text AS backup_uuid, \
         NULL::int AS s3_source_id, NULL::int AS restore_run_id, ar.id AS agent_run_id, \
         ar.phase AS phase, NULL::text AS mode, NULL::text AS target_service_name, \
         CASE WHEN ({status}) IN ('failed', 'cancelled') THEN ar.error_message END AS failure_reason, \
         ar.created_at AS created_at, ar.started_at AS started_at, ar.completed_at AS finished_at, \
         ar.triggered_by_user_id AS triggered_by_user_id, NULL::int AS related_deployment_id, \
         NULL::text AS branch_ref, NULL::text AS commit_sha \
         FROM agent_runs ar \
         JOIN projects p ON p.id = ar.project_id AND p.is_deleted = false \
         {}",
        where_clause(base, filters.direct("ar.project_id"))
    )
}

/// Raw row of the derived union.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct OperationRow {
    pub op_id: String,
    pub kind: String,
    pub status: String,
    pub raw_state: String,
    pub project_id: Option<i32>,
    pub project_slug: Option<String>,
    pub environment_id: Option<i32>,
    pub environment_name: Option<String>,
    pub deployment_id: Option<i32>,
    pub service_id: Option<i32>,
    pub service_name: Option<String>,
    pub backup_id: Option<i32>,
    pub backup_uuid: Option<String>,
    pub s3_source_id: Option<i32>,
    pub restore_run_id: Option<i32>,
    pub agent_run_id: Option<i32>,
    pub phase: Option<String>,
    pub mode: Option<String>,
    pub target_service_name: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub triggered_by_user_id: Option<i32>,
    pub related_deployment_id: Option<i32>,
    pub branch_ref: Option<String>,
    pub commit_sha: Option<String>,
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    total: i64,
    running_count: i64,
}

impl OperationRow {
    /// Convert a union row into an API entry. Returns a validation error for a
    /// row whose kind/status literal this module never emits — that can only
    /// mean the SQL and the enums drifted apart.
    pub(crate) fn into_entry(self) -> Result<OperationEntry, OperationsError> {
        let kind = OperationKind::parse(&self.kind).ok_or_else(|| OperationsError::Validation {
            message: format!("operation {} has unknown kind '{}'", self.op_id, self.kind),
        })?;
        let status =
            OperationStatus::parse(&self.status).ok_or_else(|| OperationsError::Validation {
                message: format!(
                    "operation {} has unknown status '{}'",
                    self.op_id, self.status
                ),
            })?;
        let title = operation_title(kind, &self);
        let link = operation_link(kind, &self);
        let failure_reason = failure_reason(kind, status, &self);
        Ok(OperationEntry {
            id: self.op_id,
            kind,
            status,
            title,
            project_id: self.project_id,
            project_slug: self.project_slug,
            environment_id: self.environment_id,
            environment_name: self.environment_name,
            deployment_id: self.deployment_id,
            service_id: self.service_id,
            service_name: self.service_name,
            backup_id: self.backup_id,
            restore_run_id: self.restore_run_id,
            agent_run_id: self.agent_run_id,
            phase: self.phase,
            failure_reason,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
            triggered_by_user_id: self.triggered_by_user_id,
            link,
        })
    }
}

fn state_map_for(kind: OperationKind) -> &'static StateMap {
    match kind.source() {
        OperationSource::Deployments => &DEPLOYMENT_STATES,
        OperationSource::RestoreRuns => &RESTORE_STATES,
        OperationSource::Backups => &BACKUP_STATES,
        OperationSource::AutofixRuns => &AUTOFIX_STATES,
    }
}

fn failure_reason(
    kind: OperationKind,
    status: OperationStatus,
    row: &OperationRow,
) -> Option<String> {
    if let Some(reason) = row
        .failure_reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        return Some(reason.to_string());
    }
    if !matches!(status, OperationStatus::Failed | OperationStatus::Cancelled) {
        return None;
    }
    if kind == OperationKind::Autofix && row.raw_state == "no_fix" {
        return Some("No fix found".to_string());
    }
    if kind == OperationKind::Restore && row.raw_state == "interrupted" {
        return Some("Interrupted by a server restart".to_string());
    }
    if !state_map_for(kind).is_known(&row.raw_state) {
        return Some(format!("Unrecognized state '{}'", row.raw_state));
    }
    None
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn operation_title(kind: OperationKind, row: &OperationRow) -> String {
    let service = row.service_name.as_deref().unwrap_or("service");
    match kind {
        OperationKind::Rollback => match row.related_deployment_id {
            Some(id) => format!("Rollback to deployment #{id}"),
            None => "Rollback".to_string(),
        },
        OperationKind::Promotion => match row.related_deployment_id {
            Some(id) => format!("Promote deployment #{id}"),
            None => "Promotion".to_string(),
        },
        OperationKind::Deployment => {
            let sha = row.commit_sha.as_deref().map(short_sha);
            match (row.branch_ref.as_deref(), sha) {
                (Some(branch), Some(sha)) => format!("Deploy {branch} @ {sha}"),
                (Some(branch), None) => format!("Deploy {branch}"),
                (None, Some(sha)) => format!("Deploy {sha}"),
                (None, None) => match row.deployment_id {
                    Some(id) => format!("Deployment #{id}"),
                    None => "Deployment".to_string(),
                },
            }
        }
        OperationKind::Restore => match row.mode.as_deref() {
            Some("in_place") => format!("Restore {service} in place"),
            Some("pitr") => match row.target_service_name.as_deref() {
                Some(target) => format!("Point-in-time restore of {service} to {target}"),
                None => format!("Point-in-time restore of {service}"),
            },
            _ => match row.target_service_name.as_deref() {
                Some(target) => format!("Restore {service} to {target}"),
                None => format!("Restore {service}"),
            },
        },
        OperationKind::Backup => match row.service_name.as_deref() {
            Some(name) => format!("Backup of {name}"),
            None => "Control-plane backup".to_string(),
        },
        OperationKind::Autofix => match row.agent_run_id {
            Some(id) => format!("Autofix run #{id}"),
            None => "Autofix run".to_string(),
        },
    }
}

fn operation_link(kind: OperationKind, row: &OperationRow) -> String {
    match kind {
        OperationKind::Deployment | OperationKind::Rollback | OperationKind::Promotion => {
            match (row.project_slug.as_deref(), row.deployment_id) {
                (Some(slug), Some(id)) => format!("/projects/{slug}/deployments/{id}"),
                (Some(slug), None) => format!("/projects/{slug}/deployments"),
                _ => "/projects".to_string(),
            }
        }
        OperationKind::Restore => match (row.service_id, row.restore_run_id) {
            (Some(service), Some(run)) => format!("/storage/{service}/restore?run={run}"),
            (Some(service), None) => format!("/storage/{service}"),
            _ => "/storage".to_string(),
        },
        OperationKind::Backup => match (row.s3_source_id, row.backup_uuid.as_deref()) {
            (Some(source), Some(uuid)) => format!("/backups/s3-sources/{source}/backups/{uuid}"),
            (Some(source), None) => format!("/backups/s3-sources/{source}"),
            _ => "/backups".to_string(),
        },
        OperationKind::Autofix => match (row.project_slug.as_deref(), row.agent_run_id) {
            (Some(slug), Some(run)) => format!("/projects/{slug}/agents/{run}"),
            (Some(slug), None) => format!("/projects/{slug}/autofixer"),
            _ => "/projects".to_string(),
        },
    }
}

/// Lists operations derived from existing tables. See the module docs.
pub struct OperationsService {
    db: Arc<DatabaseConnection>,
}

impl OperationsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// One page of operations visible in `scope`, newest first.
    pub async fn list_operations(
        &self,
        query: &OperationsQuery,
        scope: &OperationsScope,
        access: OperationSourceAccess,
    ) -> Result<OperationsPage, OperationsError> {
        let window_start = Utc::now() - Duration::days(OPERATIONS_RECENCY_WINDOW_DAYS);
        let Some(built) = build_operations_query(query, scope, access, window_start)? else {
            return Ok(OperationsPage {
                operations: Vec::new(),
                total: 0,
                running_count: 0,
            });
        };

        let counts = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            built.count_sql,
            built.count_values,
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|source| {
            error!(error = %source, "Failed to count operations for the operations feed");
            OperationsError::Database {
                operation: "count operations",
                source,
            }
        })?;
        let (total, running_count) = counts
            .map(|row| (row.total.max(0) as u64, row.running_count.max(0) as u64))
            .unwrap_or((0, 0));

        let rows = OperationRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            built.page_sql,
            built.page_values,
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|source| {
            error!(
                page = query.page,
                page_size = query.page_size,
                error = %source,
                "Failed to load a page of the operations feed"
            );
            OperationsError::Database {
                operation: "load a page of operations",
                source,
            }
        })?;

        let operations = rows
            .into_iter()
            .map(OperationRow::into_entry)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(OperationsPage {
            operations,
            total,
            running_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_access() -> OperationSourceAccess {
        OperationSourceAccess {
            deployments: true,
            backups: true,
            autofix: true,
        }
    }

    fn default_query() -> OperationsQuery {
        OperationsQuery::normalize(None, None, None, None, None)
    }

    fn window() -> DateTime<Utc> {
        Utc::now() - Duration::days(OPERATIONS_RECENCY_WINDOW_DAYS)
    }

    fn built(
        query: &OperationsQuery,
        scope: &OperationsScope,
        access: OperationSourceAccess,
    ) -> BuiltOperationsQuery {
        build_operations_query(query, scope, access, window())
            .expect("query builds")
            .expect("at least one source is included")
    }

    fn row(kind: &str, status: &str, raw_state: &str) -> OperationRow {
        OperationRow {
            op_id: format!("{kind}:1"),
            kind: kind.to_string(),
            status: status.to_string(),
            raw_state: raw_state.to_string(),
            project_id: Some(3),
            project_slug: Some("shop".to_string()),
            environment_id: Some(5),
            environment_name: Some("production".to_string()),
            deployment_id: None,
            service_id: None,
            service_name: None,
            backup_id: None,
            backup_uuid: None,
            s3_source_id: None,
            restore_run_id: None,
            agent_run_id: None,
            phase: None,
            mode: None,
            target_service_name: None,
            failure_reason: None,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            triggered_by_user_id: None,
            related_deployment_id: None,
            branch_ref: None,
            commit_sha: None,
        }
    }

    // ---- status / kind mapping ------------------------------------------

    #[test]
    fn deployment_states_map_to_normalised_statuses() {
        assert_eq!(map_deployment_state("pending"), OperationStatus::Queued);
        for state in [
            "creating",
            "running",
            "in_progress",
            "built",
            "ready",
            "deploying",
        ] {
            assert_eq!(
                map_deployment_state(state),
                OperationStatus::Running,
                "{state}"
            );
        }
        for state in ["deployed", "completed", "success", "superseded", "paused"] {
            assert_eq!(
                map_deployment_state(state),
                OperationStatus::Succeeded,
                "{state}"
            );
        }
        assert_eq!(map_deployment_state("failed"), OperationStatus::Failed);
        assert_eq!(
            map_deployment_state("cancelled"),
            OperationStatus::Cancelled
        );
        assert_eq!(map_deployment_state("stopped"), OperationStatus::Cancelled);
    }

    #[test]
    fn restore_states_map_to_normalised_statuses() {
        assert_eq!(map_restore_status("pending"), OperationStatus::Queued);
        assert_eq!(map_restore_status("running"), OperationStatus::Running);
        assert_eq!(map_restore_status("completed"), OperationStatus::Succeeded);
        assert_eq!(map_restore_status("failed"), OperationStatus::Failed);
        assert_eq!(map_restore_status("interrupted"), OperationStatus::Failed);
        assert_eq!(map_restore_status("cancelled"), OperationStatus::Cancelled);
    }

    #[test]
    fn backup_states_map_to_normalised_statuses() {
        assert_eq!(map_backup_state("pending"), OperationStatus::Queued);
        assert_eq!(map_backup_state("running"), OperationStatus::Running);
        assert_eq!(map_backup_state("completed"), OperationStatus::Succeeded);
        assert_eq!(map_backup_state("failed"), OperationStatus::Failed);
    }

    #[test]
    fn autofix_states_map_to_normalised_statuses() {
        assert_eq!(map_autofix_status("pending"), OperationStatus::Queued);
        for state in [
            "cloning",
            "analyzing",
            "fixing",
            "pushing",
            "creating_pr",
            "deploying",
        ] {
            assert_eq!(
                map_autofix_status(state),
                OperationStatus::Running,
                "{state}"
            );
        }
        assert_eq!(map_autofix_status("analyzed"), OperationStatus::Waiting);
        assert_eq!(map_autofix_status("fix_ready"), OperationStatus::Waiting);
        assert_eq!(map_autofix_status("completed"), OperationStatus::Succeeded);
        assert_eq!(map_autofix_status("no_fix"), OperationStatus::Failed);
        assert_eq!(map_autofix_status("failed"), OperationStatus::Failed);
        assert_eq!(map_autofix_status("cancelled"), OperationStatus::Cancelled);
    }

    #[test]
    fn unknown_states_fall_back_to_a_terminal_status() {
        // A terminal fallback keeps an unrecognised state from inflating the
        // running count or escaping the recency window.
        for map in [
            &DEPLOYMENT_STATES,
            &RESTORE_STATES,
            &BACKUP_STATES,
            &AUTOFIX_STATES,
        ] {
            assert!(!map.map("something-new").is_active());
        }
    }

    #[test]
    fn deployment_kind_classification() {
        assert_eq!(
            classify_deployment(true, None, None),
            OperationKind::Rollback
        );
        assert_eq!(
            classify_deployment(false, Some("rollback"), None),
            OperationKind::Rollback
        );
        assert_eq!(
            classify_deployment(false, Some("promotion"), Some(12)),
            OperationKind::Promotion
        );
        // Rollback wins over promotion: a rolled-back promotion is a rollback.
        assert_eq!(
            classify_deployment(true, None, Some(12)),
            OperationKind::Rollback
        );
        assert_eq!(
            classify_deployment(false, Some("user"), None),
            OperationKind::Deployment
        );
        assert_eq!(
            classify_deployment(false, None, None),
            OperationKind::Deployment
        );
    }

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&OperationKind::Autofix)
                .ok()
                .as_deref(),
            Some("\"autofix\"")
        );
        assert_eq!(
            serde_json::to_string(&OperationStatus::Succeeded)
                .ok()
                .as_deref(),
            Some("\"succeeded\"")
        );
        for status in ALL_STATUSES {
            assert_eq!(OperationStatus::parse(status.as_str()), Some(status));
        }
    }

    // ---- query normalisation --------------------------------------------

    #[test]
    fn normalize_applies_defaults_and_bounds() {
        let query = default_query();
        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, 20);
        assert_eq!(query.status, OperationStatusFilter::All);

        let clamped = OperationsQuery::normalize(Some(0), Some(500), None, None, None);
        assert_eq!(clamped.page, 1);
        assert_eq!(clamped.page_size, 100);

        let minimum = OperationsQuery::normalize(Some(3), Some(0), None, None, None);
        assert_eq!(minimum.page, 3);
        assert_eq!(minimum.page_size, 1);
    }

    #[test]
    fn offset_overflow_is_a_validation_error() {
        let query = OperationsQuery::normalize(Some(u64::MAX), Some(100), None, None, None);
        let result =
            build_operations_query(&query, &OperationsScope::Instance, all_access(), window());
        assert!(matches!(result, Err(OperationsError::Validation { .. })));
    }

    // ---- SQL builder ------------------------------------------------------

    #[test]
    fn page_query_binds_window_limit_and_offset() {
        let query = OperationsQuery::normalize(Some(3), Some(25), None, None, None);
        let built = built(&query, &OperationsScope::Instance, all_access());

        // $1 window, $2 limit, $3 offset — nothing else to bind for an admin.
        assert_eq!(built.page_values.len(), 3);
        assert_eq!(built.count_values.len(), 1);
        assert_eq!(built.page_values[1], Value::from(25_i64));
        assert_eq!(built.page_values[2], Value::from(50_i64));
        assert!(built.page_sql.contains("LIMIT $2 OFFSET $3"));
        assert!(built
            .page_sql
            .contains("ORDER BY created_at DESC, op_id DESC"));
        for table in [
            "FROM deployments d",
            "FROM restore_runs r",
            "FROM backups b",
            "FROM agent_runs ar",
        ] {
            assert!(built.page_sql.contains(table), "{table}");
        }
        assert_eq!(built.page_sql.matches("UNION ALL").count(), 3);
        assert!(built.count_sql.contains("running_count"));
        assert!(!built.count_sql.contains("OFFSET"));
    }

    #[test]
    fn hidden_projects_and_filters_are_bound_not_interpolated() {
        let query = OperationsQuery::normalize(
            None,
            None,
            None,
            Some(OperationKind::Rollback),
            Some(424_242),
        );
        let scope = OperationsScope::Projects {
            hidden_project_ids: vec![918_273, 645_546],
        };
        let built = built(&query, &scope, all_access());

        assert!(!built.page_sql.contains("u.kind = 'rollback'"));
        for literal in ["918273", "645546", "424242"] {
            assert!(!built.page_sql.contains(literal), "{literal} must be bound");
            assert!(
                !built.count_sql.contains(literal),
                "{literal} must be bound"
            );
        }
        // window, two hidden ids, project filter, kind filter (+ limit/offset)
        assert_eq!(built.count_values.len(), 5);
        assert_eq!(built.page_values.len(), 7);
        assert_eq!(built.count_values[1], Value::from(918_273_i32));
        assert_eq!(built.count_values[2], Value::from(645_546_i32));
        assert_eq!(built.count_values[3], Value::from(424_242_i32));
        assert_eq!(built.count_values[4], Value::from("rollback"));
        assert!(built.page_sql.contains("d.project_id NOT IN ($2, $3)"));
        assert!(built.page_sql.contains("d.project_id = $4"));
        assert!(built.page_sql.contains("u.kind = $5"));
        // A rollback lives in the deployments table only.
        assert!(!built.page_sql.contains("FROM restore_runs"));
        assert!(!built.page_sql.contains("FROM agent_runs"));
    }

    #[test]
    fn service_scoped_rows_fail_closed_for_regular_principals() {
        let scope = OperationsScope::Projects {
            hidden_project_ids: vec![8],
        };
        let built = built(&default_query(), &scope, all_access());
        // Must be linked to a live project, and to no hidden one.
        assert!(built.page_sql.contains(
            "EXISTS (SELECT 1 FROM project_services ps JOIN projects lp ON lp.id = ps.project_id AND lp.is_deleted = false WHERE ps.service_id = r.source_service_id)"
        ));
        assert!(built.page_sql.contains(
            "NOT EXISTS (SELECT 1 FROM project_services ps WHERE ps.service_id = esb.service_id AND ps.project_id IN ($2))"
        ));

        // Instance admins see projectless rows: no link requirement at all.
        let admin = built_sql_for(&OperationsScope::Instance);
        assert!(!admin.contains("EXISTS (SELECT 1 FROM project_services"));
    }

    fn built_sql_for(scope: &OperationsScope) -> String {
        built(&default_query(), scope, all_access()).page_sql
    }

    #[test]
    fn single_project_scope_binds_the_project() {
        let built = built(
            &default_query(),
            &OperationsScope::SingleProject { project_id: 77 },
            all_access(),
        );
        assert_eq!(built.count_values[1], Value::from(77_i32));
        assert!(built.page_sql.contains("d.project_id = $2"));
        assert!(built.page_sql.contains("ps.project_id <> $2"));
    }

    #[test]
    fn status_filter_selects_active_or_terminal_statuses() {
        let running = OperationsQuery::normalize(
            None,
            None,
            Some(OperationStatusFilter::Running),
            None,
            None,
        );
        let sql = built(&running, &OperationsScope::Instance, all_access()).page_sql;
        assert!(sql.contains("WHERE status IN ('queued', 'running', 'waiting')"));

        let finished = OperationsQuery::normalize(
            None,
            None,
            Some(OperationStatusFilter::Finished),
            None,
            None,
        );
        let sql = built(&finished, &OperationsScope::Instance, all_access()).page_sql;
        assert!(sql.contains("WHERE status IN ('succeeded', 'failed', 'cancelled')"));
    }

    #[test]
    fn sources_without_permission_are_left_out() {
        let access = OperationSourceAccess {
            deployments: true,
            backups: false,
            autofix: false,
        };
        let sql = built(&default_query(), &OperationsScope::Instance, access).page_sql;
        assert!(sql.contains("FROM deployments d"));
        assert!(!sql.contains("FROM backups b"));
        assert!(!sql.contains("FROM restore_runs r"));
        assert!(!sql.contains("FROM agent_runs ar"));

        // Asking for a kind the caller can't read yields no query at all.
        let restore_only =
            OperationsQuery::normalize(None, None, None, Some(OperationKind::Restore), None);
        assert!(build_operations_query(
            &restore_only,
            &OperationsScope::Instance,
            access,
            window()
        )
        .expect("builds")
        .is_none());
    }

    #[test]
    fn recency_window_keeps_active_rows() {
        let sql = built_sql_for(&OperationsScope::Instance);
        assert!(sql.contains(
            "(d.created_at >= $1 OR d.state IN ('pending', 'creating', 'running', 'in_progress', 'built', 'ready', 'deploying'))"
        ));
        assert!(sql.contains("(b.started_at >= $1 OR b.state IN ('pending', 'running'))"));
        assert!(sql.contains("ar.trigger_type = 'autofixer'"));
    }

    // ---- row conversion ---------------------------------------------------

    #[test]
    fn rollback_row_gets_title_and_deployment_link() {
        let mut rollback = row("rollback", "running", "running");
        rollback.op_id = "deployment:42".to_string();
        rollback.deployment_id = Some(42);
        rollback.related_deployment_id = Some(41);
        let entry = rollback.into_entry().expect("valid row");
        assert_eq!(entry.id, "deployment:42");
        assert_eq!(entry.kind, OperationKind::Rollback);
        assert_eq!(entry.title, "Rollback to deployment #41");
        assert_eq!(entry.link, "/projects/shop/deployments/42");
        assert_eq!(entry.failure_reason, None);
    }

    #[test]
    fn deployment_title_uses_branch_and_short_sha() {
        let mut deployment = row("deployment", "succeeded", "deployed");
        deployment.deployment_id = Some(9);
        deployment.branch_ref = Some("main".to_string());
        deployment.commit_sha = Some("0123456789abcdef".to_string());
        let entry = deployment.into_entry().expect("valid row");
        assert_eq!(entry.title, "Deploy main @ 0123456");
    }

    #[test]
    fn restore_row_links_to_the_service_restore_page() {
        let mut restore = row("restore", "failed", "interrupted");
        restore.service_id = Some(11);
        restore.service_name = Some("orders-db".to_string());
        restore.restore_run_id = Some(5);
        restore.mode = Some("in_place".to_string());
        let entry = restore.into_entry().expect("valid row");
        assert_eq!(entry.title, "Restore orders-db in place");
        assert_eq!(entry.link, "/storage/11/restore?run=5");
        assert_eq!(
            entry.failure_reason.as_deref(),
            Some("Interrupted by a server restart")
        );
    }

    #[test]
    fn backup_row_links_to_the_backup_detail_page() {
        let mut backup = row("backup", "failed", "failed");
        backup.backup_id = Some(3);
        backup.backup_uuid = Some("b-uuid".to_string());
        backup.s3_source_id = Some(2);
        backup.failure_reason = Some("  bucket unreachable ".to_string());
        let entry = backup.into_entry().expect("valid row");
        assert_eq!(entry.title, "Control-plane backup");
        assert_eq!(entry.link, "/backups/s3-sources/2/backups/b-uuid");
        assert_eq!(entry.failure_reason.as_deref(), Some("bucket unreachable"));
    }

    #[test]
    fn autofix_no_fix_reports_a_reason() {
        let mut autofix = row("autofix", "failed", "no_fix");
        autofix.agent_run_id = Some(17);
        let entry = autofix.into_entry().expect("valid row");
        assert_eq!(entry.title, "Autofix run #17");
        assert_eq!(entry.link, "/projects/shop/agents/17");
        assert_eq!(entry.failure_reason.as_deref(), Some("No fix found"));
    }

    #[test]
    fn unrecognized_state_is_explained() {
        let entry = row("deployment", "failed", "mystery")
            .into_entry()
            .expect("valid row");
        assert_eq!(
            entry.failure_reason.as_deref(),
            Some("Unrecognized state 'mystery'")
        );
    }

    #[test]
    fn unknown_kind_literal_is_rejected() {
        let result = row("teleport", "running", "running").into_entry();
        assert!(matches!(result, Err(OperationsError::Validation { .. })));
    }

    // ---- integration (Docker) --------------------------------------------

    async fn test_database() -> Option<temps_database::test_utils::TestDatabase> {
        match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) || error
                    .to_string()
                    .contains("failed to initialize a docker client") =>
            {
                println!("Docker not available, skipping");
                None
            }
            Err(error) => panic!("failed to create migrated test database: {error}"),
        }
    }

    async fn exec(db: &DatabaseConnection, sql: &str, values: Vec<Value>) -> i32 {
        #[derive(FromQueryResult)]
        struct Id {
            id: i32,
        }
        Id::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await
        .unwrap_or_else(|e| panic!("seed statement failed: {e}\n{sql}"))
        .map(|row| row.id)
        .unwrap_or_else(|| panic!("seed statement returned no id: {sql}"))
    }

    #[tokio::test]
    async fn lists_seeded_operations_with_scoping_and_counts() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{environments, projects, upstream_config::UpstreamList};

        let Some(test_db) = test_database().await else {
            return;
        };
        let db = test_db.db.clone();

        let mut project_ids = Vec::new();
        let mut env_ids = Vec::new();
        for slug in ["visible-app", "hidden-app"] {
            let project = projects::ActiveModel {
                name: Set(slug.to_string()),
                slug: Set(slug.to_string()),
                repo_name: Set("repo".to_string()),
                repo_owner: Set("owner".to_string()),
                directory: Set("/".to_string()),
                main_branch: Set("main".to_string()),
                preset: Set(temps_presets::PresetType::Nixpacks),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert project");
            let environment = environments::ActiveModel {
                project_id: Set(project.id),
                name: Set("production".to_string()),
                slug: Set("production".to_string()),
                subdomain: Set(slug.to_string()),
                host: Set(format!("{slug}.example.test")),
                upstreams: Set(UpstreamList::default()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("insert environment");
            project_ids.push(project.id);
            env_ids.push(environment.id);
        }
        let (visible, hidden) = (project_ids[0], project_ids[1]);
        let now = Utc::now();
        let minutes_ago = |m: i64| now - Duration::minutes(m);

        // Deployments: a finished deployment, a running rollback, one in the
        // hidden project, and one finished long ago (outside the window).
        let deploy_sql = "INSERT INTO deployments (project_id, environment_id, slug, state, \
             metadata, context_vars, cancelled_reason, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) RETURNING id";
        let deployed = exec(
            &db,
            deploy_sql,
            vec![
                visible.into(),
                env_ids[0].into(),
                "visible-app-1".into(),
                "deployed".into(),
                Value::from(serde_json::json!({})),
                Value::from(serde_json::json!({"trigger": "user"})),
                Option::<String>::None.into(),
                minutes_ago(50).into(),
            ],
        )
        .await;
        let rollback = exec(
            &db,
            deploy_sql,
            vec![
                visible.into(),
                env_ids[0].into(),
                "visible-app-2".into(),
                "running".into(),
                Value::from(serde_json::json!({"isRollback": true, "rolledBackFromId": deployed})),
                Value::from(serde_json::json!({"trigger": "rollback"})),
                Option::<String>::None.into(),
                minutes_ago(10).into(),
            ],
        )
        .await;
        exec(
            &db,
            deploy_sql,
            vec![
                hidden.into(),
                env_ids[1].into(),
                "hidden-app-1".into(),
                "running".into(),
                Value::from(serde_json::json!({})),
                Value::from(serde_json::json!({})),
                Option::<String>::None.into(),
                minutes_ago(5).into(),
            ],
        )
        .await;
        exec(
            &db,
            deploy_sql,
            vec![
                visible.into(),
                env_ids[0].into(),
                "visible-app-0".into(),
                "failed".into(),
                Value::from(serde_json::json!({})),
                Value::from(serde_json::json!({})),
                Some("build failed".to_string()).into(),
                (now - Duration::days(OPERATIONS_RECENCY_WINDOW_DAYS + 3)).into(),
            ],
        )
        .await;

        let operator = exec(
            &db,
            "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
             created_at, updated_at) VALUES ('Operator', 'operator@example.test', true, false, \
             false, NOW(), NOW()) RETURNING id",
            vec![],
        )
        .await;

        // Storage service linked to the visible project, with a failed backup
        // and a running restore.
        let service = exec(
            &db,
            "INSERT INTO external_services (name, service_type, status, created_at, updated_at) \
             VALUES ('orders-db', 'postgres', 'running', NOW(), NOW()) RETURNING id",
            vec![],
        )
        .await;
        exec(
            &db,
            "INSERT INTO project_services (project_id, service_id, created_at, updated_at) \
             VALUES ($1, $2, NOW(), NOW()) RETURNING id",
            vec![visible.into(), service.into()],
        )
        .await;
        let s3_source = exec(
            &db,
            "INSERT INTO s3_sources (name, bucket_name, bucket_path, region, access_key_id, secret_key, \
             created_at, updated_at) VALUES ('primary', 'bucket', '/', 'us-east-1', 'k', 's', NOW(), NOW()) \
             RETURNING id",
            vec![],
        )
        .await;
        let backup_sql = "INSERT INTO backups (name, backup_id, backup_type, state, started_at, \
             s3_source_id, s3_location, error_message, metadata, compression_type, created_by, tags) \
             VALUES ($1, $2, 'full', $3, $4, $5, 's3://bucket/x', $6, '{}', 'gzip', $7, '[]') RETURNING id";
        let service_backup = exec(
            &db,
            backup_sql,
            vec![
                "orders".into(),
                "uuid-service".into(),
                "failed".into(),
                minutes_ago(40).into(),
                s3_source.into(),
                Some("bucket unreachable".to_string()).into(),
                operator.into(),
            ],
        )
        .await;
        exec(
            &db,
            "INSERT INTO external_service_backups (service_id, backup_id, backup_type, state, \
             started_at, s3_location, metadata, compression_type, created_by) \
             VALUES ($1, $2, 'full', 'failed', NOW(), 's3://bucket/x', '{}', 'gzip', $3) RETURNING id",
            vec![service.into(), service_backup.into(), operator.into()],
        )
        .await;
        // Control-plane backup: no service link, admin-only.
        exec(
            &db,
            backup_sql,
            vec![
                "control-plane".into(),
                "uuid-control".into(),
                "running".into(),
                minutes_ago(30).into(),
                s3_source.into(),
                Option::<String>::None.into(),
                operator.into(),
            ],
        )
        .await;
        let restore = exec(
            &db,
            "INSERT INTO restore_runs (source_backup_id, source_service_id, mode, status, phase, \
             parameter_overrides, log_id, attempt, created_by, created_at, updated_at) \
             VALUES ($1, $2, 'in_place', 'running', 'restore', '{}', 'restore-log', 1, $4, $3, $3) \
             RETURNING id",
            vec![
                service_backup.into(),
                service.into(),
                minutes_ago(20).into(),
                operator.into(),
            ],
        )
        .await;
        let autofix = exec(
            &db,
            "INSERT INTO agent_runs (project_id, trigger_type, status, created_at) \
             VALUES ($1, 'autofixer', 'fix_ready', $2) RETURNING id",
            vec![visible.into(), minutes_ago(1).into()],
        )
        .await;

        let service_under_test = OperationsService::new(db.clone());

        // A regular principal with the hidden project excluded.
        let scope = OperationsScope::Projects {
            hidden_project_ids: vec![hidden],
        };
        let page = service_under_test
            .list_operations(&default_query(), &scope, all_access())
            .await
            .expect("list operations");
        let ids: Vec<&str> = page.operations.iter().map(|op| op.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                format!("autofix:{autofix}"),
                format!("deployment:{rollback}"),
                format!("restore:{restore}"),
                format!("backup:{service_backup}"),
                format!("deployment:{deployed}"),
            ],
            "newest first; hidden project, control-plane backup and stale rows excluded"
        );
        assert_eq!(page.total, 5);
        // rollback (running), restore (running), autofix (waiting)
        assert_eq!(page.running_count, 3);

        let rollback_entry = &page.operations[1];
        assert_eq!(rollback_entry.kind, OperationKind::Rollback);
        assert_eq!(
            rollback_entry.title,
            format!("Rollback to deployment #{deployed}")
        );
        assert_eq!(
            rollback_entry.environment_name.as_deref(),
            Some("production")
        );
        let restore_entry = &page.operations[2];
        assert_eq!(restore_entry.project_id, Some(visible));
        assert_eq!(restore_entry.service_name.as_deref(), Some("orders-db"));
        assert_eq!(
            restore_entry.link,
            format!("/storage/{service}/restore?run={restore}")
        );
        let backup_entry = &page.operations[3];
        assert_eq!(
            backup_entry.failure_reason.as_deref(),
            Some("bucket unreachable")
        );
        assert_eq!(
            backup_entry.link,
            format!("/backups/s3-sources/{s3_source}/backups/uuid-service")
        );
        assert_eq!(page.operations[0].status, OperationStatus::Waiting);

        // Pagination and the running filter.
        let second_page = service_under_test
            .list_operations(
                &OperationsQuery::normalize(Some(2), Some(2), None, None, None),
                &scope,
                all_access(),
            )
            .await
            .expect("second page");
        assert_eq!(second_page.operations.len(), 2);
        assert_eq!(second_page.operations[0].id, format!("restore:{restore}"));
        let running = service_under_test
            .list_operations(
                &OperationsQuery::normalize(
                    None,
                    None,
                    Some(OperationStatusFilter::Running),
                    None,
                    None,
                ),
                &scope,
                all_access(),
            )
            .await
            .expect("running filter");
        assert_eq!(running.total, 3);
        assert_eq!(running.running_count, 3);

        // An instance administrator also sees the hidden project and the
        // control-plane backup.
        let admin = service_under_test
            .list_operations(&default_query(), &OperationsScope::Instance, all_access())
            .await
            .expect("admin list");
        assert_eq!(admin.total, 7);
        assert_eq!(admin.running_count, 5);

        // Hiding the project the service is linked to hides its rows too.
        let hide_visible = OperationsScope::Projects {
            hidden_project_ids: vec![visible],
        };
        let other = service_under_test
            .list_operations(&default_query(), &hide_visible, all_access())
            .await
            .expect("other list");
        assert_eq!(other.total, 1);
        assert_eq!(other.operations[0].project_id, Some(hidden));
    }
}
