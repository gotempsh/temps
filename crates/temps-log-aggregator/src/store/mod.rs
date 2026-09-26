// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backend-neutral, ordered store for container log lines (ADR-046).
//!
//! Log bytes live in immutable, block-structured chunk objects on object
//! storage (S3 or the local filesystem); Postgres holds one manifest row per
//! *chunk*, never per line. This module is the contract every caller — the
//! Global Logs handlers, the project-scoped search, the CLI — programs
//! against, and [`ChunkStore`] is the implementation.
//!
//! ## The ordering key is the pagination key
//!
//! Every line carries a total order that is stable, deterministic and
//! monotonic: `(timestamp, container_id, line_id)` — see [`LogLineKey`]. The
//! next page is "every line strictly older than the cursor", newest first,
//! and the planner proves it has the newest `limit` lines before returning
//! them (a line at time `T` is final once every unread chunk ended before
//! `T`), so a deep page costs what the first page costs.
//!
//! ## Complete, or honestly partial
//!
//! A search returns either a **complete** page, or — when its time/byte
//! budget runs out first — the lines that are already final plus a cursor
//! positioned at the last fully processed chunk and [`LogPage::scanned_back_to`]
//! set. Because chunks are processed in an order that is a pure function of
//! the cursor, a partial page is a true prefix of the full result and *Next*
//! keeps working. There is no input the engine refuses to read.
//!
//! ## Hot-path safety
//!
//! Ingest does not go through this trait. Collectors feed the chunk writer's
//! per-stream head buffer with a bounded `try_send`, and drop on overflow —
//! ADR-021's "logs shed before anything else" invariant is unchanged.

pub mod access;
pub mod chunk_store;
pub mod manifest;

pub use access::{resolve_log_access_scope, LogAccessError};
pub use manifest::{Manifest, ManifestCursor, ManifestRepo};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::LogAggregatorError;
use crate::types::{LineContext, LogLevel, LogSource, LogStream};

/// Hard ceiling on a single page, regardless of what the caller asks for.
///
/// Pagination is now real, so a caller who wants more lines takes another page
/// rather than one enormous one. Keeps a single response bounded in memory and
/// on the wire.
pub const MAX_PAGE_SIZE: u32 = 1_000;

/// Page size used when the caller does not specify one.
pub const DEFAULT_PAGE_SIZE: u32 = 200;

/// Upper bound on distinct values returned per facet field.
pub const MAX_FACET_VALUES: u32 = 200;

// ── Keys and cursors ────────────────────────────────────────────────────

/// The total order over log lines: `(timestamp, container_id, line_id)`.
///
/// This is simultaneously the sort order, the index prefix and the pagination
/// cursor payload. Ordering is lexicographic in that order, matching the SQL
/// row-value comparison the store issues.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogLineKey {
    pub timestamp: DateTime<Utc>,
    pub container_id: String,
    /// Position of the line: `chunk_id << 20 | line_index` for a sealed
    /// chunk, or [`HEAD_LINE_ID_BASE`]`| line_index` for a line still in the
    /// writer's head buffer. When a head line is sealed its `line_id` changes,
    /// so resume treats `(timestamp, container_id)` as the position and
    /// `line_id` as a tie-break hint.
    pub line_id: i64,
}

/// Number of low bits of `line_id` reserved for the line index within a chunk.
pub const LINE_INDEX_BITS: u32 = 20;

/// `line_id` base for lines that have not been sealed into a chunk yet. Chosen
/// above any possible `chunk_id << 20` so head lines sort after sealed lines
/// with an identical timestamp — they are, by construction, newer.
pub const HEAD_LINE_ID_BASE: i64 = i64::MAX >> 1;

impl LogLineKey {
    /// `(chunk_id, line_index)` for a sealed-chunk key; `None` for a head line.
    pub fn chunk_position(&self) -> Option<(i64, u32)> {
        if self.line_id >= HEAD_LINE_ID_BASE || self.line_id < 0 {
            return None;
        }
        Some((
            self.line_id >> LINE_INDEX_BITS,
            (self.line_id & ((1 << LINE_INDEX_BITS) - 1)) as u32,
        ))
    }

    /// Build the `line_id` of a sealed line.
    pub fn sealed_line_id(chunk_id: i64, line_index: u32) -> i64 {
        (chunk_id << LINE_INDEX_BITS) | i64::from(line_index)
    }
}

