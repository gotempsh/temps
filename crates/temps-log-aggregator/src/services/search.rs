// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log search service.
//!
//! This is a thin translation layer, deliberately. It converts wire filters
//! into a [`LogQuery`], calls the configured [`LogLineStore`], and converts
//! the result back. All the ordering, pagination, filtering and bounding
//! live in the store (ADR-046), where a manifest index and block-structured
//! chunks can do them.
//!
//! What used to be here — a chunk-scan engine with a 24-hour free-text
//! ceiling, a bounded in-memory heap and no index over line content, level
//! or time *within* a chunk — is gone. "Narrow the window until it fits" was
//! the only pagination strategy it could offer.

use std::sync::Arc;

use crate::error::LogAggregatorError;
use crate::store::{
    decode_cursor, encode_cursor, LogAccessScope, LogLineKey, LogLineRecord, LogLineStore,
    LogQuery, LogSelection, LogSourceKind, MAX_PAGE_SIZE,
};
use crate::types::{
    ContextLine, ContextRequest, ContextResponse, LogSearchFilter, LogSearchLine, LogSearchResult,
    MAX_CONTEXT_LINES,
};

/// Search service: routes log queries to the configured backend store.
pub struct LogSearchService {
    pub(crate) store: Arc<dyn LogLineStore>,
    pub(crate) metadata_service: Arc<crate::services::LogMetadataService>,
}

impl LogSearchService {
    pub fn new(
        store: Arc<dyn LogLineStore>,
        metadata_service: Arc<crate::services::LogMetadataService>,
    ) -> Self {
        Self {
            store,
            metadata_service,
        }
    }

