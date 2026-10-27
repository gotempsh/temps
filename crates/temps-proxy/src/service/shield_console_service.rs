// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the console's Shield tab shows and changes (ADR 066, D7, D11, I10):
//! an environment's mode and counts, its recent events, an address lookup
//! against the stored snapshot, bypass tokens and operator entries.
//!
//! Entries and tokens are only written here. The snapshot compile picks the
//! entries up; the route table carries the tokens to every ingress, so a
//! token change asks it to reload.

use std::net::IpAddr;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temps_core::UtcDateTime;
use temps_entities::{
    environments, projects, shield_bypass_tokens, shield_observed, shield_operator_entries,
    shield_snapshots,
};
use temps_shield::{Index, ListVerdict, Network, ShieldMode, ShieldSnapshot};
use tracing::{error, info};
use utoipa::{IntoParams, ToSchema};

use crate::storage::shield_stats::{
    EventCount, KeyedCount, ShieldStatsError, ShieldStatsStorage, StatsScope, StoredShieldEvent,
};

/// Most active bypass tokens one project may hold. Every ingress carries
/// the hashes of an environment's tokens, so the set stays small.
pub const MAX_TOKENS_PER_PROJECT: u64 = 100;
/// Most operator entries one project, or the platform, may hold. The list
/// is returned whole, so it stays one screen of rules, not a feed.
pub const MAX_ENTRIES_PER_SCOPE: u64 = 500;
/// Proposals per page when the caller does not say, and at most. A botnet
/// can leave thousands live at once, so the list pages rather than caps.
pub const DEFAULT_PROPOSALS_PAGE_SIZE: u64 = 25;
pub const MAX_PROPOSALS_PAGE_SIZE: u64 = 100;
/// The longest search: a subject is at most 64 characters.
const MAX_PROPOSAL_SEARCH_CHARS: usize = 64;
const MAX_TOKEN_NAME_CHARS: usize = 128;
const MAX_NOTE_CHARS: usize = 500;
/// The broadest network an entry may name: a typo of `/0` must not block
/// the whole internet.
const MIN_V4_PREFIX: u8 = 8;
const MIN_V6_PREFIX: u8 = 16;
pub const DEFAULT_EVENTS_LIMIT: u64 = 20;
pub const MAX_EVENTS_LIMIT: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum ShieldConsoleError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },
    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },
    #[error(
        "Bypass token {token_id} not found for environment {environment_id} of project {project_id}, or already revoked"
    )]
    TokenNotFound {
        project_id: i32,
        environment_id: i32,
        token_id: i64,
    },
    #[error("Shield entry {entry_id} not found in {scope}")]
    EntryNotFound { scope: String, entry_id: i64 },
    #[error("Shield proposal {proposal_id} not found, or it has expired")]
    ProposalNotFound { proposal_id: i64 },
    #[error("{message}")]
    Validation { message: String },
    #[error("{scope} already has {limit} {what}; remove one before adding another")]
    LimitReached {
        scope: String,
        what: &'static str,
        limit: u64,
    },
    #[error("Shield snapshot {revision} could not be read for the lookup: {reason}")]
    Snapshot { revision: i64, reason: String },
    #[error("Database error in Shield console: {0}")]
    Database(#[from] DbErr),
    #[error(transparent)]
    Stats(#[from] ShieldStatsError),
}

fn invalid(message: impl Into<String>) -> ShieldConsoleError {
    ShieldConsoleError::Validation {
        message: message.into(),
    }
}

// ---- DTOs ---------------------------------------------------------------

/// The window a summary covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum ShieldWindow {
    #[serde(rename = "1h")]
    OneHour,
    #[default]
    #[serde(rename = "24h")]
    OneDay,
    #[serde(rename = "7d")]
    SevenDays,
}

impl ShieldWindow {
    pub fn duration(self) -> chrono::Duration {
        match self {
            Self::OneHour => chrono::Duration::hours(1),
            Self::OneDay => chrono::Duration::hours(24),
            Self::SevenDays => chrono::Duration::days(7),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ShieldSummaryQuery {
    /// `1h`, `24h` (the default) or `7d`.
    #[param(value_type = Option<ShieldWindow>)]
    pub window: Option<ShieldWindow>,
}

/// Where an environment's mode is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldModeSource {
    /// The environment sets its own mode.
    Environment,
    /// The environment inherits the project's.
    Project,
}

/// Requests by verdict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldVerdictCounts {
    pub block: u64,
    pub challenge: u64,
    pub log: u64,
}

/// Requests one source matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldSourceCount {
    /// The list, rule or signature: `spamhaus_drop`, `operator`,
    /// `CVE-2021-41773`.
    #[schema(example = "spamhaus_drop")]
    pub source: String,
    pub requests: u64,
}

/// What would have happened in `selective`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldWouldHave {
    /// Requests with a block verdict that Shield only recorded.
    pub blocked: u64,
    /// Requests with a challenge verdict that Shield only recorded.
    pub challenged: u64,
}

/// Challenges in the window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldChallengeCounts {
    pub issued: u64,
    pub solved: u64,
    pub failed: u64,
}