/// Wire format version of an opaque cursor.
///
/// Bumped whenever the cursor payload changes. A cursor issued by a different
/// version is rejected with "re-run this search" rather than mis-decoded. `1`
/// belonged to the original chunk-scan engine, `2` to the interim hypertable
/// keyset, `3` is the chunk-position keyset of ADR-046.
const CURSOR_VERSION: u8 = 3;

#[derive(Serialize, Deserialize)]
struct CursorPayload {
    v: u8,
    /// Hash of the query scope this cursor was issued for. A cursor is only
    /// valid for the exact filter set that produced it.
    s: String,
    k: LogLineKey,
}

/// Encode a keyset position as an opaque, versioned, scope-bound cursor.
///
/// The value is base64url of a private JSON payload: callers must treat it as
/// an opaque token, never parse it, and never construct one.
pub fn encode_cursor(key: &LogLineKey, scope_hash: &str) -> Result<String, LogAggregatorError> {
    use base64::Engine as _;
    let payload = serde_json::to_vec(&CursorPayload {
        v: CURSOR_VERSION,
        s: scope_hash.to_string(),
        k: key.clone(),
    })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload))
}

/// Decode an opaque cursor, verifying its version and scope binding.
///
/// Every failure mode — malformed, oversized, wrong version, different filter
/// set — produces the same actionable validation error rather than a partially
/// trusted key.
pub fn decode_cursor(raw: &str, scope_hash: &str) -> Result<LogLineKey, LogAggregatorError> {
    use base64::Engine as _;
    let reject = || LogAggregatorError::InvalidCursor {
        cursor: format!("{}…", raw.chars().take(16).collect::<String>()),
    };
    if raw.len() > 2048 {
        return Err(reject());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| reject())?;
    let payload: CursorPayload = serde_json::from_slice(&bytes).map_err(|_| reject())?;
    if payload.v != CURSOR_VERSION || payload.s != scope_hash {
        return Err(LogAggregatorError::Validation {
            message: "This cursor belongs to a different log search. Re-run the search."
                .to_string(),
        });
    }
    Ok(payload.k)
}

// ── Authorization scope ─────────────────────────────────────────────────

/// The resolved set of resources a caller may read log lines from.
///
/// **This is an allow-list, deliberately.** The previous engine inlined a
/// *deny*-list (`NOT (p.id = ANY($hidden))`) into its candidate SQL, which
/// fails open: an empty or mis-computed hidden set yields "show everything".
/// A store that cannot join `projects`/`project_services` — which is every
/// non-Postgres backend — has no way to verify a deny-list, so the decision is
/// resolved once, in Postgres, and handed over as an explicit set.
///
/// Instance admins get the explicit [`LogAccessScope::All`] variant rather
/// than "an allow-list that happens to contain everything", so "admin" and
/// "resolution returned nothing" can never be confused. Resolution failure is
/// an error (see [`LogAccessError`]) — never an empty-filter fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogAccessScope {
    /// Instance administrator: every project and every external service.
    All,
    /// Exactly these projects and external services, and nothing else.
    ///
    /// Both lists empty means the caller may read nothing at all, which is a
    /// well-formed answer that yields an empty page — not an unfiltered one.
    Allowed {
        project_ids: Vec<i32>,
        external_service_ids: Vec<i32>,
    },
}

impl LogAccessScope {
    /// True when this scope permits reading the given project's lines.
    pub fn allows_project(&self, project_id: i32) -> bool {
        match self {
            Self::All => true,
            Self::Allowed { project_ids, .. } => project_ids.contains(&project_id),
        }
    }

    /// True when this scope permits reading the given external service's lines.
    pub fn allows_external_service(&self, service_id: i32) -> bool {
        match self {
            Self::All => true,
            Self::Allowed {
                external_service_ids,
                ..
            } => external_service_ids.contains(&service_id),
        }
    }
}

// ── Query DTOs ──────────────────────────────────────────────────────────

/// Which family of log sources a query covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogSourceKind {
    /// Everything the caller can see.
    #[default]
    Collected,
    /// Deployment/application containers only (`external_service_id IS NULL`).
    Application,
    /// Managed/imported external services only (`external_service_id IS NOT NULL`).
    Service,
}