    /// Execute a project- or service-scoped log search.
    ///
    /// `scope` is the caller's already-resolved authorization allow-list; the
    /// filter narrows *within* it and can never widen it.
    pub async fn search(
        &self,
        filter: &LogSearchFilter,
        scope: &LogAccessScope,
    ) -> Result<LogSearchResult, LogAggregatorError> {
        let scope_hash = filter_scope_hash(filter)?;
        let before = match filter.cursor.as_deref() {
            Some(raw) => Some(decode_cursor(raw, &scope_hash)?),
            None => None,
        };

        let query = LogQuery {
            scope: scope.clone(),
            start_time: filter.start_time,
            end_time: filter.end_time,
            source: LogSourceKind::Collected,
            // Exactly one resource: the project, or the external service when
            // one is named (`project_id` is ignored in that mode, matching the
            // handler's own guard).
            selection: Some(match filter.external_service_id {
                Some(service_id) => LogSelection {
                    project_ids: vec![],
                    external_service_ids: vec![service_id],
                },
                None => LogSelection {
                    project_ids: vec![filter.project_id],
                    external_service_ids: vec![],
                },
            }),
            levels: filter.levels.clone(),
            envs: filter.envs.clone(),
            services: filter.services.clone(),
            container_ids: filter.container_ids.clone(),
            node_ids: filter.node_ids.clone(),
            deploy_id: filter.deploy_id,
            text: filter.text.clone(),
            before,
            limit: filter.page_size.clamp(1, MAX_PAGE_SIZE),
            context_lines: filter.context_lines.min(MAX_CONTEXT_LINES),
            attrs: Vec::new(),
            chunk_seqs: None,
        };
        query.validate()?;

        let mut page = self.store.search(&query).await?;
        if query.context_lines > 0 {
            self.store
                .attach_context(scope, &mut page.lines, query.context_lines)
                .await?;
        }

        let next_cursor = page
            .next_cursor
            .as_ref()
            .map(|key| encode_cursor(key, &scope_hash))
            .transpose()?;

        // The filter dropdowns need the FULL set of containers/nodes for the
        // scope, not just the ones that happen to be on this page. Only worth
        // paying for on the first page — "load older" reuses what the client
        // already has.
        let available_sources = if filter.cursor.is_none() {
            self.store.sources(&query).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        // Wire order is oldest-first (terminal / journalctl convention) so the
        // UI can prepend older pages at the top while new lines appear at the
        // bottom. The store returns newest-first because that is the direction
        // the index is walked.
        let mut lines: Vec<LogSearchLine> = page.lines.iter().map(to_search_line).collect();
        lines.reverse();

        Ok(LogSearchResult {
            lines,
            next_cursor,
            partial: page.scanned_back_to.is_some(),
            scanned_back_to: page.scanned_back_to,
            available_sources,
        })
    }

    /// Raw surrounding lines for one log line, oldest first.
    ///
    /// The target line is always included and flagged `is_match`; if it cannot
    /// be found the request is a 404 rather than an empty window, because
    /// "here is the context you asked for" and "that line no longer exists"
    /// are different answers.
    pub async fn get_context(
        &self,
        request: &ContextRequest,
        scope: &LogAccessScope,
    ) -> Result<ContextResponse, LogAggregatorError> {
        let line_id: i64 = request
            .line_id
            .parse()
            .map_err(|_| LogAggregatorError::Validation {
                message: format!("Invalid line_id '{}'", request.line_id),
            })?;
        let key = LogLineKey {
            timestamp: request.timestamp,
            container_id: request.container_id.clone(),
            line_id,
        };
        let radius = request.lines.min(MAX_CONTEXT_LINES);

        let records = self.store.context(scope, &key, radius, radius).await?;
        let target_index = records
            .iter()
            .position(|record| record.line_id == line_id && record.container_id == key.container_id)
            .ok_or_else(|| LogAggregatorError::LineNotFound {
                container_id: key.container_id.clone(),
                line_id,
            })?;

        let lines = records
            .iter()
            .enumerate()
            .map(|(i, record)| ContextLine {
                timestamp: record.timestamp,
                level: record.level,
                message: record.message.clone(),
                fields: record.fields.clone(),
                line_id: record.line_id.to_string(),
                is_match: i == target_index,
            })
            .collect();

        Ok(ContextResponse {
            lines,
            target_index,
        })
    }
}

/// Convert a store record into the wire line shape.
pub(crate) fn to_search_line(record: &LogLineRecord) -> LogSearchLine {
    LogSearchLine {
        timestamp: record.timestamp,
        level: record.level,
        stream: record.stream,
        service: record.service.clone(),
        message: record.message.clone(),
        fields: record.fields.clone(),
        line_id: record.line_id.to_string(),
        deploy_id: record.deploy_id,
        container_id: record.container_id.clone(),
        node_id: record.node_id,
        node_name: record.node_name.clone(),
        context: record.context.clone(),
    }
}

/// Hash of everything in a filter except the cursor itself.
///
/// A cursor is only valid for the exact filter set that produced it; changing
/// any filter invalidates it rather than silently paginating a different
/// query. Same guarantee the retired engine's cursor carried.
fn filter_scope_hash(filter: &LogSearchFilter) -> Result<String, LogAggregatorError> {
    use sha2::{Digest, Sha256};
    let mut without_cursor = filter.clone();
    without_cursor.cursor = None;
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &without_cursor,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLevel;
    use chrono::{Duration, Utc};

    fn filter() -> LogSearchFilter {
        LogSearchFilter {
            project_id: 1,
            external_service_id: None,
            start_time: Utc::now() - Duration::hours(1),
            end_time: Utc::now(),
            levels: vec![],
            services: vec![],
            envs: vec![],
            container_ids: vec![],
            node_ids: vec![],
            deploy_id: None,
            text: None,
            cursor: None,
            page_size: 200,
            context_lines: 0,
        }
    }

    #[test]
    fn a_cursor_is_bound_to_the_filters_that_produced_it() {
        let filter = filter();
        let hash = filter_scope_hash(&filter).unwrap();
        let key = LogLineKey {
            timestamp: filter.start_time,
            container_id: "container-1".into(),
            line_id: 1_758_000_000_000_000_001,
        };
        let cursor = encode_cursor(&key, &hash).unwrap();
        assert_eq!(decode_cursor(&cursor, &hash).unwrap(), key);

        // Change any filter and the cursor no longer applies.
        let mut narrowed = filter.clone();
        narrowed.levels = vec![LogLevel::Error];
        let narrowed_hash = filter_scope_hash(&narrowed).unwrap();
        assert_ne!(hash, narrowed_hash);
        assert!(decode_cursor(&cursor, &narrowed_hash).is_err());
    }

    #[test]
    fn the_cursor_itself_does_not_change_the_scope_hash() {
        // Otherwise page 2's cursor could never validate against page 2's own
        // request, and pagination would stop after one page.
        let base = filter();
        let mut with_cursor = base.clone();
        with_cursor.cursor = Some("anything".into());
        assert_eq!(
            filter_scope_hash(&base).unwrap(),
            filter_scope_hash(&with_cursor).unwrap()
        );
    }

    #[test]
    fn to_search_line_carries_the_decimal_line_id() {
        let record = LogLineRecord {
            timestamp: Utc::now(),
            project_id: Some(1),
            external_service_id: None,
            env: "production".into(),
            service: "web".into(),
            level: LogLevel::Info,
            stream: crate::types::LogStream::Stdout,
            container_id: "c1".into(),
            node_id: None,
            node_name: None,
            deploy_id: None,
            message: "hello".into(),
            fields: None,
            line_id: 42,
            context: None,
        };
        assert_eq!(to_search_line(&record).line_id, "42");
    }
}
