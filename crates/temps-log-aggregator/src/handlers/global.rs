// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-project log search and facets.
//!
//! Two things that used to live here are gone:
//!
//! - **The two-slot semaphore.** `Semaphore::const_new(2)` meant the third
//!   concurrent search *anywhere in the cluster* got a 429 — three engineers
//!   looking at one incident together handed one of them an error. It was the
//!   honest consequence of a design where one request could pull 64 MB from
//!   object storage and zstd-decompress up to 512 whole chunks on the control
//!   plane. Reads now plan over a manifest index and fetch only the blocks a
//!   page needs (ADR-046 §3), so bounding belongs per-request (the store's own
//!   time/byte budget), not per-cluster.
//! - **The deny-list.** Hidden projects used to be inlined into search SQL as
//!   `NOT (p.id = ANY($hidden))`, which fails open on an empty/mis-computed
//!   set. `resolve_log_access_scope` resolves an explicit allow-list once, in
//!   Postgres, and refuses the request outright if it cannot (ADR-045 §7).

use crate::{
    chunk::wal::DEFERRED_DIR,
    error::LogAggregatorError,
    handlers::types::LogAggregatorAppState,
    index::analytics::{AggregateRow, AttrOp, AttrPredicate, GroupKey, HistogramBucket, Metric},
    services::global_search::{
        owner_key, GlobalLogFacetsRequest, GlobalLogFacetsResponse, GlobalLogSearchRequest,
        GlobalLogSearchResponse, GlobalLogSource,
    },
    services::search::to_search_line,
    services::{CollectionStatus, RecoveryState},
    store::{
        encode_cursor, resolve_log_access_scope, FacetField, FacetValue, LogAccessScope, LogQuery,
    },
    types::LogLevel,
};
use axum::{extract::State, http::StatusCode, Json};
// `axum_extra`'s Query deserialises repeated keys (`levels=a&levels=b`,
// `attr=…&attr=…`) into `Vec`s; axum's own `Query` (serde_urlencoded) cannot.
use axum_extra::extract::Query;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};

/// Backstop deadline. The store's own per-request time/byte budget is tighter
/// and returns a specific, actionable "partial results" answer first; this
/// only catches something pathological outside the query itself.
const REQUEST_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(30);

/// Resolve the caller's authorization allow-list, or refuse the request.
///
/// A resolution failure is **always** a refusal. It is never downgraded to an
/// unrestricted query (which would leak every project's logs) and never to a
/// silent empty result (which would look like "no logs" and send the operator
/// hunting for a problem that does not exist).
async fn scope_for(
    auth: &AuthContext,
    state: &LogAggregatorAppState,
) -> Result<LogAccessScope, Problem> {
    resolve_log_access_scope(
        state.db.as_ref(),
        auth,
        state.project_access_checker.as_ref(),
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "Could not resolve global log access — refusing the request");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Could not resolve log access")
            .with_detail(
                "Your log permissions could not be determined, so no logs were returned. \
                 Retry shortly; if this persists, check the server logs.",
            )
    })
}

#[utoipa::path(post, path="/logs/global/search", tag="Logs", request_body=GlobalLogSearchRequest,
    responses((status=200,body=GlobalLogSearchResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=408,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub async fn search_global_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Json(request): Json<GlobalLogSearchRequest>,
) -> Result<Json<GlobalLogSearchResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;
    let attrs = parse_attr_predicates(&request.attrs)?;

    let result = tokio::time::timeout(
        REQUEST_BACKSTOP,
        search_global_with_attrs(&state, &request, &scope, &attrs),
    )
    .await
    .map_err(|_| {
        problemdetails::new(StatusCode::REQUEST_TIMEOUT)
            .with_title("Log search exceeded its time budget")
            .with_detail("Shorten the time window or narrow the selected sources.")
    })??;
    Ok(Json(result))
}

/// Upper bound on the chunks the index may nominate for a text+attribute
/// search. Past it the restriction is dropped (every chunk in the window is
/// scanned, predicates still enforced per line) rather than silently
/// truncated.
const MAX_NOMINATED_CHUNKS: u32 = 20_000;