/// An explicit selection of resources to search within the allow-list.
///
/// Projects and services are OR'd together: a user who selects "project A"
/// and "database B" means "show me both", and AND-ing them would be
/// contradictory (a line is one or the other) and silently return nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSelection {
    pub project_ids: Vec<i32>,
    pub external_service_ids: Vec<i32>,
}

/// A fully resolved log query.
///
/// Every field is already an id or a literal: name/slug resolution and the
/// authorization decision happen in Postgres *before* the store is called, so
/// the store never needs to join a control-plane table. That is what lets a
/// second backend implement this trait without reimplementing authorization.
#[derive(Debug, Clone)]
pub struct LogQuery {
    /// Resolved authorization allow-list. Applied by every implementation.
    pub scope: LogAccessScope,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub source: LogSourceKind,
    /// Explicit resource selection, or `None` for "everything in `scope`".
    ///
    /// `Some(selection)` narrows *within* the allow-list and can never widen
    /// it. A `Some` whose lists are both empty means the caller asked for
    /// resources that do not exist, which is an empty page — not an
    /// unfiltered one. That distinction is why this is an `Option` rather
    /// than "empty vec means no filter".
    pub selection: Option<LogSelection>,
    pub levels: Vec<LogLevel>,
    pub envs: Vec<String>,
    pub services: Vec<String>,
    pub container_ids: Vec<String>,
    pub node_ids: Vec<i32>,
    pub deploy_id: Option<i32>,
    /// Case-insensitive substring match over `message`.
    pub text: Option<String>,
    /// Keyset position: return only lines strictly older than this key.
    pub before: Option<LogLineKey>,
    /// Page size, already clamped to [`MAX_PAGE_SIZE`] by the caller.
    pub limit: u32,
    /// `grep -C`: raw neighbouring lines to attach to each match, per side.
    /// `0` (the default) returns matches only.
    pub context_lines: u32,
    /// Attribute predicates evaluated per line against its extracted
    /// `fields` (ADR-047 §5). The index answers these without touching the
    /// chunks; the chunk scan enforces them in memory so the two paths agree
    /// line for line (and unsealed head lines, which no index has yet, are
    /// still found).
    pub attrs: Vec<crate::index::analytics::AttrPredicate>,
    /// When set, only sealed chunks with one of these manifest `seq`s are
    /// scanned — the index's answer to "which chunks hold a matching line",
    /// so a text needle combined with attribute predicates scans the
    /// bloom-pruned candidate chunks instead of resolving pointers one by
    /// one. Head buffers are always scanned regardless.
    pub chunk_seqs: Option<Vec<i64>>,
}

impl LogQuery {
    /// Validate the parts of a query the store enforces regardless of caller.
    pub fn validate(&self) -> Result<(), LogAggregatorError> {
        if self.end_time <= self.start_time {
            return Err(LogAggregatorError::Validation {
                message: "end_time must be after start_time".to_string(),
            });
        }
        if self.limit == 0 || self.limit > MAX_PAGE_SIZE {
            return Err(LogAggregatorError::Validation {
                message: format!("Page size must be between 1 and {MAX_PAGE_SIZE}"),
            });
        }
        if self.text.as_ref().is_some_and(|t| t.len() > 1024) {
            return Err(LogAggregatorError::Validation {
                message: "Search text must be 1024 characters or fewer".to_string(),
            });
        }
        for values in [&self.envs, &self.services, &self.container_ids] {
            if values.len() > 100 || values.iter().any(|v| v.len() > 256) {
                return Err(LogAggregatorError::Validation {
                    message: "Too many or oversized log filters".to_string(),
                });
            }
        }
        let selected = self
            .selection
            .as_ref()
            .map_or(0, |s| s.project_ids.len() + s.external_service_ids.len());
        if selected > 1_000 || self.node_ids.len() > 100 {
            return Err(LogAggregatorError::Validation {
                message: "Too many log filters".to_string(),
            });
        }
        Ok(())
    }
}

/// One line as returned by a read.
#[derive(Debug, Clone)]
pub struct LogLineRecord {
    pub timestamp: DateTime<Utc>,
    /// `None` for external-service lines (which carry the `0` sentinel in the
    /// column and identify by `external_service_id`).
    pub project_id: Option<i32>,
    pub external_service_id: Option<i32>,
    pub env: String,
    pub service: String,
    pub level: LogLevel,
    pub stream: LogStream,
    pub container_id: String,
    pub node_id: Option<i32>,
    pub node_name: Option<String>,
    pub deploy_id: Option<i32>,
    pub message: String,
    pub fields: Option<serde_json::Value>,
    pub line_id: i64,
    /// Raw surrounding lines, populated only when `context_lines > 0`.
    pub context: Option<LineContext>,
}

