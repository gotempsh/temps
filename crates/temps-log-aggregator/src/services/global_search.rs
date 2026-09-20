// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-project log search and facets.
//!
//! The wire DTOs for `POST /api/logs/global/search` and
//! `POST /api/logs/global/facets`, plus the translation from "what the user
//! typed" (project slugs, service names, resource scopes) into the id-only
//! [`LogQuery`] the store understands.
//!
//! Name → id resolution and the authorization allow-list are both settled here,
//! in Postgres, *before* the store is called: a storage backend that cannot
//! join `projects`/`project_services` must never be asked to make either
//! decision (ADR-045 §7).
//!
//! What this module no longer contains: the old scan engine's `CHUNK_BATCH`,
//! `MAX_CHUNKS`, `MAX_COMPRESSED_BYTES`, `MAX_CHUNK_BYTES` (which used to
//! *skip* an oversized chunk outright — silent data loss),
//! `MAX_DECOMPRESSED_BYTES` and `scan_limit_reached`. A search now returns a
//! complete page, or an honestly partial one with a resumable cursor
//! (ADR-046 §3 step 5) — never a truncated page presented as complete.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use super::LogSearchService;
use crate::{
    error::LogAggregatorError,
    services::search::to_search_line,
    store::{
        decode_cursor, encode_cursor, FacetField, FacetValue, LogAccessScope, LogQuery,
        LogSelection, LogSourceKind, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
    },
    types::{LogLevel, LogSearchLine},
};

/// Which family of log sources a global query covers.
pub type GlobalLogSource = LogSourceKind;

/// Filter body shared by `/logs/global/search` and `/logs/global/facets`.
///
/// Both endpoints take the identical filter set so a facet count always
/// describes the search the user is actually looking at.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GlobalLogSearchRequest {
    #[schema(value_type = String)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String)]
    pub end_time: DateTime<Utc>,
    #[serde(default)]
    pub source: GlobalLogSource,
    /// Match project ID, slug or name. Empty selects all authorized projects.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Database/service IDs or names, not container service labels.
    #[serde(default)]
    pub external_services: Vec<String>,
    /// Explicit resource identities, e.g. `application:12` or `service:34`.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub levels: Vec<LogLevel>,
    #[serde(default)]
    pub envs: Vec<String>,
    /// Container service labels (the `service` column), e.g. `web`, `worker`.
    #[serde(default)]
    pub services: Vec<String>,
    /// Docker container IDs.
    #[serde(default)]
    pub container_ids: Vec<String>,
    #[serde(default)]
    pub node_ids: Vec<i32>,
    pub deploy_id: Option<i32>,
    /// Case-insensitive substring match over the message.
    pub text: Option<String>,
    /// Opaque cursor from the previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Defaults to 200, server-capped at 1000.
    pub page_size: Option<u32>,
    /// Attribute predicates (ADR-047 §5): `<key><op><value>` with `op` one of
    /// `=`, `!=`, `^=` (prefix), `>`, `<`, or `<key>?` for "exists". Requires
    /// the line index; when combined with `text`, the text filter is applied
    /// in memory over the lines the index already matched (the index holds
    /// no message bytes, so it cannot answer `text` on its own).
    #[serde(default)]
    pub attrs: Vec<String>,
}

/// One line in a global search result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GlobalLogLine {
    #[serde(flatten)]
    pub line: LogSearchLine,
    /// `None` for external-service lines.
    pub project_id: Option<i32>,
    pub external_service_id: Option<i32>,
    /// Display name of the owning project or external service.
    pub owner: String,
    pub env: String,
}

/// A page of global search results.
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalLogSearchResponse {
    /// Newest first, ordered by `(timestamp, container_id, line_id)`.
    pub lines: Vec<GlobalLogLine>,
    /// Opaque cursor for the next (older) page, or `None` on the last page.
    ///
    /// This is a keyset position over the store's own sort order, so page 40
    /// costs what page 1 costs. Stays populated on a partial page too — that
    /// is the whole point: the user can press Next to keep searching.
    pub next_cursor: Option<String>,
    /// `true` when the store's time/byte budget ran out before this page
    /// could be proven complete (`scanned_back_to` explains how far).
    pub partial: bool,
    /// Set when `partial` is `true`: every chunk ending after this timestamp
    /// has been searched, nothing older has yet.
    #[schema(value_type = Option<String>)]
    pub scanned_back_to: Option<DateTime<Utc>>,
}