/// How long visitors took to solve a challenge (D6).
#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
pub struct ShieldSolveTimes {
    pub samples: u64,
    pub median_ms: Option<f64>,
    pub p90_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ShieldSummaryResponse {
    pub project_id: i32,
    pub environment_id: i32,
    pub window: ShieldWindow,
    #[schema(value_type = String, format = DateTime)]
    pub start: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub end: UtcDateTime,
    /// The mode the environment runs in.
    pub mode: ShieldMode,
    pub mode_source: ShieldModeSource,
    /// The project's mode, which an inheriting environment runs in.
    pub project_mode: ShieldMode,
    /// Requests in the window with a verdict, from the request log.
    pub verdicts: ShieldVerdictCounts,
    /// The sources behind those verdicts, most first (at most 20).
    pub sources: Vec<ShieldSourceCount>,
    /// Block and challenge verdicts Shield recorded without acting on them:
    /// what switching to `selective` would have stopped. The request log's
    /// counts less the verdicts events show were acted on, so a window that
    /// was partly in `selective` does not count those twice.
    pub would_have: ShieldWouldHave,
    pub challenges: ShieldChallengeCounts,
    pub solve_times: ShieldSolveTimes,
    /// `clickhouse` or `postgres`: where the counts come from.
    pub storage: String,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ShieldEventsQuery {
    /// Events to return, 1 to 100; 20 by default.
    pub limit: Option<u64>,
    /// Only events before this instant (the `next_before` of the previous
    /// page).
    #[param(value_type = Option<String>, format = DateTime)]
    pub before: Option<UtcDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldEventResponse {
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: UtcDateTime,
    /// `verdict`, `challenge_issued`, `challenge_solved` or
    /// `challenge_failed`.
    pub kind: String,
    pub ip: String,
    pub ja4: Option<String>,
    pub verdict: String,
    pub mode: String,
    pub reasons: Vec<String>,
    pub signature: Option<String>,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub request_id: String,
    /// The worker that saw it; `null` on the control plane.
    pub node_id: Option<i32>,
    pub difficulty: Option<i16>,
    pub solve_ms: Option<i64>,
}

impl From<StoredShieldEvent> for ShieldEventResponse {
    fn from(event: StoredShieldEvent) -> Self {
        Self {
            timestamp: event.timestamp,
            kind: event.kind,
            ip: event.ip,
            ja4: event.ja4,
            verdict: event.verdict,
            mode: event.mode,
            reasons: event.reasons,
            signature: event.signature,
            host: event.host,
            path: event.path,
            user_agent: event.user_agent,
            request_id: event.request_id,
            node_id: event.node_id,
            difficulty: event.difficulty,
            solve_ms: event.solve_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldEventsResponse {
    pub events: Vec<ShieldEventResponse>,
    /// Pass as `before` for the next page; `null` when this one is the last.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_before: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ShieldLookupQuery {
    /// A client address.
    pub ip: Option<String>,
    /// A JA4 TLS fingerprint.
    pub ja4: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldLookupResponse {
    pub ip: Option<String>,
    pub ja4: Option<String>,
    /// `pass`, `log`, `challenge` or `block`: what the lists say.
    pub action: String,
    /// An operator allow rule matched: the client passes whatever else it
    /// matches.
    pub allowed: bool,
    /// Why, one line per match: `spamhaus_drop (185.93.89.0/24)`.
    pub reasons: Vec<String>,
    /// The environment's mode, which decides whether the action is taken.
    pub mode: ShieldMode,
    /// Whether this environment acts on the action rather than recording it.
    pub enforced: bool,
    /// The snapshot looked up; `null` when none is stored yet and only the
    /// built-in lists apply.
    pub snapshot_revision: Option<i64>,
}

/// Which environments a bypass token is valid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldTokenScope {
    Environment,
    Project,
}

/// A bypass token, without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldTokenResponse {
    pub id: i64,
    pub name: String,
    pub scope: ShieldTokenScope,
    /// The environment it is limited to; `null` for a project-wide token.
    pub environment_id: Option<i32>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_used_at: Option<UtcDateTime>,
}

impl From<shield_bypass_tokens::Model> for ShieldTokenResponse {
    fn from(token: shield_bypass_tokens::Model) -> Self {
        Self {
            id: token.id,
            name: token.name,
            scope: match token.environment_id {
                Some(_) => ShieldTokenScope::Environment,
                None => ShieldTokenScope::Project,
            },
            environment_id: token.environment_id,
            created_by: token.created_by,
            created_at: token.created_at,
            last_used_at: token.last_used_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateShieldTokenRequest {
    /// What uses it: `uptime monitor`, `CI`.
    pub name: String,
    /// Valid for every environment of the project, not only this one.
    #[serde(default)]
    pub project_wide: bool,
}

/// A new bypass token. `secret` is shown this once and never again.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedShieldTokenResponse {
    #[serde(flatten)]
    pub token: ShieldTokenResponse,
    /// Send it in `X-Temps-Shield-Token`.
    #[schema(example = "tst_12_…")]
    pub secret: String,
}

/// The verdict an operator entry gives what it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldEntryVerdict {
    /// Let it through whatever any list says.
    Allow,
    Log,
    Challenge,
    Block,
}

impl ShieldEntryVerdict {
    fn as_str(self) -> &'static str {
        self.list_verdict().as_str()
    }

    fn list_verdict(self) -> ListVerdict {
        match self {
            Self::Allow => ListVerdict::Allow,
            Self::Log => ListVerdict::Log,
            Self::Challenge => ListVerdict::Challenge,
            Self::Block => ListVerdict::Block,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match ListVerdict::parse(text)? {
            ListVerdict::Allow => Some(Self::Allow),
            ListVerdict::Log => Some(Self::Log),
            ListVerdict::Challenge => Some(Self::Challenge),
            ListVerdict::Block => Some(Self::Block),
        }
    }
}

/// Whose entries: the platform's, or one project's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldEntryScope {
    Platform,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOwner {
    Platform,
    Project(i32),
}

impl EntryOwner {
    fn scope(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Project(_) => "project",
        }
    }

    fn project_id(self) -> Option<i32> {
        match self {
            Self::Platform => None,
            Self::Project(id) => Some(id),
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Platform => "the platform".to_string(),
            Self::Project(id) => format!("project {id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldEntryResponse {
    pub id: i64,
    pub scope: ShieldEntryScope,
    pub project_id: Option<i32>,
    /// The address range, when the entry names one.
    #[schema(example = "185.93.89.0/24")]
    pub network: Option<String>,
    /// The fingerprint, when the entry names one.
    pub ja4: Option<String>,
    pub verdict: ShieldEntryVerdict,
    pub note: Option<String>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    /// After this the entry no longer applies; `null` for never.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<UtcDateTime>,
}

/// Where a learned proposal stands. Only `approved` ones apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldProposalStatus {
    /// Waiting for an instance admin.
    Pending,
    /// Applies on every ingress until it expires.
    Approved,
    /// Kept so learning does not propose it again before it expires.
    Rejected,
}

impl ShieldProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ShieldProposalsQuery {
    /// Only proposals in this state; all of them when omitted.
    #[param(value_type = Option<ShieldProposalStatus>)]
    pub status: Option<ShieldProposalStatus>,
    /// Only proposals whose address or fingerprint contains this text,
    /// ignoring case.
    pub q: Option<String>,
    /// Page number, from 1 (the default).
    #[param(example = 1)]
    pub page: Option<u64>,
    /// Proposals per page: 25 by default, at most 100.
    #[param(example = 25)]
    pub page_size: Option<u64>,
}

/// One page of proposals, newest evidence first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldProposalPage {
    pub items: Vec<ShieldProposalResponse>,
    pub page: u64,
    pub page_size: u64,
    /// Proposals that match, on every page.
    pub total: u64,
}

/// What a proposal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShieldProposalKind {
    /// One address.
    Ip,
    /// A JA4 fingerprint.
    Ja4,
}

/// An address or fingerprint fleet learning proposes to challenge or block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShieldProposalResponse {
    pub id: i64,
    pub kind: ShieldProposalKind,
    /// The address or the fingerprint.
    #[schema(example = "203.0.113.7")]
    pub subject: String,
    /// `challenge` or `block`.
    pub verdict: ShieldEntryVerdict,
    /// The evidence, as learning summarised it.
    pub reason: String,
    /// Events behind it in the last learning window.
    pub hits: i64,
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub last_seen: UtcDateTime,
    /// After this the proposal, and an approval of it, no longer apply.
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: UtcDateTime,
    pub status: ShieldProposalStatus,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub decided_at: Option<UtcDateTime>,
    pub decided_by: Option<i32>,
}

impl ShieldProposalResponse {
    fn from_model(row: shield_observed::Model) -> Result<Self, ShieldConsoleError> {
        let unknown = |column: &str, value: &str| {
            ShieldConsoleError::Database(DbErr::Custom(format!(
                "shield_observed {} has an unknown {column} `{value}`",
                row.id
            )))
        };
        let kind = match row.kind.as_str() {
            "ip" => ShieldProposalKind::Ip,
            "ja4" => ShieldProposalKind::Ja4,
            other => return Err(unknown("kind", other)),
        };
        let verdict = ShieldEntryVerdict::parse(&row.verdict)
            .ok_or_else(|| unknown("verdict", &row.verdict))?;
        let status = ShieldProposalStatus::parse(&row.status)
            .ok_or_else(|| unknown("status", &row.status))?;
        Ok(Self {
            id: row.id,
            kind,
            subject: row.subject,
            verdict,
            reason: row.reason,
            hits: row.hits,
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            expires_at: row.expires_at,
            status,
            decided_at: row.decided_at,
            decided_by: row.decided_by,
        })
    }
}

impl ShieldEntryResponse {
    fn from_model(entry: shield_operator_entries::Model) -> Result<Self, ShieldConsoleError> {
        // The CHECK constraint keeps these to the four names; anything else
        // is a row a newer binary wrote, reported rather than guessed.
        let verdict = ShieldEntryVerdict::parse(&entry.verdict).ok_or_else(|| {
            ShieldConsoleError::Database(DbErr::Custom(format!(
                "shield_operator_entries {} has an unknown verdict `{}`",
                entry.id, entry.verdict
            )))
        })?;
        Ok(Self {
            id: entry.id,
            scope: match entry.scope.as_str() {
                "platform" => ShieldEntryScope::Platform,
                _ => ShieldEntryScope::Project,
            },
            project_id: entry.project_id,
            network: entry.network,
            ja4: entry.ja4,
            verdict,
            note: entry.note,
            created_by: entry.created_by,
            created_at: entry.created_at,
            expires_at: entry.expires_at,
        })
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateShieldEntryRequest {
    /// An address or CIDR range; exactly one of `network` and `ja4`.
    pub network: Option<String>,
    /// A JA4 fingerprint; exactly one of `network` and `ja4`.
    pub ja4: Option<String>,
    pub verdict: ShieldEntryVerdict,
    pub note: Option<String>,
    /// When the entry stops applying; omit for never.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<UtcDateTime>,
}

/// Replaces an entry's verdict, note and expiry; what it matches stays.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateShieldEntryRequest {
    pub verdict: ShieldEntryVerdict,
    pub note: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<UtcDateTime>,
}

// ---- service ------------------------------------------------------------

/// The decoded snapshot the last lookup used, kept until a newer revision
/// is stored so lookups do not decode it each time.
struct CachedIndex {
    revision: i64,
    index: Index,
}

pub struct ShieldConsoleService {
    db: Arc<DatabaseConnection>,
    stats: Arc<dyn ShieldStatsStorage>,
    index: ArcSwapOption<CachedIndex>,
}

/// An environment's mode and where it is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentMode {
    pub mode: ShieldMode,
    pub source: ShieldModeSource,
    pub project_mode: ShieldMode,
}

impl EnvironmentMode {
    /// [`ShieldMode::effective`], and which of the columns decided it.
    pub fn resolve(
        environment_mode: Option<&str>,
        environment_attack: Option<bool>,
        project_mode: &str,
        project_attack: bool,
    ) -> Self {
        let mode = ShieldMode::effective(
            environment_mode,
            project_mode,
            environment_attack,
            project_attack,
        );
        let source = match environment_attack {
            Some(true) => ShieldModeSource::Environment,
            None if project_attack => ShieldModeSource::Project,
            _ if environment_mode.and_then(ShieldMode::parse).is_some() => {
                ShieldModeSource::Environment
            }
            _ => ShieldModeSource::Project,
        };
        Self {
            mode,
            source,
            project_mode: ShieldMode::effective(None, project_mode, None, project_attack),
        }
    }
}

impl ShieldConsoleService {
    pub fn new(db: Arc<DatabaseConnection>, stats: Arc<dyn ShieldStatsStorage>) -> Self {
        Self {
            db,
            stats,
            index: ArcSwapOption::empty(),
        }
    }

    /// The environment's mode; also proves it belongs to the project.
    pub async fn environment_mode(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentMode, ShieldConsoleError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::ShieldMode)
            .column(projects::Column::AttackMode)
            .into_tuple::<(String, bool)>()
            .one(self.db.as_ref())
            .await?
            .ok_or(ShieldConsoleError::ProjectNotFound { project_id })?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .select_only()
            .column(environments::Column::ShieldMode)
            .column(environments::Column::AttackMode)
            .into_tuple::<(Option<String>, Option<bool>)>()
            .one(self.db.as_ref())
            .await?
            .ok_or(ShieldConsoleError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        Ok(EnvironmentMode::resolve(
            environment.0.as_deref(),
            environment.1,
            &project.0,
            project.1,
        ))
    }

    async fn ensure_project(&self, project_id: i32) -> Result<(), ShieldConsoleError> {
        projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .select_only()
            .column(projects::Column::Id)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?
            .map(|_| ())
            .ok_or(ShieldConsoleError::ProjectNotFound { project_id })
    }

    pub async fn summary(
        &self,
        project_id: i32,
        environment_id: i32,
        window: ShieldWindow,
    ) -> Result<ShieldSummaryResponse, ShieldConsoleError> {
        let mode = self.environment_mode(project_id, environment_id).await?;
        let end = Utc::now();
        let scope = StatsScope {
            project_id,
            environment_id,
            start: end - window.duration(),
            end,
        };
        let (verdicts, sources, events, solve_times) = tokio::try_join!(
            self.stats.verdict_counts(scope),
            self.stats.source_counts(scope),
            self.stats.event_counts(scope),
            self.stats.solve_times(scope),
        )?;
        let verdicts = verdict_counts(&verdicts);
        Ok(ShieldSummaryResponse {
            project_id,
            environment_id,
            window,
            start: scope.start,
            end: scope.end,
            mode: mode.mode,
            mode_source: mode.source,
            project_mode: mode.project_mode,
            would_have: would_have(&verdicts, &events),
            verdicts,
            sources: sources
                .into_iter()
                .map(|row| ShieldSourceCount {
                    source: row.key,
                    requests: row.count,
                })
                .collect(),
            challenges: challenge_counts(&events),
            solve_times: ShieldSolveTimes {
                samples: solve_times.samples,
                median_ms: solve_times.median_ms,
                p90_ms: solve_times.p90_ms,
            },
            storage: self.stats.backend().to_string(),
        })
    }

    pub async fn events(
        &self,
        project_id: i32,
        environment_id: i32,
        query: ShieldEventsQuery,
    ) -> Result<ShieldEventsResponse, ShieldConsoleError> {
        self.environment_mode(project_id, environment_id).await?;
        let limit = query
            .limit
            .unwrap_or(DEFAULT_EVENTS_LIMIT)
            .clamp(1, MAX_EVENTS_LIMIT);
        let events = self
            .stats
            .recent_events(project_id, environment_id, query.before, limit)
            .await?;
        let next_before = (events.len() as u64 == limit)
            .then(|| events.last().map(|event| event.timestamp))
            .flatten();
        Ok(ShieldEventsResponse {
            events: events.into_iter().map(Into::into).collect(),
            next_before,
        })
    }

    pub async fn lookup(
        &self,
        project_id: i32,
        environment_id: i32,
        query: ShieldLookupQuery,
    ) -> Result<ShieldLookupResponse, ShieldConsoleError> {
        let ip = match query
            .ip
            .as_deref()
            .map(str::trim)
            .filter(|ip| !ip.is_empty())
        {
            Some(text) => Some(
                text.parse::<IpAddr>()
                    .map_err(|_| invalid(format!("`{text}` is not an IPv4 or IPv6 address")))?,
            ),
            None => None,
        };
        let ja4 = match query
            .ja4
            .as_deref()
            .map(str::trim)
            .filter(|ja4| !ja4.is_empty())
        {
            Some(text) => Some(parse_ja4(text)?),
            None => None,
        };
        if ip.is_none() && ja4.is_none() {
            return Err(invalid(
                "Give an address (`ip`), a JA4 fingerprint (`ja4`), or both",
            ));
        }
        let mode = self.environment_mode(project_id, environment_id).await?;
        let cached = self.current_index().await?;
        let now = i64::try_from(temps_shield::unix_now()).unwrap_or(i64::MAX);
        let lookup = match &cached {
            Some(cached) => cached
                .index
                .evaluate_client(None, ip, ja4.as_deref(), project_id, now),
            None => Index::baseline().evaluate_client(None, ip, ja4.as_deref(), project_id, now),
        };
        Ok(ShieldLookupResponse {
            ip: ip.map(|ip| ip.to_string()),
            ja4,
            action: lookup.verdict.action.as_str().to_string(),
            allowed: lookup.allowed,
            reasons: lookup.verdict.reason_strings(),
            mode: mode.mode,
            enforced: mode.mode.enforces(),
            snapshot_revision: cached.map(|cached| cached.revision),
        })
    }

    /// The newest stored snapshot, decoded; reused while no newer one is
    /// stored.
    async fn current_index(&self) -> Result<Option<Arc<CachedIndex>>, ShieldConsoleError> {
        let newest = shield_snapshots::Entity::find()
            .select_only()
            .column(shield_snapshots::Column::Revision)
            .order_by_desc(shield_snapshots::Column::Revision)
            .into_tuple::<i64>()
            .one(self.db.as_ref())
            .await?;
        let Some(revision) = newest else {
            return Ok(None);
        };
        if let Some(cached) = self.index.load_full() {
            if cached.revision == revision {
                return Ok(Some(cached));
            }
        }
        let row = shield_snapshots::Entity::find_by_id(revision)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ShieldConsoleError::Snapshot {
                revision,
                reason: "it was pruned while being read; retry".to_string(),
            })?;
        if hex::encode(Sha256::digest(&row.body)) != row.sha256 {
            return Err(ShieldConsoleError::Snapshot {
                revision,
                reason: "its body does not match its checksum".to_string(),
            });
        }
        // Decoding and indexing a large snapshot takes milliseconds.
        let index = tokio::task::spawn_blocking(move || {
            ShieldSnapshot::decode(&row.body).map(|mut snapshot| {
                snapshot.revision = u64::try_from(revision).unwrap_or(0);
                Index::new(&snapshot)
            })
        })
        .await
        .map_err(|e| ShieldConsoleError::Snapshot {
            revision,
            reason: format!("decoding stopped: {e}"),
        })?
        .map_err(|e| ShieldConsoleError::Snapshot {
            revision,
            reason: e.to_string(),
        })?;
        let cached = Arc::new(CachedIndex { revision, index });
        self.index.store(Some(Arc::clone(&cached)));
        Ok(Some(cached))
    }

    // ---- bypass tokens ----------------------------------------------------

    /// The active tokens valid for the environment: its own and the
    /// project-wide ones, newest first.
    pub async fn list_tokens(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<ShieldTokenResponse>, ShieldConsoleError> {
        self.environment_mode(project_id, environment_id).await?;
        let tokens = shield_bypass_tokens::Entity::find()
            .filter(shield_bypass_tokens::Column::ProjectId.eq(project_id))
            .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
            .filter(
                shield_bypass_tokens::Column::EnvironmentId
                    .eq(environment_id)
                    .or(shield_bypass_tokens::Column::EnvironmentId.is_null()),
            )
            .order_by_desc(shield_bypass_tokens::Column::CreatedAt)
            .order_by_desc(shield_bypass_tokens::Column::Id)
            .limit(MAX_TOKENS_PER_PROJECT)
            .all(self.db.as_ref())
            .await?;
        Ok(tokens.into_iter().map(Into::into).collect())
    }

    /// Mints a token. Only its hash is stored; the token itself is in the
    /// answer and nowhere else.
    pub async fn create_token(
        &self,
        project_id: i32,
        environment_id: i32,
        request: CreateShieldTokenRequest,
        created_by: Option<i32>,
    ) -> Result<CreatedShieldTokenResponse, ShieldConsoleError> {
        let name = request.name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_TOKEN_NAME_CHARS {
            return Err(invalid(format!(
                "A bypass token needs a name of 1 to {MAX_TOKEN_NAME_CHARS} characters"
            )));
        }
        if name.chars().any(char::is_control) {
            return Err(invalid(
                "A bypass token name cannot hold control characters",
            ));
        }
        self.environment_mode(project_id, environment_id).await?;
        let active = shield_bypass_tokens::Entity::find()
            .filter(shield_bypass_tokens::Column::ProjectId.eq(project_id))
            .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
            .count(self.db.as_ref())
            .await?;
        if active >= MAX_TOKENS_PER_PROJECT {
            return Err(ShieldConsoleError::LimitReached {
                scope: format!("Project {project_id}"),
                what: "active bypass tokens",
                limit: MAX_TOKENS_PER_PROJECT,
            });
        }

        // The token names its row, so the row comes first; a random hash
        // that no token has holds its place until the real one is set, in
        // the same transaction.
        let txn = self.db.begin().await?;
        let placeholder: [u8; 32] = rand::random();
        let row = shield_bypass_tokens::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set((!request.project_wide).then_some(environment_id)),
            name: Set(name),
            token_hash: Set(temps_shield::bypass::to_hex(&placeholder)),
            created_by: Set(created_by),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        let (secret, hash) = temps_shield::bypass::generate(row.id);
        let mut active: shield_bypass_tokens::ActiveModel = row.into();
        active.token_hash = Set(temps_shield::bypass::to_hex(&hash));
        let row = active.update(&txn).await?;
        txn.commit().await?;
        info!(
            project_id,
            environment_id,
            token_id = row.id,
            "Shield bypass token created"
        );
        self.notify_routes(project_id).await;
        Ok(CreatedShieldTokenResponse {
            token: row.into(),
            secret,
        })
    }

    pub async fn revoke_token(
        &self,
        project_id: i32,
        environment_id: i32,
        token_id: i64,
    ) -> Result<ShieldTokenResponse, ShieldConsoleError> {
        self.environment_mode(project_id, environment_id).await?;
        let not_found = ShieldConsoleError::TokenNotFound {
            project_id,
            environment_id,
            token_id,
        };
        let token = shield_bypass_tokens::Entity::find_by_id(token_id)
            .filter(shield_bypass_tokens::Column::ProjectId.eq(project_id))
            .filter(shield_bypass_tokens::Column::RevokedAt.is_null())
            .filter(
                shield_bypass_tokens::Column::EnvironmentId
                    .eq(environment_id)
                    .or(shield_bypass_tokens::Column::EnvironmentId.is_null()),
            )
            .one(self.db.as_ref())
            .await?
            .ok_or(not_found)?;
        let mut active: shield_bypass_tokens::ActiveModel = token.into();
        active.revoked_at = Set(Some(Utc::now()));
        let token = active.update(self.db.as_ref()).await?;
        info!(
            project_id,
            environment_id, token_id, "Shield bypass token revoked"
        );
        self.notify_routes(project_id).await;
        Ok(token.into())
    }

    /// Bypass tokens reach every ingress with the route table; ask it to
    /// reload so a revoked token stops working now, not at the next deploy.
    async fn notify_routes(&self, project_id: i32) {
        // A fixed string: Postgres has no parameterised NOTIFY.
        if let Err(e) = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "NOTIFY route_table_changes".to_string(),
            ))
            .await
        {
            error!(
                error = %e,
                project_id,
                "Failed to notify route_table_changes after a Shield bypass token change; \
                 ingresses keep the previous tokens until the next route reload"
            );
        }
    }

    // ---- operator entries -------------------------------------------------

    pub async fn list_entries(
        &self,
        owner: EntryOwner,
    ) -> Result<Vec<ShieldEntryResponse>, ShieldConsoleError> {
        if let EntryOwner::Project(project_id) = owner {
            self.ensure_project(project_id).await?;
        }
        let rows = shield_operator_entries::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT {ENTRY_COLUMNS} FROM shield_operator_entries \
                     WHERE scope = $1 AND project_id IS NOT DISTINCT FROM $2 \
                     ORDER BY created_at DESC, id DESC LIMIT {MAX_ENTRIES_PER_SCOPE}"
                ),
                [owner.scope().into(), owner.project_id().into()],
            ))
            .all(self.db.as_ref())
            .await?;
        rows.into_iter()
            .map(ShieldEntryResponse::from_model)
            .collect()
    }

    pub async fn create_entry(
        &self,
        owner: EntryOwner,
        request: CreateShieldEntryRequest,
        created_by: Option<i32>,
    ) -> Result<ShieldEntryResponse, ShieldConsoleError> {
        let subject = EntrySubject::parse(request.network.as_deref(), request.ja4.as_deref())?;
        let note = normalize_note(request.note)?;
        check_expiry(request.expires_at)?;
        if let EntryOwner::Project(project_id) = owner {
            self.ensure_project(project_id).await?;
        }
        let (existing, total) = self.count_entries(owner, &subject).await?;
        if existing > 0 {
            return Err(invalid(format!(
                "{} already has an entry for {}; edit that one instead",
                owner.describe(),
                subject.describe()
            )));
        }
        if total >= MAX_ENTRIES_PER_SCOPE {
            return Err(ShieldConsoleError::LimitReached {
                scope: capitalize(&owner.describe()),
                what: "Shield entries",
                limit: MAX_ENTRIES_PER_SCOPE,
            });
        }
        let (network, ja4) = subject.columns();
        let row = shield_operator_entries::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "INSERT INTO shield_operator_entries \
                         (scope, project_id, network, ja4, verdict, note, created_by, expires_at) \
                     VALUES ($1, $2, $3::cidr, $4, $5, $6, $7, $8) \
                     RETURNING {ENTRY_COLUMNS}"
                ),
                [
                    owner.scope().into(),
                    owner.project_id().into(),
                    network.into(),
                    ja4.into(),
                    request.verdict.as_str().into(),
                    note.into(),
                    created_by.into(),
                    request.expires_at.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DbErr::RecordNotInserted)?;
        ShieldEntryResponse::from_model(row)
    }

    pub async fn update_entry(
        &self,
        owner: EntryOwner,
        entry_id: i64,
        request: UpdateShieldEntryRequest,
    ) -> Result<ShieldEntryResponse, ShieldConsoleError> {
        let note = normalize_note(request.note)?;
        check_expiry(request.expires_at)?;
        let row = shield_operator_entries::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "UPDATE shield_operator_entries SET verdict = $1, note = $2, expires_at = $3 \
                     WHERE id = $4 AND scope = $5 AND project_id IS NOT DISTINCT FROM $6 \
                     RETURNING {ENTRY_COLUMNS}"
                ),
                [
                    request.verdict.as_str().into(),
                    note.into(),
                    request.expires_at.into(),
                    entry_id.into(),
                    owner.scope().into(),
                    owner.project_id().into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ShieldConsoleError::EntryNotFound {
                scope: owner.describe(),
                entry_id,
            })?;
        ShieldEntryResponse::from_model(row)
    }

    pub async fn delete_entry(
        &self,
        owner: EntryOwner,
        entry_id: i64,
    ) -> Result<ShieldEntryResponse, ShieldConsoleError> {
        let row = shield_operator_entries::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "DELETE FROM shield_operator_entries \
                     WHERE id = $1 AND scope = $2 AND project_id IS NOT DISTINCT FROM $3 \
                     RETURNING {ENTRY_COLUMNS}"
                ),
                [
                    entry_id.into(),
                    owner.scope().into(),
                    owner.project_id().into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ShieldConsoleError::EntryNotFound {
                scope: owner.describe(),
                entry_id,
            })?;
        ShieldEntryResponse::from_model(row)
    }

    /// Proposals that have not expired, newest evidence first, a page at a
    /// time.
    pub async fn list_proposals(
        &self,
        query: ShieldProposalsQuery,
    ) -> Result<ShieldProposalPage, ShieldConsoleError> {
        let page = query.page.unwrap_or(1).max(1);
        let page_size = query
            .page_size
            .unwrap_or(DEFAULT_PROPOSALS_PAGE_SIZE)
            .clamp(1, MAX_PROPOSALS_PAGE_SIZE);
        let search = query
            .q
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        if search
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_PROPOSAL_SEARCH_CHARS)
        {
            return Err(invalid(format!(
                "A search holds at most {MAX_PROPOSAL_SEARCH_CHARS} characters"
            )));
        }
        let status = query.status.map(|status| status.as_str().to_string());
        // `strpos` rather than LIKE, so `%` and `_` in a search are plain text.
        const MATCHES: &str = "expires_at > now() AND ($1::text IS NULL OR status = $1) \
             AND ($2::text IS NULL OR strpos(lower(subject), lower($2)) > 0)";
        let offset = (page - 1).saturating_mul(page_size);
        let txn = self.db.begin().await?;
        let total: i64 = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("SELECT COUNT(*)::bigint AS total FROM shield_observed WHERE {MATCHES}"),
                [status.clone().into(), search.clone().into()],
            ))
            .await?
            .map(|row| row.try_get("", "total"))
            .transpose()?
            .unwrap_or(0);
        let rows = shield_observed::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT * FROM shield_observed WHERE {MATCHES} \
                     ORDER BY last_seen DESC, id DESC LIMIT $3 OFFSET $4"
                ),
                [
                    status.into(),
                    search.into(),
                    (page_size as i64).into(),
                    i64::try_from(offset).unwrap_or(i64::MAX).into(),
                ],
            ))
            .all(&txn)
            .await?;
        txn.commit().await?;
        Ok(ShieldProposalPage {
            items: rows
                .into_iter()
                .map(ShieldProposalResponse::from_model)
                .collect::<Result<_, _>>()?,
            page,
            page_size,
            total: u64::try_from(total).unwrap_or(0),
        })
    }

    /// Approves or rejects a proposal, or changes an earlier decision. The
    /// snapshot compile picks the change up on its next pass.
    pub async fn decide_proposal(
        &self,
        proposal_id: i64,
        status: ShieldProposalStatus,
        decided_by: Option<i32>,
    ) -> Result<ShieldProposalResponse, ShieldConsoleError> {
        if status == ShieldProposalStatus::Pending {
            return Err(invalid(
                "A proposal is approved or rejected, not set back to pending",
            ));
        }
        let row = shield_observed::Entity::find()
            .from_raw_sql(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE shield_observed \
                 SET status = $2, decided_at = now(), decided_by = $3 \
                 WHERE id = $1 AND expires_at > now() \
                 RETURNING *",
                [
                    proposal_id.into(),
                    status.as_str().into(),
                    decided_by.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?
            .ok_or(ShieldConsoleError::ProposalNotFound { proposal_id })?;
        info!(
            proposal_id,
            status = status.as_str(),
            subject = %row.subject,
            "Shield proposal decided"
        );
        ShieldProposalResponse::from_model(row)
    }

    /// Entries in `owner` for the same subject, and in all.
    async fn count_entries(
        &self,
        owner: EntryOwner,
        subject: &EntrySubject,
    ) -> Result<(i64, u64), ShieldConsoleError> {
        let (network, ja4) = subject.columns();
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT \
                     COUNT(*) FILTER (WHERE network = $3::cidr OR ja4 = $4)::bigint AS same, \
                     COUNT(*)::bigint AS total \
                 FROM shield_operator_entries \
                 WHERE scope = $1 AND project_id IS NOT DISTINCT FROM $2",
                [
                    owner.scope().into(),
                    owner.project_id().into(),
                    network.into(),
                    ja4.into(),
                ],
            ))
            .await?;
        match row {
            Some(row) => {
                let same: i64 = row.try_get("", "same")?;
                let total: i64 = row.try_get("", "total")?;
                Ok((same, u64::try_from(total).unwrap_or(0)))
            }
            None => Ok((0, 0)),
        }
    }
}