impl LogLineRecord {
    /// The keyset position of this line.
    pub fn key(&self) -> LogLineKey {
        LogLineKey {
            timestamp: self.timestamp,
            container_id: self.container_id.clone(),
            line_id: self.line_id,
        }
    }
}

/// A page of log lines.
///
/// Either complete — `lines` holds every match in the window up to `limit`,
/// newest first — or honestly partial: the planner ran out of budget, and
/// `lines` holds every match that is already *final* (no unread chunk can
/// precede it), `next_cursor` resumes exactly where processing stopped, and
/// `scanned_back_to` tells the user how far back the search got. A partial
/// page is always a true prefix of the complete result; there is never a
/// silently truncated page.
#[derive(Debug, Clone)]
pub struct LogPage {
    /// Newest first, ordered by `(timestamp, container_id, line_id)` DESC.
    pub lines: Vec<LogLineRecord>,
    /// Position to resume from, or `None` when this is the last page.
    pub next_cursor: Option<LogLineKey>,
    /// `Some(t)` when the page is partial: every chunk ending after `t` has
    /// been searched, nothing older has. `None` for a complete page.
    pub scanned_back_to: Option<DateTime<Utc>>,
}

// ── Facets ──────────────────────────────────────────────────────────────

/// A dimension a caller can ask for distinct values of.
///
/// Restricted to a closed enum on purpose: the field name reaches SQL as a
/// column identifier, so it can never be caller-supplied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacetField {
    Env,
    Service,
    Level,
    Stream,
    Project,
    ExternalService,
    Node,
    Deploy,
    Container,
}

impl FacetField {
    /// The wire name of this field, and the key it appears under in a
    /// [`FacetResult`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Service => "service",
            Self::Level => "level",
            Self::Stream => "stream",
            Self::Project => "project_id",
            Self::ExternalService => "external_service_id",
            Self::Node => "node_id",
            Self::Deploy => "deploy_id",
            Self::Container => "container_id",
        }
    }

    /// The SQL column this field groups by. Static strings only — never
    /// interpolated from caller input.
    pub(crate) fn column(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Service => "service",
            Self::Level => "level",
            Self::Stream => "stream",
            Self::Project => "project_id",
            Self::ExternalService => "external_service_id",
            Self::Node => "node_id",
            Self::Deploy => "deploy_id",
            Self::Container => "container_id",
        }
    }
}

/// One distinct value of a facet field, with its occurrence count inside the
/// queried window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FacetValue {
    pub value: String,
    pub count: i64,
}

/// Distinct values per requested field.
#[derive(Debug, Clone, Default)]
pub struct FacetResult {
    /// Keyed by [`FacetField::as_str`]. Values are ordered by count, descending.
    pub fields: std::collections::BTreeMap<String, Vec<FacetValue>>,
    /// `true` when the aggregation hit its cap or timeout, so the value lists
    /// are a prefix rather than the complete set.
    ///
    /// Reported honestly rather than pretending the list is complete — the
    /// whole point of a facet is that the user can trust it to contain values
    /// they have not already seen.
    pub partial: bool,
}

// ── The trait ───────────────────────────────────────────────────────────

/// Backend-neutral storage interface for container log lines.
///
/// The chunk-backed [`ChunkStore`] is the implementation; the trait exists
/// so callers never depend on the storage layout and so a different engine
/// (ClickHouse, for operators who run it) can sit behind the same selection
/// point. Every method takes ids and literals only — no implementation
/// resolves names or authorization.
#[async_trait]
pub trait LogLineStore: Send + Sync {
    /// One keyset page, newest first.
    ///
    /// Returns a complete page, or an honestly partial one (see
    /// [`LogPage::scanned_back_to`]). Never a truncated page presented as a
    /// complete answer.
    async fn search(&self, query: &LogQuery) -> Result<LogPage, LogAggregatorError>;

    /// Distinct values + counts for the requested fields inside the query's
    /// window, so filter pickers can offer values the user has never seen on
    /// screen.
    async fn facets(
        &self,
        query: &LogQuery,
        fields: &[FacetField],
    ) -> Result<FacetResult, LogAggregatorError>;