/// Facet request: the same filter body plus the fields to aggregate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct GlobalLogFacetsRequest {
    #[serde(flatten)]
    pub filter: GlobalLogSearchRequest,
    /// Fields to return distinct values for. Empty returns the default set.
    #[serde(default)]
    pub fields: Vec<FacetField>,
}

/// Distinct values and counts per requested field, inside the current window.
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalLogFacetsResponse {
    /// Keyed by field name (`env`, `service`, `level`, `node_id`, …). Values
    /// are ordered by count, descending.
    #[schema(value_type = Object)]
    pub facets: BTreeMap<String, Vec<FacetValue>>,
    /// `true` when a field's value list was capped or timed out, so it is a
    /// prefix rather than the complete set. Reported rather than glossed: the
    /// point of a facet is that a user can trust it to surface values they
    /// have never seen on screen.
    pub partial: bool,
}

/// Fields returned when a facet request does not name any.
pub const DEFAULT_FACET_FIELDS: &[FacetField] = &[
    FacetField::Env,
    FacetField::Service,
    FacetField::Level,
    FacetField::Node,
    FacetField::Deploy,
];

fn invalid(message: &str) -> LogAggregatorError {
    LogAggregatorError::Validation {
        message: message.into(),
    }
}

/// Project/service ids named by a request's `projects`, `external_services`
/// and `scopes` selectors.
#[derive(Debug, Default, PartialEq, Eq)]
struct ResolvedSelectors {
    project_ids: Vec<i32>,
    external_service_ids: Vec<i32>,
    /// True when the caller named at least one selector. Distinguishes "the
    /// user asked for nothing in particular" (search everything authorized)
    /// from "the user asked for something that does not exist" (empty page).
    explicit: bool,
}

impl GlobalLogSearchRequest {
    /// Hash of every filter except the cursor, binding a cursor to the exact
    /// query that produced it.
    fn scope_hash(&self) -> Result<String, LogAggregatorError> {
        let mut without_cursor = self.clone();
        without_cursor.cursor = None;
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(
            &without_cursor,
        )?)))
    }

    fn validate(&self) -> Result<(), LogAggregatorError> {
        // No 24-hour ceiling any more: the window is bounded by an index, not
        // by how many megabytes a scan is willing to decompress.
        if self.end_time <= self.start_time {
            return Err(invalid("end_time must be after start_time"));
        }
        if !(1..=MAX_PAGE_SIZE).contains(&self.page_size.unwrap_or(DEFAULT_PAGE_SIZE)) {
            return Err(invalid(&format!(
                "Page size must be between 1 and {MAX_PAGE_SIZE}"
            )));
        }
        for values in [
            &self.projects,
            &self.external_services,
            &self.scopes,
            &self.envs,
            &self.services,
            &self.container_ids,
            &self.attrs,
        ] {
            if values.len() > 100 || values.iter().any(|s| s.len() > 256) {
                return Err(invalid("Too many or oversized log filters"));
            }
        }
        if self.node_ids.len() > 100 || self.text.as_ref().is_some_and(|s| s.len() > 1024) {
            return Err(invalid("Oversized log filter"));
        }
        for scope in &self.scopes {
            if !scope.split_once(':').is_some_and(|(kind, id)| {
                matches!(kind, "application" | "service")
                    && id.parse::<i32>().is_ok_and(|id| id > 0)
            }) {
                return Err(invalid("Invalid log resource scope"));
            }
        }
        Ok(())
    }
}

impl LogSearchService {
    /// One keyset page of cross-project log lines.
    pub async fn search_global(
        &self,
        request: &GlobalLogSearchRequest,
        scope: &LogAccessScope,
    ) -> Result<GlobalLogSearchResponse, LogAggregatorError> {
        let (query, scope_hash) = self.build_query(request, scope).await?;
        self.search_with_query(query, scope, &scope_hash).await
    }