/// `search_global`, extended with attribute predicates (ADR-047 §5).
///
/// Without `attrs` this is exactly the unmodified chunk-store path. With
/// `attrs` and no `text`, the line index answers directly: it returns
/// pointers into the chunks, resolved to full records one block per hit.
/// With `attrs` *and* `text`, the index holds no message bytes, so it is
/// asked only *which chunks* contain attribute matches; the regular
/// bloom-pruned chunk scan then runs over just those chunks (plus the
/// unsealed head buffers, which no index has seen yet) with the predicates
/// enforced per line from the same `fields` the index was built from. That
/// keeps the chunk scan's time/byte budget, honest `partial` pages and
/// cursor semantics.
async fn search_global_with_attrs(
    state: &LogAggregatorAppState,
    request: &GlobalLogSearchRequest,
    scope: &LogAccessScope,
    attrs: &[AttrPredicate],
) -> Result<GlobalLogSearchResponse, LogAggregatorError> {
    if attrs.is_empty() {
        return state.search_service.search_global(request, scope).await;
    }

    let (mut query, scope_hash) = state.search_service.build_query(request, scope).await?;

    if query.text.is_some() {
        let seqs = state
            .line_index
            .matching_chunks(&query, attrs, MAX_NOMINATED_CHUNKS)
            .await?;
        if (seqs.len() as u32) < MAX_NOMINATED_CHUNKS {
            query.chunk_seqs = Some(seqs);
        }
        query.attrs = attrs.to_vec();
        return state
            .search_service
            .search_with_query(query, scope, &scope_hash)
            .await;
    }

    let pointers = state.line_index.search_pointers(&query, attrs).await?;
    let full_page = pointers.len() as u32 >= query.limit;
    let positions: Vec<(i64, u32)> = pointers
        .iter()
        .map(|p| (p.chunk_seq, p.line_index))
        .collect();
    let records = state
        .search_service
        .store
        .lines_by_position(scope, &positions)
        .await?;
    let next_cursor = if full_page {
        records
            .last()
            .map(|r| encode_cursor(&r.key(), &scope_hash))
            .transpose()?
    } else {
        None
    };

    let owners = state.search_service.resolve_owner_names(&records).await?;
    let lines = records
        .iter()
        .map(|record| crate::services::global_search::GlobalLogLine {
            owner: owners.get(&owner_key(record)).cloned().unwrap_or_default(),
            project_id: record.project_id,
            external_service_id: record.external_service_id,
            env: record.env.clone(),
            line: to_search_line(record),
        })
        .collect();

    Ok(GlobalLogSearchResponse {
        lines,
        next_cursor,
        // Every pointer the index returned was resolved (or belongs to a
        // chunk that is gone); there is no store budget to run out of.
        partial: false,
        scanned_back_to: None,
    })
}

#[utoipa::path(post, path="/logs/global/facets", tag="Logs", request_body=GlobalLogFacetsRequest,
    responses((status=200,body=GlobalLogFacetsResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=408,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub async fn facet_global_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Json(request): Json<GlobalLogFacetsRequest>,
) -> Result<Json<GlobalLogFacetsResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;

    let result = tokio::time::timeout(
        REQUEST_BACKSTOP,
        state.search_service.facets_global(&request, &scope),
    )
    .await
    .map_err(|_| {
        problemdetails::new(StatusCode::REQUEST_TIMEOUT)
            .with_title("Facet query exceeded its time budget")
            .with_detail("Shorten the time window or narrow the selected sources.")
    })??;
    Ok(Json(result))
}

// ── Capabilities (ADR-047) ──────────────────────────────────────────────

/// Settings page that configures the ClickHouse connection.
pub const ANALYTICS_SETUP_PATH: &str = "/settings/metrics-monitoring";

/// What the log explorer can offer on this instance. Attribute facets,
/// histograms and `GROUP BY` analytics need the ClickHouse line index; when
/// it is missing the client shows *why* and where to configure it instead
/// of hiding the feature (CLAUDE.md: unconfigured features onboard).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct GlobalLogCapabilities {
    pub analytics: AnalyticsCapability,
    pub collection: LogCollectionCapability,
}

/// How many deferred generations the response itemizes. The counts cover
/// all of them; the list is for the operator to recognise what they are.
const DEFERRED_GENERATIONS_LISTED: usize = 20;

/// What a non-administrator is told when the deferred directory is unreadable.
const DEFERRED_LISTING_FAILED: &str =
    "The server could not read its deferred WAL directory, so files set aside by recovery are \
     not counted.";