    /// Raw lines surrounding one line, ignoring level/text filters.
    ///
    /// `before` and `after` are per-side counts. The result is oldest-first
    /// and always includes the target line itself.
    async fn context(
        &self,
        scope: &LogAccessScope,
        key: &LogLineKey,
        before: u32,
        after: u32,
    ) -> Result<Vec<LogLineRecord>, LogAggregatorError>;

    /// Attach `grep -C`-style raw neighbouring lines to each line of a page
    /// already produced by [`Self::search`], in place.
    ///
    /// Defaulted to a no-op so a backend that has no cheap way to fetch
    /// neighbours (or a caller that never asks for context) pays nothing.
    /// [`crate::store::chunk_store::ChunkStore`] overrides this to read the
    /// same chunks the page's matches came from.
    async fn attach_context(
        &self,
        _scope: &LogAccessScope,
        _lines: &mut [LogLineRecord],
        _n: u32,
    ) -> Result<(), LogAggregatorError> {
        Ok(())
    }

    /// Distinct `(container_id, service, node)` tuples in the query's window.
    ///
    /// The source picker's flavour of [`Self::facets`]: it needs the whole
    /// tuple (a container id alone cannot be labelled), so it cannot be
    /// expressed as a single-column `GROUP BY`. Populates the filter dropdowns
    /// with the *full* universe of containers/nodes for the scope, not just
    /// the ones on the current page.
    async fn sources(&self, query: &LogQuery) -> Result<Vec<LogSource>, LogAggregatorError>;

    /// Event time of the newest stored line for a container, if any.
    ///
    /// The ingest path uses this to resume a Docker log stream where it left
    /// off after a restart instead of replaying the container's whole history.
    /// It lives on the trait because the answer must come from whichever store
    /// actually holds the lines.
    async fn latest_timestamp_for_container(
        &self,
        container_id: &str,
    ) -> Result<Option<DateTime<Utc>>, LogAggregatorError>;

    /// Delete a project's lines older than `before`; returns the number of
    /// lines removed (summed from the manifests of the chunks deleted).
    ///
    /// Backs the operator-initiated purge endpoint. Ordinary expiry is the
    /// retention loop and does not go through here.
    async fn purge_project(
        &self,
        project_id: i32,
        before: DateTime<Utc>,
    ) -> Result<u64, LogAggregatorError>;

    /// Resolve `(chunk_seq, line_index)` positions — as produced by the
    /// ADR-047 line index's `search_pointers` — to full records, in the same
    /// order as `positions`. Positions the scope cannot see or that no
    /// longer exist are silently dropped (the chunk may have been purged or
    /// compacted away since the index row was written).
    ///
    /// Defaulted to "nothing resolves" so a backend that has no chunk layout
    /// to look up pays nothing; the chunk-backed store is the only
    /// implementation that overrides this.
    async fn lines_by_position(
        &self,
        _scope: &LogAccessScope,
        _positions: &[(i64, u32)],
    ) -> Result<Vec<LogLineRecord>, LogAggregatorError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> LogLineKey {
        LogLineKey {
            timestamp: "2026-01-01T12:00:00.123456789Z".parse().unwrap(),
            container_id: "container-1".to_string(),
            line_id: LogLineKey::sealed_line_id(42, 7),
        }
    }

    #[test]
    fn cursor_round_trips_within_the_same_scope() {
        let encoded = encode_cursor(&key(), "scope-a").unwrap();
        assert_eq!(decode_cursor(&encoded, "scope-a").unwrap(), key());
    }

    #[test]
    fn cursor_is_rejected_for_a_different_scope() {
        let encoded = encode_cursor(&key(), "scope-a").unwrap();
        let error = decode_cursor(&encoded, "scope-b").unwrap_err();
        assert!(
            error.to_string().contains("Re-run the search"),
            "a cursor from another filter set must say what to do: {error}"
        );
    }