    /// Run an already-built (and possibly amended — attribute predicates,
    /// chunk restriction) query through the chunk store and shape the page.
    pub(crate) async fn search_with_query(
        &self,
        query: LogQuery,
        scope: &LogAccessScope,
        scope_hash: &str,
    ) -> Result<GlobalLogSearchResponse, LogAggregatorError> {
        let mut page = self.store.search(&query).await?;
        if query.context_lines > 0 {
            self.store
                .attach_context(scope, &mut page.lines, query.context_lines)
                .await?;
        }

        let next_cursor = page
            .next_cursor
            .as_ref()
            .map(|key| encode_cursor(key, scope_hash))
            .transpose()?;

        let owners = self.resolve_owner_names(&page.lines).await?;
        let lines = page
            .lines
            .iter()
            .map(|record| GlobalLogLine {
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
            partial: page.scanned_back_to.is_some(),
            scanned_back_to: page.scanned_back_to,
        })
    }

    /// Distinct values and counts for the requested facet fields.
    pub async fn facets_global(
        &self,
        request: &GlobalLogFacetsRequest,
        scope: &LogAccessScope,
    ) -> Result<GlobalLogFacetsResponse, LogAggregatorError> {
        let (query, _) = self.build_query(&request.filter, scope).await?;
        let mut fields = request.fields.clone();
        if fields.is_empty() {
            fields = DEFAULT_FACET_FIELDS.to_vec();
        }
        fields.sort_by_key(|f| f.as_str());
        fields.dedup();

        let result = self.store.facets(&query, &fields).await?;
        Ok(GlobalLogFacetsResponse {
            facets: result.fields,
            partial: result.partial,
        })
    }

    /// Validate a request, resolve its selectors to ids and build the store
    /// query. Returns the query and the cursor scope hash.
    ///
    /// `pub(crate)` so the ADR-047 read-side handlers (attributes, facets,
    /// histogram, aggregate) can build the identical scoped [`LogQuery`] from
    /// their own query-string filter, sharing every bit of name resolution
    /// and validation `search_global`/`facets_global` use.
    pub(crate) async fn build_query(
        &self,
        request: &GlobalLogSearchRequest,
        scope: &LogAccessScope,
    ) -> Result<(LogQuery, String), LogAggregatorError> {
        request.validate()?;
        let scope_hash = request.scope_hash()?;
        let before = match request.cursor.as_deref() {
            Some(raw) => Some(decode_cursor(raw, &scope_hash)?),
            None => None,
        };

        let selectors = self.resolve_selectors(request).await?;
        // A caller who named selectors gets `Some(...)` even when they all
        // resolved to nothing — "you asked for a project that does not exist"
        // is an empty page, never a silently unfiltered one.
        let selection = selectors.explicit.then_some(LogSelection {
            project_ids: selectors.project_ids,
            external_service_ids: selectors.external_service_ids,
        });

        let query = LogQuery {
            scope: scope.clone(),
            start_time: request.start_time,
            end_time: request.end_time,
            source: request.source,
            selection,
            levels: request.levels.clone(),
            envs: request.envs.clone(),
            services: request.services.clone(),
            container_ids: request.container_ids.clone(),
            node_ids: request.node_ids.clone(),
            deploy_id: request.deploy_id,
            text: request.text.clone(),
            before,
            limit: request
                .page_size
                .unwrap_or(DEFAULT_PAGE_SIZE)
                .clamp(1, MAX_PAGE_SIZE),
            context_lines: 0,
            attrs: Vec::new(),
            chunk_seqs: None,
        };
        query.validate()?;
        Ok((query, scope_hash))
    }