/// The entry columns, with the CIDR as text so the entity can read it.
const ENTRY_COLUMNS: &str = "id, scope, project_id, network::text AS network, ja4, verdict, \
     note, created_by, created_at, expires_at";

/// What an operator entry matches.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntrySubject {
    Network(Network),
    Ja4(String),
}

impl EntrySubject {
    fn parse(network: Option<&str>, ja4: Option<&str>) -> Result<Self, ShieldConsoleError> {
        let network = network.map(str::trim).filter(|text| !text.is_empty());
        let ja4 = ja4.map(str::trim).filter(|text| !text.is_empty());
        match (network, ja4) {
            (Some(text), None) => {
                let network = Network::parse(text).ok_or_else(|| {
                    invalid(format!(
                        "`{text}` is not an address or CIDR range such as 203.0.113.0/24"
                    ))
                })?;
                let broadest = match network {
                    Network::V4 { .. } => MIN_V4_PREFIX,
                    Network::V6 { .. } => MIN_V6_PREFIX,
                };
                if network.prefix() < broadest {
                    return Err(invalid(format!(
                        "`{text}` is broader than /{broadest}; an entry names a range, not a \
                         large share of the internet"
                    )));
                }
                Ok(Self::Network(network))
            }
            (None, Some(text)) => Ok(Self::Ja4(parse_ja4(text)?)),
            (Some(_), Some(_)) => Err(invalid(
                "An entry names either a network or a JA4 fingerprint, not both",
            )),
            (None, None) => Err(invalid(
                "An entry needs a network (an address or CIDR range) or a JA4 fingerprint",
            )),
        }
    }