    #[test]
    fn cursor_rejects_garbage_and_oversized_input() {
        assert!(decode_cursor("not-base64!!", "scope-a").is_err());
        assert!(decode_cursor(&"a".repeat(4096), "scope-a").is_err());
        // Valid base64 of something that isn't a cursor payload.
        use base64::Engine as _;
        let bogus = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}");
        assert!(decode_cursor(&bogus, "scope-a").is_err());
    }

    #[test]
    fn cursor_from_the_retired_scan_engine_is_rejected() {
        // v1 cursors were raw JSON emitted by global_search.rs. They must be
        // refused outright, never partially trusted.
        let v1 = r#"{"version":1,"scope":"scope-a","before":["2026-01-01T12:00:00Z","…",0]}"#;
        assert!(decode_cursor(v1, "scope-a").is_err());
    }

    #[test]
    fn keys_order_by_timestamp_then_container_then_line_id() {
        let base = key();
        let mut later_ts = base.clone();
        later_ts.timestamp = "2026-01-01T12:00:01Z".parse().unwrap();
        let mut later_container = base.clone();
        later_container.container_id = "container-2".to_string();
        let mut later_line = base.clone();
        later_line.line_id += 1;

        assert!(later_ts > base);
        assert!(later_container > base);
        assert!(later_line > base);
        // Timestamp dominates the other two components.
        assert!(later_ts > later_container && later_ts > later_line);
    }

    fn valid_query() -> LogQuery {
        LogQuery {
            scope: LogAccessScope::All,
            start_time: "2026-01-01T00:00:00Z".parse().unwrap(),
            end_time: "2026-01-02T00:00:00Z".parse().unwrap(),
            source: LogSourceKind::Collected,
            selection: None,
            levels: vec![],
            envs: vec![],
            services: vec![],
            container_ids: vec![],
            node_ids: vec![],
            deploy_id: None,
            text: None,
            before: None,
            limit: DEFAULT_PAGE_SIZE,
            context_lines: 0,
            attrs: Vec::new(),
            chunk_seqs: None,
        }
    }

    #[test]
    fn query_validation_rejects_bad_windows_and_page_sizes() {
        assert!(valid_query().validate().is_ok());

        let mut inverted = valid_query();
        inverted.end_time = inverted.start_time;
        assert!(inverted.validate().is_err());

        let mut zero_page = valid_query();
        zero_page.limit = 0;
        assert!(zero_page.validate().is_err());

        let mut huge_page = valid_query();
        huge_page.limit = MAX_PAGE_SIZE + 1;
        assert!(huge_page.validate().is_err());
    }

    #[test]
    fn query_validation_rejects_oversized_filters() {
        let mut long_text = valid_query();
        long_text.text = Some("x".repeat(1025));
        assert!(long_text.validate().is_err());

        let mut many_envs = valid_query();
        many_envs.envs = (0..101).map(|i| i.to_string()).collect();
        assert!(many_envs.validate().is_err());

        let mut many_nodes = valid_query();
        many_nodes.node_ids = (0..101).collect();
        assert!(many_nodes.validate().is_err());
    }

    #[test]
    fn empty_allow_list_permits_nothing() {
        // The critical fail-closed property: an allow-list that resolved to
        // nothing must match nothing, NOT everything.
        let empty = LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![],
        };
        assert!(!empty.allows_project(1));
        assert!(!empty.allows_external_service(1));

        let admin = LogAccessScope::All;
        assert!(admin.allows_project(1));
        assert!(admin.allows_external_service(1));
    }

    #[test]
    fn allow_list_admits_only_listed_resources() {
        let scope = LogAccessScope::Allowed {
            project_ids: vec![1, 2],
            external_service_ids: vec![7],
        };
        assert!(scope.allows_project(1));
        assert!(!scope.allows_project(3));
        assert!(scope.allows_external_service(7));
        assert!(!scope.allows_external_service(8));
    }

    #[test]
    fn facet_field_names_and_columns_agree() {
        for field in [
            FacetField::Env,
            FacetField::Service,
            FacetField::Level,
            FacetField::Stream,
            FacetField::Project,
            FacetField::ExternalService,
            FacetField::Node,
            FacetField::Deploy,
            FacetField::Container,
        ] {
            assert_eq!(field.as_str(), field.column());
        }
    }

    #[test]
    fn line_id_round_trips_chunk_position() {
        let id = LogLineKey::sealed_line_id(123_456, 987);
        let k = LogLineKey {
            line_id: id,
            ..key()
        };
        assert_eq!(k.chunk_position(), Some((123_456, 987)));

        let head = LogLineKey {
            line_id: HEAD_LINE_ID_BASE | 5,
            ..key()
        };
        assert_eq!(head.chunk_position(), None);
        assert!(
            head.line_id > id,
            "head lines sort after sealed lines at equal ts"
        );
    }
}