    /// Resolve `projects` / `external_services` / `scopes` selectors to ids.
    ///
    /// These are matched against control-plane tables, which is exactly why
    /// they cannot be pushed into the store: a non-Postgres backend has no
    /// `projects` table to join.
    async fn resolve_selectors(
        &self,
        request: &GlobalLogSearchRequest,
    ) -> Result<ResolvedSelectors, LogAggregatorError> {
        let mut resolved = ResolvedSelectors {
            explicit: !request.projects.is_empty()
                || !request.external_services.is_empty()
                || !request.scopes.is_empty(),
            ..Default::default()
        };
        if !resolved.explicit {
            return Ok(resolved);
        }
        let db = self.metadata_service.db.as_ref();

        if !request.projects.is_empty() {
            let rows = db
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM projects WHERE id::text = ANY($1::text[]) \
                     OR name = ANY($1::text[]) OR slug = ANY($1::text[])",
                    [request.projects.clone().into()],
                ))
                .await?;
            for row in &rows {
                resolved.project_ids.push(row.try_get::<i32>("", "id")?);
            }
        }

        if !request.external_services.is_empty() {
            let rows = db
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM external_services WHERE id::text = ANY($1::text[]) \
                     OR name = ANY($1::text[])",
                    [request.external_services.clone().into()],
                ))
                .await?;
            for row in &rows {
                resolved
                    .external_service_ids
                    .push(row.try_get::<i32>("", "id")?);
            }
        }

        // `scopes` are already ids — validated to `application:<n>` /
        // `service:<n>` by `validate()`.
        for scope in &request.scopes {
            match scope.split_once(':') {
                Some(("application", id)) => {
                    if let Ok(id) = id.parse::<i32>() {
                        resolved.project_ids.push(id);
                    }
                }
                Some(("service", id)) => {
                    if let Ok(id) = id.parse::<i32>() {
                        resolved.external_service_ids.push(id);
                    }
                }
                _ => return Err(invalid("Invalid log resource scope")),
            }
        }

        resolved.project_ids.sort_unstable();
        resolved.project_ids.dedup();
        resolved.external_service_ids.sort_unstable();
        resolved.external_service_ids.dedup();
        Ok(resolved)
    }

    /// Look up display names for the projects and services on a page.
    ///
    /// Two bounded queries per page rather than a join in the store, so the
    /// store stays backend-neutral and never needs the control-plane schema.
    ///
    /// `pub(crate)` so the attribute-filtered search path (which resolves its
    /// own [`crate::store::LogLineRecord`]s from index pointers) can build
    /// the identical `owner` field the plain path does.
    pub(crate) async fn resolve_owner_names(
        &self,
        records: &[crate::store::LogLineRecord],
    ) -> Result<HashMap<(bool, i32), String>, LogAggregatorError> {
        let mut owners = HashMap::new();
        if records.is_empty() {
            return Ok(owners);
        }
        let db = self.metadata_service.db.as_ref();

        let mut project_ids: Vec<i32> = records.iter().filter_map(|r| r.project_id).collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        if !project_ids.is_empty() {
            let rows = db
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id, name FROM projects WHERE id = ANY($1::int[])",
                    [project_ids.into()],
                ))
                .await?;
            for row in &rows {
                owners.insert(
                    (false, row.try_get::<i32>("", "id")?),
                    row.try_get::<String>("", "name")?,
                );
            }
        }

        let mut service_ids: Vec<i32> = records
            .iter()
            .filter_map(|r| r.external_service_id)
            .collect();
        service_ids.sort_unstable();
        service_ids.dedup();
        if !service_ids.is_empty() {
            let rows = db
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id, name FROM external_services WHERE id = ANY($1::int[])",
                    [service_ids.into()],
                ))
                .await?;
            for row in &rows {
                owners.insert(
                    (true, row.try_get::<i32>("", "id")?),
                    row.try_get::<String>("", "name")?,
                );
            }
        }

        Ok(owners)
    }
}