/// Whether new container log lines are being collected right now. Separate
/// from `analytics` because the two fail independently: after a restart the
/// index can be healthy while collection waits on WAL recovery, and without
/// this the explorer shows a histogram that simply stops.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct LogCollectionCapability {
    pub state: LogCollectionState,
    /// `true` only in the `running` state.
    pub collecting: bool,
    /// When the current state began: recovery start for `recovering`,
    /// recovery end for `running`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub since: Option<DateTime<Utc>>,
    /// Why collection is paused, verbatim, for `retrying` and `stopped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the next recovery pass starts, for `retrying`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub retry_at: Option<DateTime<Utc>>,
    /// Generations recovery set aside because it could not replay them in
    /// full. Collection runs regardless; their lines are missing from search
    /// until an operator retries them.
    pub deferred_count: u64,
    pub deferred_bytes: u64,
    /// The oldest few deferred generations.
    pub deferred: Vec<DeferredWalGeneration>,
    /// Directory holding them, on the server's filesystem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_dir: Option<String>,
    /// Set when that directory could not be read: the deferred counts and
    /// list are then empty because they are unknown, not because nothing
    /// was deferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_error: Option<String>,
    /// `false` when the caller is not an instance administrator: `error`,
    /// `deferred_dir` and each generation's `reason` are then omitted, and
    /// `deferred_error` is generic, since they carry server filesystem paths
    /// and raw I/O errors. State and
    /// counts are always present, so a paused collector is never hidden.
    pub details_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogCollectionState {
    /// Collecting.
    Running,
    /// Replaying the WAL from before the last restart; collection starts
    /// when it finishes.
    Recovering,
    /// A recovery pass failed; paused until the retry at `retry_at` works.
    Retrying,
    /// Recovery gave up; paused until temps is restarted.
    Stopped,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct DeferredWalGeneration {
    pub file_name: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub deferred_at: Option<DateTime<Utc>>,
    /// What recovery could not read and what it did with the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl LogCollectionCapability {
    /// What a non-administrator may see: the state and the counts, not the
    /// paths and error text that only someone with a shell could act on.
    fn without_server_details(mut self) -> Self {
        self.details_visible = false;
        self.error = None;
        self.deferred_dir = None;
        if self.deferred_error.is_some() {
            self.deferred_error = Some(DEFERRED_LISTING_FAILED.to_string());
        }
        for generation in &mut self.deferred {
            generation.reason = None;
        }
        self
    }
}

impl From<CollectionStatus> for LogCollectionCapability {
    fn from(status: CollectionStatus) -> Self {
        let (state, since, error, retry_at) = match status.recovery {
            RecoveryState::Complete { finished_at } => {
                (LogCollectionState::Running, Some(finished_at), None, None)
            }
            RecoveryState::Recovering { started_at } => {
                (LogCollectionState::Recovering, Some(started_at), None, None)
            }
            RecoveryState::Retrying { error, retry_at } => (
                LogCollectionState::Retrying,
                None,
                Some(error),
                Some(retry_at),
            ),
            RecoveryState::Stopped { error } => {
                (LogCollectionState::Stopped, None, Some(error), None)
            }
        };
        Self {
            details_visible: true,
            state,
            collecting: state == LogCollectionState::Running,
            since,
            error,
            retry_at,
            deferred_count: status.deferred.len() as u64,
            deferred_bytes: status.deferred.iter().map(|g| g.bytes).sum(),
            deferred_error: status.deferred_error,
            deferred_dir: status
                .wal_dir
                .map(|dir| dir.join(DEFERRED_DIR).display().to_string()),
            deferred: status
                .deferred
                .into_iter()
                .take(DEFERRED_GENERATIONS_LISTED)
                .map(|generation| DeferredWalGeneration {
                    file_name: generation
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    bytes: generation.bytes,
                    deferred_at: generation.deferred_at,
                    reason: generation.reason,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AnalyticsCapability {
    /// `true` when the line index is active and receiving sealed chunks.
    pub configured: bool,
    /// Where the index lives (`clickhouse`, `temps_cloud` or `timescaledb`)
    /// when configured. The TimescaleDB fallback answers the same endpoints
    /// but scales with line count on the control-plane database; the
    /// onboarding copy points at ClickHouse or Temps Cloud for volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<crate::index::LineIndexBackend>,
    /// Widest window one analytics query answers on this store, in days.
    /// The TimescaleDB store clamps `start_time` to this many days before
    /// `end_time` so a query can never scan the whole control-plane
    /// database; ClickHouse stores are unbounded (`None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_window_days: Option<u32>,
    /// Exactly what is missing, when `configured` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Console route where the operator fixes it.
    pub setup_path: String,
    /// What analytics would do here — shown as the example in the
    /// onboarding state.
    pub example: String,
    /// Live chunks in the manifest and how many of them are indexed. Equal
    /// numbers mean the index is complete; a gap is the reindexer's queue.
    pub live_chunks: u64,
    pub indexed_chunks: u64,
    /// Chunks retired (compacted, purged, or expired by retention) whose
    /// removal from the line index has not yet been confirmed — durably
    /// queued in `log_line_forget_backlog` and drained by `ForgetSweeper`
    /// (ADR-047 §8a). Non-zero for more than a few sweep intervals (30s,
    /// see [`crate::services::FORGET_SWEEP_INTERVAL`]) means the index
    /// still holds rows for chunks that no longer exist — over-counted in
    /// facets/histograms/aggregates until the sweeper catches up.
    pub forget_backlog: u64,
}

#[utoipa::path(get, path="/logs/global/capabilities", tag="Logs",
    responses((status=200,body=GlobalLogCapabilities),(status=403,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub async fn global_log_capabilities(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
) -> Result<Json<GlobalLogCapabilities>, Problem> {
    permission_guard!(auth, LogsRead);
    let reason = state.line_index.unavailable_reason();
    let (live_chunks, indexed_chunks) = state.manifests.index_coverage().await.map_err(|e| {
        tracing::error!(error = %e, "index coverage query failed");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Could not read log index status")
            .with_detail(e.to_string())
    })?;
    let forget_backlog = state.manifests.forget_backlog_size().await.map_err(|e| {
        tracing::error!(error = %e, "forget backlog size query failed");
        problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Could not read log index status")
            .with_detail(e.to_string())
    })?;
    let collection = state
        .chunk_writer
        .collection_status(DEFERRED_GENERATIONS_LISTED)
        .await;
    Ok(Json(GlobalLogCapabilities {
        analytics: AnalyticsCapability {
            configured: reason.is_none(),
            backend: state.line_index.backend(),
            max_window_days: state.line_index.max_window_days(),
            reason,
            setup_path: ANALYTICS_SETUP_PATH.to_string(),
            example: "Group ERROR lines by http_route for the last hour, chart requests slower \
                      than 500 ms per service, or facet on any field your app logs."
                .to_string(),
            live_chunks,
            indexed_chunks,
            forget_backlog,
        },
        collection: if auth.is_instance_admin() {
            collection.into()
        } else {
            LogCollectionCapability::from(collection).without_server_details()
        },
    }))
}

// ── Attribute analytics (ADR-047 §5) ────────────────────────────────────
//
// Read side of the line index: attribute keys, facets, histograms and
// `GROUP BY` aggregates. Every handler here builds the identical scoped
// `LogQuery` the plain search/facets endpoints build (via
// `LogSearchService::build_query`, so name resolution, selector validation
// and the allow-list scope never drift between the two families), then hands
// it plus attribute predicates to `state.line_index`. On an instance without
// ClickHouse configured, `state.line_index` is a `NoLineIndex` and every one
// of these calls fails with `LogAggregatorError::LineIndex`, which
// `log_handler.rs` maps to `503` with the reason — never a silent empty
// answer.

/// The subset of [`GlobalLogSearchRequest`]'s filter fields that make sense
/// as GET query parameters: everything except `text` (the index holds no
/// message bytes), `cursor`/`page_size` (analytics reads are not paginated
/// pages of lines) and `attrs` (each endpoint below takes its own attribute
/// predicates, since some also take group/facet keys over the same syntax).
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GlobalLogFilterQuery {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    #[serde(default)]
    source: GlobalLogSource,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    external_services: Vec<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    levels: Vec<LogLevel>,
    #[serde(default)]
    envs: Vec<String>,
    #[serde(default)]
    services: Vec<String>,
    #[serde(default)]
    container_ids: Vec<String>,
    #[serde(default)]
    node_ids: Vec<i32>,
    deploy_id: Option<i32>,
}

impl From<GlobalLogFilterQuery> for GlobalLogSearchRequest {
    fn from(f: GlobalLogFilterQuery) -> Self {
        GlobalLogSearchRequest {
            start_time: f.start_time,
            end_time: f.end_time,
            source: f.source,
            projects: f.projects,
            external_services: f.external_services,
            scopes: f.scopes,
            levels: f.levels,
            envs: f.envs,
            services: f.services,
            container_ids: f.container_ids,
            node_ids: f.node_ids,
            deploy_id: f.deploy_id,
            text: None,
            cursor: None,
            page_size: None,
            attrs: Vec::new(),
        }
    }
}

/// Build the identical scoped [`LogQuery`] the plain search/facets endpoints
/// build, from a query-string filter.
async fn build_analytics_query(
    state: &LogAggregatorAppState,
    scope: &LogAccessScope,
    filter: GlobalLogFilterQuery,
) -> Result<LogQuery, Problem> {
    let request: GlobalLogSearchRequest = filter.into();
    let (query, _scope_hash) = state.search_service.build_query(&request, scope).await?;
    Ok(query)
}

/// Parse repeatable `attr=<key><op><value>` query parameters (and
/// `attr=<key>?` for "exists") into index predicates.
pub(crate) fn parse_attr_predicates(
    raws: &[String],
) -> Result<Vec<AttrPredicate>, LogAggregatorError> {
    raws.iter().map(|s| parse_attr_predicate(s)).collect()
}

fn attr_filter_error(raw: &str) -> LogAggregatorError {
    LogAggregatorError::Validation {
        message: format!(
            "invalid attribute filter {raw:?}: expected <key><op><value> with op in =, !=, ^= \
             (prefix), >, < or <key>? for \"exists\""
        ),
    }
}

fn parse_attr_predicate(raw: &str) -> Result<AttrPredicate, LogAggregatorError> {
    if let Some(key) = raw.strip_suffix('?') {
        if key.is_empty() {
            return Err(attr_filter_error(raw));
        }
        return Ok(AttrPredicate {
            key: key.to_string(),
            op: AttrOp::Exists,
            value: None,
        });
    }
    // Two-character operators first: `!=`/`^=` both contain `=`, so checking
    // the single-`=` case first would split them in the wrong place.
    for (token, op) in [("!=", AttrOp::Neq), ("^=", AttrOp::Prefix)] {
        if let Some(idx) = raw.find(token) {
            let (key, rest) = (&raw[..idx], &raw[idx + token.len()..]);
            if key.is_empty() {
                return Err(attr_filter_error(raw));
            }
            return Ok(AttrPredicate {
                key: key.to_string(),
                op,
                value: Some(rest.to_string()),
            });
        }
    }
    for (ch, op) in [('=', AttrOp::Eq), ('>', AttrOp::Gt), ('<', AttrOp::Lt)] {
        if let Some(idx) = raw.find(ch) {
            let (key, rest) = (&raw[..idx], &raw[idx + 1..]);
            if key.is_empty() {
                return Err(attr_filter_error(raw));
            }
            return Ok(AttrPredicate {
                key: key.to_string(),
                op,
                value: Some(rest.to_string()),
            });
        }
    }
    Err(attr_filter_error(raw))
}

/// Parse one `group_by`/`keys` item: a label facet name (`env`, `service`,
/// `level`, `stream`, `project`, `external_service`, `node`, `deploy`,
/// `container`) or `attr:<name>` for an attribute.
fn parse_group_key(raw: &str) -> Result<GroupKey, LogAggregatorError> {
    let raw = raw.trim();
    if let Some(attr) = raw.strip_prefix("attr:") {
        if attr.is_empty() {
            return Err(LogAggregatorError::Validation {
                message: "attr: group key needs a name, e.g. attr:worker".to_string(),
            });
        }
        return Ok(GroupKey::Attr(attr.to_string()));
    }
    let field: FacetField = serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|_| LogAggregatorError::Validation {
            message: format!(
                "unknown group key {raw:?}; expected one of env, service, level, stream, \
                 project, external_service, node, deploy, container, or attr:<name>"
            ),
        })?;
    Ok(GroupKey::Label(field))
}

/// Parse a comma-separated list of group keys.
fn parse_group_keys(raw: &str) -> Result<Vec<GroupKey>, LogAggregatorError> {
    let keys: Result<Vec<GroupKey>, LogAggregatorError> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_group_key)
        .collect();
    let keys = keys?;
    if keys.is_empty() {
        return Err(LogAggregatorError::Validation {
            message: "at least one group-by key is required".to_string(),
        });
    }
    Ok(keys)
}

/// Parse a `metric` parameter: `count`, or `<fn>:<attr>` for
/// `count_distinct`, `avg`, `p50`, `p95`, `p99`, `max`, `sum`.
fn parse_metric(raw: &str) -> Result<Metric, LogAggregatorError> {
    let invalid = || LogAggregatorError::Validation {
        message: format!(
            "invalid metric {raw:?}; expected count, or one of count_distinct, avg, p50, p95, \
             p99, max, sum followed by :<attr>"
        ),
    };
    if raw == "count" {
        return Ok(Metric::Count);
    }
    let (name, attr) = raw.split_once(':').ok_or_else(invalid)?;
    if attr.is_empty() {
        return Err(invalid());
    }
    Ok(match name {
        "count_distinct" => Metric::CountDistinct(attr.to_string()),
        "avg" => Metric::Avg(attr.to_string()),
        "p50" => Metric::P50(attr.to_string()),
        "p95" => Metric::P95(attr.to_string()),
        "p99" => Metric::P99(attr.to_string()),
        "max" => Metric::Max(attr.to_string()),
        "sum" => Metric::Sum(attr.to_string()),
        _ => return Err(invalid()),
    })
}

/// Upper bound on any of these endpoints' `limit`/`max_groups`, regardless of
/// what the caller asks for.
const MAX_ANALYTICS_LIMIT: u32 = 1000;

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AttributeKeysResponse {
    pub keys: Vec<FacetValue>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct AttributeKeysExtra {
    limit: Option<u32>,
}

#[utoipa::path(get, path="/logs/global/attributes", tag="Logs",
    params(
        ("start_time" = String, Query, description = "Window start (RFC 3339)"),
        ("end_time" = String, Query, description = "Window end (RFC 3339)"),
        ("source" = Option<GlobalLogSource>, Query, description = "Source kind (default: all)"),
        ("projects" = Option<Vec<String>>, Query, description = "Project selectors (repeatable)"),
        ("external_services" = Option<Vec<String>>, Query, description = "External service selectors (repeatable)"),
        ("scopes" = Option<Vec<String>>, Query, description = "Scope selectors (repeatable)"),
        ("levels" = Option<Vec<LogLevel>>, Query, description = "Levels (repeatable)"),
        ("envs" = Option<Vec<String>>, Query, description = "Environments (repeatable)"),
        ("services" = Option<Vec<String>>, Query, description = "Services (repeatable)"),
        ("container_ids" = Option<Vec<String>>, Query, description = "Container ids (repeatable)"),
        ("node_ids" = Option<Vec<i32>>, Query, description = "Node ids (repeatable)"),
        ("deploy_id" = Option<i32>, Query, description = "Deployment id"),
        ("limit" = Option<u32>, Query, description = "Max keys returned (default/cap 1000)"),
    ),
    responses((status=200,body=AttributeKeysResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=503,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub(crate) async fn global_log_attribute_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Query(filter): Query<GlobalLogFilterQuery>,
    Query(extra): Query<AttributeKeysExtra>,
) -> Result<Json<AttributeKeysResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;
    let query = build_analytics_query(&state, &scope, filter).await?;
    let limit = extra.limit.unwrap_or(100).clamp(1, MAX_ANALYTICS_LIMIT);
    let keys = state.line_index.attribute_keys(&query, limit).await?;
    Ok(Json(AttributeKeysResponse { keys }))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct FacetsAttrsResponse {
    #[schema(value_type = Object)]
    pub facets: BTreeMap<String, Vec<FacetValue>>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct FacetsAttrsExtra {
    keys: String,
    #[serde(default)]
    attr: Vec<String>,
    limit: Option<u32>,
}

#[utoipa::path(get, path="/logs/global/facets/attrs", tag="Logs",
    params(
        ("start_time" = String, Query, description = "Window start (RFC 3339)"),
        ("end_time" = String, Query, description = "Window end (RFC 3339)"),
        ("source" = Option<GlobalLogSource>, Query, description = "Source kind (default: all)"),
        ("projects" = Option<Vec<String>>, Query, description = "Project selectors (repeatable)"),
        ("external_services" = Option<Vec<String>>, Query, description = "External service selectors (repeatable)"),
        ("scopes" = Option<Vec<String>>, Query, description = "Scope selectors (repeatable)"),
        ("levels" = Option<Vec<LogLevel>>, Query, description = "Levels (repeatable)"),
        ("envs" = Option<Vec<String>>, Query, description = "Environments (repeatable)"),
        ("services" = Option<Vec<String>>, Query, description = "Services (repeatable)"),
        ("container_ids" = Option<Vec<String>>, Query, description = "Container ids (repeatable)"),
        ("node_ids" = Option<Vec<i32>>, Query, description = "Node ids (repeatable)"),
        ("deploy_id" = Option<i32>, Query, description = "Deployment id"),
        ("keys" = String, Query, description = "Comma-separated label names or attr:<name>"),
        ("attr" = Vec<String>, Query, description = "Repeatable attribute predicate: <key><op><value> or <key>?"),
        ("limit" = Option<u32>, Query, description = "Max values per key (default/cap 1000)"),
    ),
    responses((status=200,body=FacetsAttrsResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=503,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub(crate) async fn global_log_facets_attrs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Query(filter): Query<GlobalLogFilterQuery>,
    Query(extra): Query<FacetsAttrsExtra>,
) -> Result<Json<FacetsAttrsResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;
    let query = build_analytics_query(&state, &scope, filter).await?;
    let keys = parse_group_keys(&extra.keys)?;
    let attrs = parse_attr_predicates(&extra.attr)?;
    let limit = extra.limit.unwrap_or(50).clamp(1, MAX_ANALYTICS_LIMIT);
    let facets = state
        .line_index
        .facets(&query, &attrs, &keys, limit)
        .await?;
    Ok(Json(FacetsAttrsResponse { facets }))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct HistogramResponse {
    pub buckets: Vec<HistogramBucket>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct HistogramExtra {
    bucket_secs: Option<u32>,
    group_by: Option<String>,
    max_groups: Option<u32>,
    #[serde(default)]
    attr: Vec<String>,
}

#[utoipa::path(get, path="/logs/global/histogram", tag="Logs",
    params(
        ("start_time" = String, Query, description = "Window start (RFC 3339)"),
        ("end_time" = String, Query, description = "Window end (RFC 3339)"),
        ("source" = Option<GlobalLogSource>, Query, description = "Source kind (default: all)"),
        ("projects" = Option<Vec<String>>, Query, description = "Project selectors (repeatable)"),
        ("external_services" = Option<Vec<String>>, Query, description = "External service selectors (repeatable)"),
        ("scopes" = Option<Vec<String>>, Query, description = "Scope selectors (repeatable)"),
        ("levels" = Option<Vec<LogLevel>>, Query, description = "Levels (repeatable)"),
        ("envs" = Option<Vec<String>>, Query, description = "Environments (repeatable)"),
        ("services" = Option<Vec<String>>, Query, description = "Services (repeatable)"),
        ("container_ids" = Option<Vec<String>>, Query, description = "Container ids (repeatable)"),
        ("node_ids" = Option<Vec<i32>>, Query, description = "Node ids (repeatable)"),
        ("deploy_id" = Option<i32>, Query, description = "Deployment id"),
        ("bucket_secs" = Option<u32>, Query, description = "Bucket width in seconds (default 60)"),
        ("group_by" = Option<String>, Query, description = "One label name or attr:<name> to split series by"),
        ("max_groups" = Option<u32>, Query, description = "Max series before folding the rest into \"other\" (default 8)"),
        ("attr" = Vec<String>, Query, description = "Repeatable attribute predicate: <key><op><value> or <key>?"),
    ),
    responses((status=200,body=HistogramResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=503,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub(crate) async fn global_log_histogram(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Query(filter): Query<GlobalLogFilterQuery>,
    Query(extra): Query<HistogramExtra>,
) -> Result<Json<HistogramResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;
    let query = build_analytics_query(&state, &scope, filter).await?;
    let attrs = parse_attr_predicates(&extra.attr)?;
    let group_by = extra.group_by.as_deref().map(parse_group_key).transpose()?;
    let bucket_secs = extra.bucket_secs.unwrap_or(60).clamp(1, 86_400 * 7);
    let max_groups = extra.max_groups.unwrap_or(8).clamp(1, 50);
    let buckets = state
        .line_index
        .histogram(&query, &attrs, bucket_secs, group_by.as_ref(), max_groups)
        .await?;
    Ok(Json(HistogramResponse { buckets }))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AggregateResponse {
    pub rows: Vec<AggregateRow>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct AggregateExtra {
    group_by: String,
    metric: String,
    limit: Option<u32>,
    #[serde(default)]
    attr: Vec<String>,
}

#[utoipa::path(get, path="/logs/global/aggregate", tag="Logs",
    params(
        ("start_time" = String, Query, description = "Window start (RFC 3339)"),
        ("end_time" = String, Query, description = "Window end (RFC 3339)"),
        ("source" = Option<GlobalLogSource>, Query, description = "Source kind (default: all)"),
        ("projects" = Option<Vec<String>>, Query, description = "Project selectors (repeatable)"),
        ("external_services" = Option<Vec<String>>, Query, description = "External service selectors (repeatable)"),
        ("scopes" = Option<Vec<String>>, Query, description = "Scope selectors (repeatable)"),
        ("levels" = Option<Vec<LogLevel>>, Query, description = "Levels (repeatable)"),
        ("envs" = Option<Vec<String>>, Query, description = "Environments (repeatable)"),
        ("services" = Option<Vec<String>>, Query, description = "Services (repeatable)"),
        ("container_ids" = Option<Vec<String>>, Query, description = "Container ids (repeatable)"),
        ("node_ids" = Option<Vec<i32>>, Query, description = "Node ids (repeatable)"),
        ("deploy_id" = Option<i32>, Query, description = "Deployment id"),
        ("group_by" = String, Query, description = "Comma-separated label names and/or attr:<name>"),
        ("metric" = String, Query, description = "count | count_distinct:<k> | avg:<k> | p50:<k> | p95:<k> | p99:<k> | max:<k> | sum:<k>"),
        ("limit" = Option<u32>, Query, description = "Max rows returned (default 50, cap 1000)"),
        ("attr" = Vec<String>, Query, description = "Repeatable attribute predicate: <key><op><value> or <key>?"),
    ),
    responses((status=200,body=AggregateResponse),(status=400,body=ProblemDetails),(status=403,body=ProblemDetails),(status=503,body=ProblemDetails)),
    security(("bearer_auth"=[])))]
pub(crate) async fn global_log_aggregate(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<LogAggregatorAppState>>,
    Query(filter): Query<GlobalLogFilterQuery>,
    Query(extra): Query<AggregateExtra>,
) -> Result<Json<AggregateResponse>, Problem> {
    permission_guard!(auth, LogsRead);
    let scope = scope_for(&auth, &state).await?;
    let query = build_analytics_query(&state, &scope, filter).await?;
    let group_by = parse_group_keys(&extra.group_by)?;
    let metric = parse_metric(&extra.metric)?;
    let attrs = parse_attr_predicates(&extra.attr)?;
    let limit = extra.limit.unwrap_or(50).clamp(1, MAX_ANALYTICS_LIMIT);
    let rows = state
        .line_index
        .aggregate(&query, &attrs, &group_by, &metric, limit)
        .await?;
    Ok(Json(AggregateResponse { rows }))
}

#[cfg(test)]
mod attr_parser_tests {
    use super::*;

    #[test]
    fn parses_all_operators() {
        assert_eq!(
            parse_attr_predicate("status_code=200").unwrap(),
            AttrPredicate {
                key: "status_code".into(),
                op: AttrOp::Eq,
                value: Some("200".into())
            }
        );
        assert_eq!(
            parse_attr_predicate("status_code!=200").unwrap(),
            AttrPredicate {
                key: "status_code".into(),
                op: AttrOp::Neq,
                value: Some("200".into())
            }
        );
        assert_eq!(
            parse_attr_predicate("http_route^=/api/").unwrap(),
            AttrPredicate {
                key: "http_route".into(),
                op: AttrOp::Prefix,
                value: Some("/api/".into())
            }
        );
        assert_eq!(
            parse_attr_predicate("duration_ms>500").unwrap(),
            AttrPredicate {
                key: "duration_ms".into(),
                op: AttrOp::Gt,
                value: Some("500".into())
            }
        );
        assert_eq!(
            parse_attr_predicate("duration_ms<10").unwrap(),
            AttrPredicate {
                key: "duration_ms".into(),
                op: AttrOp::Lt,
                value: Some("10".into())
            }
        );
        assert_eq!(
            parse_attr_predicate("worker?").unwrap(),
            AttrPredicate {
                key: "worker".into(),
                op: AttrOp::Exists,
                value: None
            }
        );
    }

    #[test]
    fn rejects_malformed_predicates() {
        for bad in [
            "",
            "?",
            "=value",
            "!=value",
            "^=value",
            ">5",
            "no-operator-here",
        ] {
            assert!(
                parse_attr_predicate(bad).is_err(),
                "expected error for {bad:?}"
            );
        }
    }

    #[test]
    fn parses_a_list_and_stops_at_the_first_error() {
        let good = vec!["a=1".to_string(), "b!=2".to_string()];
        assert_eq!(parse_attr_predicates(&good).unwrap().len(), 2);

        let bad = vec!["a=1".to_string(), "bad".to_string()];
        assert!(parse_attr_predicates(&bad).is_err());
    }

    #[test]
    fn group_key_parses_labels_and_attrs() {
        assert_eq!(
            parse_group_key("env").unwrap(),
            GroupKey::Label(FacetField::Env)
        );
        assert_eq!(
            parse_group_key("external_service").unwrap(),
            GroupKey::Label(FacetField::ExternalService)
        );
        assert_eq!(
            parse_group_key("attr:worker").unwrap(),
            GroupKey::Attr("worker".into())
        );
        assert!(parse_group_key("attr:").is_err());
        assert!(parse_group_key("not-a-field").is_err());
    }

    #[test]
    fn group_keys_parses_comma_separated_list_and_rejects_empty() {
        let keys = parse_group_keys("env, attr:worker,service").unwrap();
        assert_eq!(
            keys,
            vec![
                GroupKey::Label(FacetField::Env),
                GroupKey::Attr("worker".into()),
                GroupKey::Label(FacetField::Service),
            ]
        );
        assert!(parse_group_keys("").is_err());
        assert!(parse_group_keys(" , ").is_err());
    }

    #[test]
    fn metric_parses_count_and_functions() {
        assert_eq!(parse_metric("count").unwrap(), Metric::Count);
        assert_eq!(
            parse_metric("p95:duration_ms").unwrap(),
            Metric::P95("duration_ms".into())
        );
        assert_eq!(
            parse_metric("count_distinct:worker").unwrap(),
            Metric::CountDistinct("worker".into())
        );
        assert!(parse_metric("bogus:key").is_err());
        assert!(parse_metric("avg:").is_err());
        assert!(parse_metric("avg").is_err());
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;

    fn deferred(index: usize) -> crate::chunk::wal::DeferredGeneration {
        crate::chunk::wal::DeferredGeneration {
            path: std::path::PathBuf::from(format!(
                "/data/logs/wal/deferred/g{index}.b.sealed-wal"
            )),
            bytes: 10,
            deferred_at: None,
            reason: Some(format!("reason {index}")),
        }
    }

    #[test]
    fn collection_reports_paused_recovery_with_its_error_and_retry_time() {
        let retry_at = Utc::now();
        let capability = LogCollectionCapability::from(CollectionStatus {
            recovery: RecoveryState::Retrying {
                error: "object storage unreachable".into(),
                retry_at,
            },
            wal_dir: Some("/data/logs/wal".into()),
            deferred: Vec::new(),
            deferred_error: None,
        });
        assert_eq!(capability.state, LogCollectionState::Retrying);
        assert!(!capability.collecting);
        assert_eq!(
            capability.error.as_deref(),
            Some("object storage unreachable")
        );
        assert_eq!(capability.retry_at, Some(retry_at));
        assert_eq!(
            capability.deferred_dir.as_deref(),
            Some("/data/logs/wal/deferred")
        );
    }

    #[test]
    fn collection_counts_every_deferred_generation_but_lists_a_bounded_few() {
        let capability = LogCollectionCapability::from(CollectionStatus {
            recovery: RecoveryState::Complete {
                finished_at: Utc::now(),
            },
            wal_dir: Some("/data/logs/wal".into()),
            deferred: (0..DEFERRED_GENERATIONS_LISTED + 5).map(deferred).collect(),
            deferred_error: None,
        });
        assert_eq!(capability.state, LogCollectionState::Running);
        assert!(
            capability.collecting,
            "deferred files never pause collection"
        );
        assert_eq!(
            capability.deferred_count,
            (DEFERRED_GENERATIONS_LISTED + 5) as u64
        );
        assert_eq!(
            capability.deferred_bytes,
            10 * (DEFERRED_GENERATIONS_LISTED + 5) as u64
        );
        assert_eq!(capability.deferred.len(), DEFERRED_GENERATIONS_LISTED);
        assert_eq!(capability.deferred[0].file_name, "g0.b.sealed-wal");
        assert_eq!(capability.deferred[0].reason.as_deref(), Some("reason 0"));
    }

    #[test]
    fn non_administrators_see_state_and_counts_but_no_server_paths_or_errors() {
        let capability = LogCollectionCapability::from(CollectionStatus {
            recovery: RecoveryState::Stopped {
                error: "IO error reading /srv/temps/logs/wal/x.sealed-wal".into(),
            },
            wal_dir: Some("/srv/temps/logs/wal".into()),
            deferred: vec![deferred(0)],
            deferred_error: None,
        })
        .without_server_details();
        assert!(!capability.details_visible);
        assert_eq!(capability.state, LogCollectionState::Stopped);
        assert!(!capability.collecting, "a paused collector is never hidden");
        assert_eq!(capability.deferred_count, 1);
        assert!(capability.error.is_none());
        assert!(capability.deferred_dir.is_none());
        assert!(capability.deferred[0].reason.is_none());
        let json = serde_json::to_string(&capability).unwrap();
        assert!(!json.contains("/srv/temps"), "{json}");
    }

    #[test]
    fn an_unreadable_deferred_directory_is_reported_without_hiding_the_state() {
        let status = CollectionStatus {
            recovery: RecoveryState::Retrying {
                error: "object storage unreachable".into(),
                retry_at: Utc::now(),
            },
            wal_dir: Some("/srv/temps/logs/wal".into()),
            deferred: Vec::new(),
            deferred_error: Some(
                "IO error: Permission denied reading /srv/temps/logs/wal/deferred".into(),
            ),
        };
        let admin = LogCollectionCapability::from(status.clone());
        assert_eq!(admin.state, LogCollectionState::Retrying);
        assert!(admin
            .deferred_error
            .as_deref()
            .is_some_and(|error| error.contains("Permission denied")));

        let reader = LogCollectionCapability::from(status).without_server_details();
        assert_eq!(reader.state, LogCollectionState::Retrying);
        assert_eq!(
            reader.deferred_error.as_deref(),
            Some(DEFERRED_LISTING_FAILED)
        );
        let json = serde_json::to_string(&reader).unwrap();
        assert!(!json.contains("/srv/temps"), "{json}");
    }

    #[test]
    fn collection_without_a_wal_is_running_with_nothing_deferred() {
        let capability = LogCollectionCapability::from(CollectionStatus {
            recovery: RecoveryState::Complete {
                finished_at: Utc::now(),
            },
            wal_dir: None,
            deferred: Vec::new(),
            deferred_error: None,
        });
        assert!(capability.collecting);
        assert_eq!(capability.deferred_count, 0);
        assert!(capability.deferred_dir.is_none());
    }
}