    fn columns(&self) -> (Option<String>, Option<String>) {
        match self {
            Self::Network(network) => (Some(network.to_string()), None),
            Self::Ja4(ja4) => (None, Some(ja4.clone())),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Network(network) => network.to_string(),
            Self::Ja4(ja4) => format!("ja4 {ja4}"),
        }
    }
}

fn parse_ja4(text: &str) -> Result<String, ShieldConsoleError> {
    temps_shield::ja4::Ja4::parse_forwarded(text)
        .map(|ja4| ja4.as_str().to_string())
        .ok_or_else(|| {
            invalid(format!(
                "`{text}` is not a JA4 fingerprint such as t13d1516h2_8daaf6152771_02713d6af862"
            ))
        })
}

fn normalize_note(note: Option<String>) -> Result<Option<String>, ShieldConsoleError> {
    let note = note
        .map(|note| note.trim().to_string())
        .filter(|note| !note.is_empty());
    if note
        .as_ref()
        .is_some_and(|note| note.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(invalid(format!(
            "A note holds at most {MAX_NOTE_CHARS} characters"
        )));
    }
    Ok(note)
}

fn check_expiry(expires_at: Option<UtcDateTime>) -> Result<(), ShieldConsoleError> {
    match expires_at {
        Some(at) if at <= Utc::now() => Err(invalid(format!(
            "expires_at {} is in the past; omit it for an entry that does not expire",
            at.to_rfc3339()
        ))),
        _ => Ok(()),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn verdict_counts(rows: &[KeyedCount]) -> ShieldVerdictCounts {
    let mut counts = ShieldVerdictCounts::default();
    for row in rows {
        match row.key.as_str() {
            "block" => counts.block += row.count,
            "challenge" => counts.challenge += row.count,
            "log" => counts.log += row.count,
            // `pass` is not stored; anything else is not a verdict.
            _ => {}
        }
    }
    counts
}

/// The request log does not record the mode, so the block and challenge
/// verdicts it holds are the would-have counts less those events show were
/// acted on (`selective` or `attack`).
fn would_have(verdicts: &ShieldVerdictCounts, events: &[EventCount]) -> ShieldWouldHave {
    let acted = |verdict: &str| -> u64 {
        events
            .iter()
            .filter(|event| {
                event.kind == temps_shield::EventKind::Verdict.as_str()
                    && event.verdict == verdict
                    && ShieldMode::parse(&event.mode).is_some_and(ShieldMode::enforces)
            })
            .map(|event| event.count)
            .sum()
    };
    ShieldWouldHave {
        blocked: verdicts.block.saturating_sub(acted("block")),
        challenged: verdicts.challenge.saturating_sub(acted("challenge")),
    }
}

fn challenge_counts(events: &[EventCount]) -> ShieldChallengeCounts {
    let of = |kind: temps_shield::EventKind| -> u64 {
        events
            .iter()
            .filter(|event| event.kind == kind.as_str())
            .map(|event| event.count)
            .sum()
    };
    ShieldChallengeCounts {
        issued: of(temps_shield::EventKind::ChallengeIssued),
        solved: of(temps_shield::EventKind::ChallengeSolved),
        failed: of(temps_shield::EventKind::ChallengeFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, verdict: &str, mode: &str, count: u64) -> EventCount {
        EventCount {
            kind: kind.into(),
            verdict: verdict.into(),
            mode: mode.into(),
            count,
        }
    }

    #[test]
    fn would_have_leaves_out_what_was_acted_on() {
        let verdicts = verdict_counts(&[
            KeyedCount {
                key: "block".into(),
                count: 10,
            },
            KeyedCount {
                key: "challenge".into(),
                count: 7,
            },
            KeyedCount {
                key: "log".into(),
                count: 3,
            },
        ]);
        assert_eq!(
            verdicts,
            ShieldVerdictCounts {
                block: 10,
                challenge: 7,
                log: 3
            }
        );
        let events = [
            event("verdict", "block", "selective", 4),
            event("verdict", "block", "log", 6),
            event("verdict", "challenge", "attack", 9),
            event("challenge_issued", "challenge", "selective", 5),
            event("challenge_solved", "challenge", "selective", 2),
            event("challenge_failed", "challenge", "selective", 1),
        ];
        assert_eq!(
            would_have(&verdicts, &events),
            ShieldWouldHave {
                blocked: 6,
                // More acted on than logged (events from a dropped log
                // batch): never below zero.
                challenged: 0,
            }
        );
        assert_eq!(
            challenge_counts(&events),
            ShieldChallengeCounts {
                issued: 5,
                solved: 2,
                failed: 1
            }
        );
        // In log mode the whole time, every verdict would have been acted on.
        assert_eq!(
            would_have(&verdicts, &[event("verdict", "block", "log", 10)]),
            ShieldWouldHave {
                blocked: 10,
                challenged: 7
            }
        );
    }

    #[test]
    fn the_mode_says_where_it_is_set() {
        let resolve = EnvironmentMode::resolve;
        assert_eq!(
            resolve(None, None, "selective", false),
            EnvironmentMode {
                mode: ShieldMode::Selective,
                source: ShieldModeSource::Project,
                project_mode: ShieldMode::Selective
            }
        );
        assert_eq!(
            resolve(Some("off"), Some(false), "selective", false).source,
            ShieldModeSource::Environment
        );
        let legacy_project_attack = resolve(None, None, "log", true);
        assert_eq!(legacy_project_attack.mode, ShieldMode::Attack);
        assert_eq!(legacy_project_attack.source, ShieldModeSource::Project);
        assert_eq!(legacy_project_attack.project_mode, ShieldMode::Attack);
        let env_attack = resolve(Some("log"), Some(true), "off", false);
        assert_eq!(env_attack.mode, ShieldMode::Attack);
        assert_eq!(env_attack.source, ShieldModeSource::Environment);
    }

    #[test]
    fn entries_name_one_bounded_subject() {
        assert_eq!(
            EntrySubject::parse(Some(" 203.0.113.7/24 "), None)
                .unwrap()
                .columns(),
            (Some("203.0.113.0/24".to_string()), None)
        );
        assert_eq!(
            EntrySubject::parse(Some("2001:db8::1"), Some(""))
                .unwrap()
                .columns()
                .0
                .as_deref(),
            Some("2001:db8::1/128")
        );
        let ja4 = "t13d1516h2_8daaf6152771_02713d6af862";
        assert_eq!(
            EntrySubject::parse(None, Some(ja4)).unwrap(),
            EntrySubject::Ja4(ja4.into())
        );
        for (network, ja4, expected) in [
            (Some("0.0.0.0/0"), None, "broader than /8"),
            (Some("2001::/12"), None, "broader than /16"),
            (Some("not-an-ip"), None, "not an address"),
            (None, Some("t13d"), "not a JA4"),
            (Some("10.0.0.0/8"), Some(ja4), "not both"),
            (None, None, "needs a network"),
        ] {
            let error = EntrySubject::parse(network, ja4).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn notes_and_expiry_are_bounded() {
        assert_eq!(normalize_note(Some("  ".into())).unwrap(), None);
        assert!(normalize_note(Some("x".repeat(MAX_NOTE_CHARS + 1))).is_err());
        assert!(check_expiry(Some(Utc::now() - chrono::Duration::minutes(1))).is_err());
        assert!(check_expiry(Some(Utc::now() + chrono::Duration::days(1))).is_ok());
        assert!(check_expiry(None).is_ok());
    }

    // ---- against Postgres ------------------------------------------------
    //
    // The queries are raw SQL (CIDR columns, JSONB reasons), so they are
    // checked against the real schema. They skip without Docker.

    use sea_orm::Value;
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

    async fn database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker not available, skipping Shield console database test");
                None
            }
            Err(error) => panic!("test database: {error}"),
        }
    }

    async fn exec(db: &DatabaseConnection, sql: &str, values: Vec<Value>) {
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    /// Project 7101 with environments 7201 (inherits) and 7202 (selective),
    /// and project 7102 with environment 7203.
    async fn seed(db: &DatabaseConnection) {
        exec(
            db,
            "INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, \
             preset, created_at, updated_at, slug, shield_mode) VALUES \
             (7101, 'shield-a', 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), 'shield-a', 'log'), \
             (7102, 'shield-b', 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), 'shield-b', 'log')",
            vec![],
        )
        .await;
        exec(
            db,
            "INSERT INTO environments (id, project_id, name, slug, subdomain, host, upstreams, \
             created_at, updated_at, shield_mode) VALUES \
             (7201, 7101, 'production', 'production', 'shield-a-prod', '', '[]', now(), now(), NULL), \
             (7202, 7101, 'staging', 'staging', 'shield-a-staging', '', '[]', now(), now(), 'selective'), \
             (7203, 7102, 'production', 'production', 'shield-b-prod', '', '[]', now(), now(), NULL)",
            vec![],
        )
        .await;
    }

    async fn log_request(db: &DatabaseConnection, env: i32, verdict: &str, reasons: &[&str]) {
        let project = if env == 7203 { 7102 } else { 7101 };
        exec(
            db,
            "INSERT INTO proxy_logs (timestamp, method, path, host, status_code, request_source, \
             is_system_request, routing_status, request_id, created_date, project_id, \
             environment_id, shield_verdict, shield_reasons) VALUES \
             (now() - interval '1 minute', 'GET', '/', 'a.test', 200, 'proxy', false, 'routed', \
              gen_random_uuid()::text, current_date, $1, $2, $3, $4)",
            vec![
                project.into(),
                env.into(),
                verdict.into(),
                serde_json::json!(reasons).into(),
            ],
        )
        .await;
    }

    async fn store_event(
        db: &DatabaseConnection,
        kind: &str,
        verdict: &str,
        mode: &str,
        solve: Option<i32>,
    ) {
        exec(
            db,
            "INSERT INTO shield_events (timestamp, project_id, environment_id, request_id, kind, \
             ip, verdict, mode, reasons, host, path, user_agent, solve_ms) VALUES \
             (now() - interval '1 minute', 7101, 7201, gen_random_uuid()::text, $1, \
              '203.0.113.9', $2, $3, '[\"spamhaus_drop (203.0.113.0/24)\"]', 'a.test', '/', 'ua', $4)",
            vec![kind.into(), verdict.into(), mode.into(), solve.into()],
        )
        .await;
    }

    fn service(db: &TestDatabase) -> ShieldConsoleService {
        let db = db.connection_arc();
        ShieldConsoleService::new(
            db.clone(),
            Arc::new(crate::storage::shield_stats::PostgresShieldStats::new(db)),
        )
    }

    #[tokio::test]
    async fn the_summary_counts_one_environment_from_postgres() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        seed(&db).await;
        let drop_reason = "spamhaus_drop (203.0.113.0/24)";
        log_request(
            &db,
            7201,
            "block",
            &[drop_reason, "spamhaus_drop (198.51.100.0/24)"],
        )
        .await;
        log_request(&db, 7201, "block", &["CVE-2021-41773 on /cgi-bin/x"]).await;
        log_request(&db, 7201, "challenge", &["firehol_level1 (192.0.2.0/24)"]).await;
        log_request(&db, 7201, "log", &["iana_reserved (192.0.2.0/24)"]).await;
        // Other environments do not count.
        log_request(&db, 7202, "block", &[drop_reason]).await;
        log_request(&db, 7203, "block", &[drop_reason]).await;
        store_event(&db, "verdict", "block", "selective", None).await;
        store_event(
            &db,
            "challenge_solved",
            "challenge",
            "selective",
            Some(1000),
        )
        .await;
        store_event(
            &db,
            "challenge_solved",
            "challenge",
            "selective",
            Some(3000),
        )
        .await;
        store_event(&db, "challenge_failed", "challenge", "selective", None).await;

        let service = service(&test_db);
        let summary = service
            .summary(7101, 7201, ShieldWindow::OneDay)
            .await
            .unwrap();
        assert_eq!(summary.mode, ShieldMode::Log);
        assert_eq!(summary.mode_source, ShieldModeSource::Project);
        assert_eq!(
            summary.verdicts,
            ShieldVerdictCounts {
                block: 2,
                challenge: 1,
                log: 1
            }
        );
        // A request counts once per source, however many reasons name it.
        assert_eq!(summary.sources[0].source, "CVE-2021-41773");
        let drop = summary
            .sources
            .iter()
            .find(|source| source.source == "spamhaus_drop")
            .unwrap();
        assert_eq!(drop.requests, 1);
        assert_eq!(summary.sources.len(), 4);
        // One block was acted on in selective; the other only recorded.
        assert_eq!(
            summary.would_have,
            ShieldWouldHave {
                blocked: 1,
                challenged: 1
            }
        );
        assert_eq!(
            summary.challenges,
            ShieldChallengeCounts {
                issued: 0,
                solved: 2,
                failed: 1
            }
        );
        assert_eq!(summary.solve_times.samples, 2);
        assert_eq!(summary.solve_times.median_ms, Some(2000.0));
        assert_eq!(summary.storage, "postgres");

        let staging = service
            .summary(7101, 7202, ShieldWindow::OneHour)
            .await
            .unwrap();
        assert_eq!(staging.mode, ShieldMode::Selective);
        assert_eq!(staging.mode_source, ShieldModeSource::Environment);
        assert_eq!(staging.solve_times, ShieldSolveTimes::default());

        // Another project's environment is not found through this one.
        assert!(matches!(
            service.summary(7101, 7203, ShieldWindow::OneDay).await,
            Err(ShieldConsoleError::EnvironmentNotFound { .. })
        ));

        let events = service
            .events(
                7101,
                7201,
                ShieldEventsQuery {
                    limit: Some(3),
                    before: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(events.events.len(), 3);
        assert_eq!(events.events[0].reasons, [drop_reason]);
        let older = service
            .events(
                7101,
                7201,
                ShieldEventsQuery {
                    limit: Some(3),
                    before: events.next_before,
                },
            )
            .await
            .unwrap();
        assert!(older.events.len() <= 1, "{older:?}");
    }

    #[tokio::test]
    async fn bypass_tokens_store_only_a_hash_and_revoke() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        seed(&db).await;
        let service = service(&test_db);
        let own = service
            .create_token(
                7101,
                7201,
                CreateShieldTokenRequest {
                    name: " uptime monitor ".into(),
                    project_wide: false,
                },
                None,
            )
            .await
            .unwrap();
        let shared = service
            .create_token(
                7101,
                7202,
                CreateShieldTokenRequest {
                    name: "CI".into(),
                    project_wide: true,
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(own.token.name, "uptime monitor");
        assert_eq!(own.token.scope, ShieldTokenScope::Environment);
        assert_eq!(shared.token.scope, ShieldTokenScope::Project);
        assert!(own.secret.starts_with(&format!("tst_{}_", own.token.id)));

        let stored = shield_bypass_tokens::Entity::find_by_id(own.token.id)
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.token_hash,
            temps_shield::bypass::to_hex(&temps_shield::bypass::hash(&own.secret))
        );
        assert!(!stored.token_hash.contains(&own.secret));
        let set = temps_shield::bypass::TokenSet::from_hex(&[stored.token_hash]);
        assert_eq!(set.check(&own.secret), Some(own.token.id));

        // 7201 sees its own and the project-wide one; 7202 only the latter.
        let ids = |tokens: Vec<ShieldTokenResponse>| -> Vec<i64> {
            tokens.into_iter().map(|token| token.id).collect()
        };
        assert_eq!(
            ids(service.list_tokens(7101, 7201).await.unwrap()),
            [shared.token.id, own.token.id]
        );
        assert_eq!(
            ids(service.list_tokens(7101, 7202).await.unwrap()),
            [shared.token.id]
        );
        // Another environment cannot revoke a token that is not valid there,
        // nor another project's.
        assert!(matches!(
            service.revoke_token(7101, 7202, own.token.id).await,
            Err(ShieldConsoleError::TokenNotFound { .. })
        ));
        assert!(matches!(
            service.revoke_token(7102, 7203, own.token.id).await,
            Err(ShieldConsoleError::TokenNotFound { .. })
        ));
        service
            .revoke_token(7101, 7201, own.token.id)
            .await
            .unwrap();
        assert_eq!(
            ids(service.list_tokens(7101, 7201).await.unwrap()),
            [shared.token.id]
        );
        assert!(matches!(
            service.revoke_token(7101, 7201, own.token.id).await,
            Err(ShieldConsoleError::TokenNotFound { .. })
        ));
        let blank = service
            .create_token(
                7101,
                7201,
                CreateShieldTokenRequest {
                    name: "  ".into(),
                    project_wide: false,
                },
                None,
            )
            .await;
        assert!(matches!(blank, Err(ShieldConsoleError::Validation { .. })));
    }

    #[tokio::test]
    async fn operator_entries_round_trip_networks_and_fingerprints() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        seed(&db).await;
        let service = service(&test_db);
        let project = EntryOwner::Project(7101);
        let network = service
            .create_entry(
                project,
                CreateShieldEntryRequest {
                    network: Some("203.0.113.7/24".into()),
                    ja4: None,
                    verdict: ShieldEntryVerdict::Block,
                    note: Some(" scanner ".into()),
                    expires_at: None,
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(network.network.as_deref(), Some("203.0.113.0/24"));
        assert_eq!(network.note.as_deref(), Some("scanner"));
        assert_eq!(network.scope, ShieldEntryScope::Project);
        let duplicate = service
            .create_entry(
                project,
                CreateShieldEntryRequest {
                    network: Some("203.0.113.0/24".into()),
                    ja4: None,
                    verdict: ShieldEntryVerdict::Log,
                    note: None,
                    expires_at: None,
                },
                None,
            )
            .await;
        assert!(matches!(
            duplicate,
            Err(ShieldConsoleError::Validation { .. })
        ));
        let fingerprint = service
            .create_entry(
                EntryOwner::Platform,
                CreateShieldEntryRequest {
                    network: None,
                    ja4: Some("t13d1516h2_8daaf6152771_02713d6af862".into()),
                    verdict: ShieldEntryVerdict::Challenge,
                    note: None,
                    expires_at: Some(Utc::now() + chrono::Duration::days(1)),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(fingerprint.scope, ShieldEntryScope::Platform);
        assert_eq!(fingerprint.project_id, None);

        assert_eq!(
            service.list_entries(project).await.unwrap(),
            std::slice::from_ref(&network)
        );
        assert_eq!(
            service.list_entries(EntryOwner::Platform).await.unwrap(),
            std::slice::from_ref(&fingerprint)
        );
        assert!(service
            .list_entries(EntryOwner::Project(7102))
            .await
            .unwrap()
            .is_empty());

        let changed = service
            .update_entry(
                project,
                network.id,
                UpdateShieldEntryRequest {
                    verdict: ShieldEntryVerdict::Allow,
                    note: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(changed.verdict, ShieldEntryVerdict::Allow);
        assert_eq!(changed.network, network.network);
        // Another project, or the platform, cannot reach it.
        assert!(matches!(
            service
                .delete_entry(EntryOwner::Project(7102), network.id)
                .await,
            Err(ShieldConsoleError::EntryNotFound { .. })
        ));
        assert!(matches!(
            service.delete_entry(EntryOwner::Platform, network.id).await,
            Err(ShieldConsoleError::EntryNotFound { .. })
        ));
        service.delete_entry(project, network.id).await.unwrap();
        assert!(service.list_entries(project).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_lookup_reads_the_newest_snapshot_and_explains_itself() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        seed(&db).await;
        let service = service(&test_db);
        let query = |ip: &str| ShieldLookupQuery {
            ip: Some(ip.into()),
            ja4: None,
        };

        // Without a stored snapshot only the built-in lists apply.
        let baseline = service
            .lookup(7101, 7201, query("192.0.2.1"))
            .await
            .unwrap();
        assert_eq!(baseline.snapshot_revision, None);
        assert_eq!(baseline.action, "log");

        let snapshot = ShieldSnapshot {
            revision: 0,
            generated_at: 1_800_000_000,
            lists: vec![temps_shield::SourceList::new(
                "spamhaus_drop",
                ListVerdict::Block,
                i64::try_from(temps_shield::unix_now()).unwrap(),
                None,
                Network::parse("185.93.89.0/24"),
            )],
            ..ShieldSnapshot::default()
        };
        let body = snapshot.encode().unwrap();
        exec(
            &db,
            "INSERT INTO shield_snapshots (revision, created_at, sha256, body) \
             VALUES (41, now(), $1, $2)",
            vec![hex::encode(Sha256::digest(&body)).into(), body.into()],
        )
        .await;
        let listed = service
            .lookup(7101, 7202, query("185.93.89.10"))
            .await
            .unwrap();
        assert_eq!(listed.snapshot_revision, Some(41));
        assert_eq!(listed.action, "block");
        assert_eq!(listed.reasons, ["spamhaus_drop (185.93.89.0/24)"]);
        assert_eq!(listed.mode, ShieldMode::Selective);
        assert!(listed.enforced);

        assert!(matches!(
            service.lookup(7101, 7201, query("not-an-ip")).await,
            Err(ShieldConsoleError::Validation { .. })
        ));
        assert!(matches!(
            service
                .lookup(7101, 7201, ShieldLookupQuery::default())
                .await,
            Err(ShieldConsoleError::Validation { .. })
        ));
    }

    /// Proposals list by state, a decision can be changed, and an expired
    /// or missing one cannot be decided.
    #[tokio::test]
    async fn proposals_are_listed_and_decided_against_postgres() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        exec(
            &db,
            "INSERT INTO shield_observed (id, kind, subject, verdict, reason, first_seen, \
             last_seen, hits, expires_at) VALUES \
             (9101, 'ip', '203.0.113.7', 'block', 'exploit CVE-2021-41773', \
              now() - interval '2 hours', now() - interval '1 hour', 1, now() + interval '6 days'), \
             (9102, 'ja4', 't13d1516h2_8daaf6152771_02713d6af862', 'challenge', \
              'exploits from 20 addresses', now() - interval '2 hours', now(), 20, \
              now() + interval '1 day'), \
             (9103, 'ip', '203.0.113.8', 'challenge', '3 failed challenges', \
              now() - interval '2 days', now() - interval '2 days', 3, now() - interval '1 second')",
            vec![],
        )
        .await;
        let service = service(&test_db);

        // Newest evidence first; the expired one is gone; all start pending.
        let all = service
            .list_proposals(ShieldProposalsQuery::default())
            .await
            .unwrap();
        assert_eq!((all.total, all.page, all.page_size), (2, 1, 25));
        let all = all.items;
        assert_eq!(all.iter().map(|p| p.id).collect::<Vec<_>>(), [9102, 9101]);
        assert!(all
            .iter()
            .all(|p| p.status == ShieldProposalStatus::Pending && p.decided_at.is_none()));
        assert_eq!(all[0].kind, ShieldProposalKind::Ja4);
        assert_eq!(all[1].verdict, ShieldEntryVerdict::Block);

        let approved = service
            .decide_proposal(9101, ShieldProposalStatus::Approved, None)
            .await
            .unwrap();
        assert_eq!(approved.status, ShieldProposalStatus::Approved);
        assert!(approved.decided_at.is_some());
        let pending = service
            .list_proposals(with_status(ShieldProposalStatus::Pending))
            .await
            .unwrap()
            .items;
        assert_eq!(pending.iter().map(|p| p.id).collect::<Vec<_>>(), [9102]);

        // An approval can be withdrawn.
        service
            .decide_proposal(9101, ShieldProposalStatus::Rejected, None)
            .await
            .unwrap();
        let rejected = service
            .list_proposals(with_status(ShieldProposalStatus::Rejected))
            .await
            .unwrap()
            .items;
        assert_eq!(rejected.iter().map(|p| p.id).collect::<Vec<_>>(), [9101]);

        for missing in [9103, 9999] {
            assert!(matches!(
                service
                    .decide_proposal(missing, ShieldProposalStatus::Approved, None)
                    .await,
                Err(ShieldConsoleError::ProposalNotFound { proposal_id }) if proposal_id == missing
            ));
        }
        assert!(matches!(
            service
                .decide_proposal(9102, ShieldProposalStatus::Pending, None)
                .await,
            Err(ShieldConsoleError::Validation { .. })
        ));
    }

    fn with_status(status: ShieldProposalStatus) -> ShieldProposalsQuery {
        ShieldProposalsQuery {
            status: Some(status),
            ..ShieldProposalsQuery::default()
        }
    }

    /// More proposals than one page: each page holds the next newest, the
    /// total counts them all, and the filters, the search and the page
    /// size apply together. A search is plain text, not a pattern.
    #[tokio::test]
    async fn proposals_page_newest_first_and_search_by_subject() {
        let Some(test_db) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        // 130 addresses, 198.51.100.1 seen most recently; every tenth rejected.
        exec(
            &db,
            "INSERT INTO shield_observed (kind, subject, verdict, reason, first_seen, last_seen, \
             hits, expires_at, status) \
             SELECT 'ip', '198.51.100.' || n, 'block', 'exploit CVE-2021-41773', \
                 now() - interval '1 day', now() - n * interval '1 minute', 1, \
                 now() + interval '7 days', \
                 CASE WHEN n % 10 = 0 THEN 'rejected' ELSE 'pending' END \
             FROM generate_series(1, 130) AS n",
            vec![],
        )
        .await;
        let service = service(&test_db);
        let subjects = |page: &ShieldProposalPage| {
            page.items
                .iter()
                .map(|p| p.subject.clone())
                .collect::<Vec<_>>()
        };
        let query = |page: Option<u64>, page_size: Option<u64>| ShieldProposalsQuery {
            page,
            page_size,
            ..ShieldProposalsQuery::default()
        };

        let first = service.list_proposals(query(None, None)).await.unwrap();
        assert_eq!((first.total, first.page, first.page_size), (130, 1, 25));
        assert_eq!(first.items.len(), 25);
        assert_eq!(first.items[0].subject, "198.51.100.1");
        assert!(first
            .items
            .windows(2)
            .all(|pair| pair[0].last_seen >= pair[1].last_seen));

        let second = service.list_proposals(query(Some(2), None)).await.unwrap();
        assert_eq!(second.items[0].subject, "198.51.100.26");
        let last = service.list_proposals(query(Some(6), None)).await.unwrap();
        assert_eq!(subjects(&last).len(), 5);
        assert_eq!(
            subjects(&last).last().map(String::as_str),
            Some("198.51.100.130")
        );
        let past = service.list_proposals(query(Some(9), None)).await.unwrap();
        assert!(past.items.is_empty());
        assert_eq!(past.total, 130);

        // Every proposal turns up exactly once across the pages.
        let mut seen = Vec::new();
        for page in 1..=2 {
            let page = service
                .list_proposals(query(Some(page), Some(100)))
                .await
                .unwrap();
            seen.extend(subjects(&page));
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 130);

        // The page size is bounded; page 0 reads as the first.
        let clamped = service
            .list_proposals(query(Some(0), Some(10_000)))
            .await
            .unwrap();
        assert_eq!(
            (clamped.page, clamped.page_size, clamped.items.len()),
            (1, 100, 100)
        );

        let rejected = service
            .list_proposals(with_status(ShieldProposalStatus::Rejected))
            .await
            .unwrap();
        assert_eq!(rejected.total, 13);
        assert_eq!(rejected.items[0].subject, "198.51.100.10");

        let search = |q: &str, status: Option<ShieldProposalStatus>| ShieldProposalsQuery {
            q: Some(q.to_string()),
            status,
            ..ShieldProposalsQuery::default()
        };
        // 198.51.100.12 and 120 to 129, newest first.
        let found = service
            .list_proposals(search(" 100.12 ", None))
            .await
            .unwrap();
        assert_eq!(found.total, 11);
        assert_eq!(found.items[0].subject, "198.51.100.12");
        let found = service
            .list_proposals(search("100.12", Some(ShieldProposalStatus::Rejected)))
            .await
            .unwrap();
        assert_eq!(subjects(&found), ["198.51.100.120"]);
        for pattern in ["%", "_", "198.51.100.1_"] {
            assert_eq!(
                service
                    .list_proposals(search(pattern, None))
                    .await
                    .unwrap()
                    .total,
                0,
                "{pattern}"
            );
        }
        assert!(matches!(
            service.list_proposals(search(&"a".repeat(65), None)).await,
            Err(ShieldConsoleError::Validation { .. })
        ));
    }

    #[test]
    fn windows_parse_from_their_short_names() {
        for (text, window, hours) in [
            ("1h", ShieldWindow::OneHour, 1),
            ("24h", ShieldWindow::OneDay, 24),
            ("7d", ShieldWindow::SevenDays, 168),
        ] {
            let parsed: ShieldWindow = serde_json::from_value(text.into()).unwrap();
            assert_eq!(parsed, window);
            assert_eq!(window.duration().num_hours(), hours);
        }
        assert!(serde_json::from_value::<ShieldWindow>("30d".into()).is_err());
    }
}