/// `(is_external_service, id)` key into the owner-name lookup.
pub(crate) fn owner_key(record: &crate::store::LogLineRecord) -> (bool, i32) {
    match record.external_service_id {
        Some(id) => (true, id),
        None => (false, record.project_id.unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> GlobalLogSearchRequest {
        serde_json::from_value(serde_json::json!({
            "start_time": "2026-01-01T00:00:00Z",
            "end_time": "2026-01-02T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn a_window_longer_than_a_day_is_now_allowed() {
        // The retired scan engine refused anything over 24 hours because it
        // had to read every byte in the window. A keyset page over an index
        // does not care how wide the window is.
        let mut wide = request();
        wide.end_time = "2026-03-01T00:00:00Z".parse().unwrap();
        assert!(wide.validate().is_ok());
    }

    #[test]
    fn an_inverted_or_empty_window_is_rejected() {
        let mut inverted = request();
        inverted.end_time = inverted.start_time;
        assert!(inverted.validate().is_err());

        let mut backwards = request();
        backwards.end_time = "2025-01-01T00:00:00Z".parse().unwrap();
        assert!(backwards.validate().is_err());
    }

    #[test]
    fn page_size_bounds_are_enforced() {
        let mut too_big = request();
        too_big.page_size = Some(MAX_PAGE_SIZE + 1);
        assert!(too_big.validate().is_err());

        let mut zero = request();
        zero.page_size = Some(0);
        assert!(zero.validate().is_err());

        let mut at_cap = request();
        at_cap.page_size = Some(MAX_PAGE_SIZE);
        assert!(at_cap.validate().is_ok());
    }

    #[test]
    fn malformed_resource_scopes_are_rejected() {
        for bad in [
            "application",
            "application:",
            "application:abc",
            "application:0",
            "application:-1",
            "cluster:1",
        ] {
            let mut q = request();
            q.scopes = vec![bad.to_string()];
            assert!(q.validate().is_err(), "{bad} must be rejected");
        }
        let mut good = request();
        good.scopes = vec!["application:12".into(), "service:34".into()];
        assert!(good.validate().is_ok());
    }

    #[test]
    fn oversized_filter_lists_are_rejected() {
        let mut many = request();
        many.projects = (0..101).map(|i| i.to_string()).collect();
        assert!(many.validate().is_err());

        let mut long = request();
        long.envs = vec!["x".repeat(257)];
        assert!(long.validate().is_err());

        let mut long_text = request();
        long_text.text = Some("x".repeat(1025));
        assert!(long_text.validate().is_err());
    }

    #[test]
    fn the_scope_hash_ignores_only_the_cursor() {
        let base = request();
        let mut with_cursor = base.clone();
        with_cursor.cursor = Some("opaque".into());
        assert_eq!(
            base.scope_hash().unwrap(),
            with_cursor.scope_hash().unwrap(),
            "page 2's own cursor must validate against page 2's request"
        );

        let mut narrowed = base.clone();
        narrowed.levels = vec![LogLevel::Error];
        assert_ne!(
            base.scope_hash().unwrap(),
            narrowed.scope_hash().unwrap(),
            "changing a filter must invalidate the cursor"
        );
    }

    #[test]
    fn cursors_from_the_retired_scan_engine_are_rejected() {
        let hash = request().scope_hash().unwrap();
        let v1 =
            format!(r#"{{"version":1,"scope":"{hash}","before":["2026-01-01T12:00:00Z","x",0]}}"#);
        let error = decode_cursor(&v1, &hash).unwrap_err();
        assert!(
            error.to_string().contains("cursor") || error.to_string().contains("Re-run"),
            "{error}"
        );
    }

    #[test]
    fn owner_key_distinguishes_a_project_from_a_service_with_the_same_id() {
        let mut record = crate::store::LogLineRecord {
            timestamp: "2026-01-01T00:00:00Z".parse().unwrap(),
            project_id: Some(7),
            external_service_id: None,
            env: "production".into(),
            service: "web".into(),
            level: LogLevel::Info,
            stream: crate::types::LogStream::Stdout,
            container_id: "c".into(),
            node_id: None,
            node_name: None,
            deploy_id: None,
            message: "m".into(),
            fields: None,
            line_id: 1,
            context: None,
        };
        assert_eq!(owner_key(&record), (false, 7));
        record.project_id = None;
        record.external_service_id = Some(7);
        assert_eq!(owner_key(&record), (true, 7));
    }

    #[test]
    fn default_facet_fields_cover_the_filter_pickers() {
        let names: Vec<&str> = DEFAULT_FACET_FIELDS.iter().map(|f| f.as_str()).collect();
        for expected in ["env", "service", "level", "node_id", "deploy_id"] {
            assert!(names.contains(&expected), "missing facet field {expected}");
        }
    }
}
