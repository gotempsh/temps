// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One sandbox's recorded outbound connections, read for its own page.
//!
//! The records are the same OpenTelemetry log records the project's logs
//! show (see [`super::records`]). They are read here by the `sandbox_id`
//! column the platform stores them with, newest first, so a sandbox's
//! activity never needs its project's logs filtered by attribute.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_otel::log_attributes::{LogAttributeFilter, LogAttributeOperator};
use temps_otel::types::{LogQuery, LogRecord};
use utoipa::ToSchema;

use super::{EgressRecordingError, EgressRecordingService, SANDBOX_ID_ATTRIBUTE};

/// Records per page when the caller does not say.
pub const DEFAULT_PER_PAGE: u64 = 20;
/// The most records one page returns.
pub const MAX_PER_PAGE: u64 = 100;

/// Only the connections that ended this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EgressOutcomeFilter {
    Allowed,
    Refused,
    Failed,
}

impl EgressOutcomeFilter {
    fn as_str(self) -> &'static str {
        match self {
            EgressOutcomeFilter::Allowed => "allowed",
            EgressOutcomeFilter::Refused => "refused",
            EgressOutcomeFilter::Failed => "failed",
        }
    }
}

/// One recorded outbound connection (or several identical refusals,
/// collapsed: see `count`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressActivityRecord {
    /// When the connection started (the first, when collapsed).
    pub timestamp: DateTime<Utc>,
    /// `allowed`, `refused` or `failed`.
    pub outcome: String,
    /// Why it was refused or failed, e.g. `no_allow_rule`, `private_address`,
    /// `port_not_allowed`.
    pub reason: Option<String>,
    /// `tunnel` (HTTPS `CONNECT`), `forward` (plain HTTP), `relay` (a Temps
    /// relay) or `unknown`.
    pub kind: String,
    /// The destination host the sandbox asked for (`server.address`).
    pub host: Option<String>,
    /// The destination port (`server.port`).
    pub port: Option<u16>,
    /// The address the proxy connected to (`network.peer.address`).
    pub peer_address: Option<String>,
    /// Plain HTTP only: the method.
    pub method: Option<String>,
    /// Plain HTTP only: the path, never the query string.
    pub path: Option<String>,
    /// The Temps relay used, for `relay` connections.
    pub relay: Option<String>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub duration_ms: u64,
    /// How many identical connections this record stands for. Repeated
    /// refusals of the same destination are collapsed into one record.
    pub count: u64,
    /// When the last of the collapsed connections started.
    pub last_started_at: Option<DateTime<Utc>>,
    /// The node whose proxy observed it.
    pub node_name: Option<String>,
    /// Trace correlation, when the record carries it.
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl From<&LogRecord> for EgressActivityRecord {
    fn from(log: &LogRecord) -> Self {
        let text = |key: &str| log.attributes.get(key).cloned();
        let number = |key: &str| {
            log.attributes
                .get(key)
                .and_then(|value| value.parse::<u64>().ok())
        };
        Self {
            timestamp: log.timestamp,
            outcome: text("temps.egress.outcome").unwrap_or_default(),
            reason: text("temps.egress.reason"),
            kind: text("temps.egress.kind").unwrap_or_default(),
            host: text("server.address"),
            port: number("server.port").and_then(|port| u16::try_from(port).ok()),
            peer_address: text("network.peer.address"),
            method: text("http.request.method"),
            path: text("url.path"),
            relay: text("temps.egress.relay"),
            bytes_sent: number("temps.egress.bytes_sent").unwrap_or(0),
            bytes_received: number("temps.egress.bytes_received").unwrap_or(0),
            duration_ms: number("temps.egress.duration_ms").unwrap_or(0),
            count: number("temps.egress.count").unwrap_or(1),
            last_started_at: text("temps.egress.last_started_at")
                .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                .map(|value| value.with_timezone(&Utc)),
            node_name: text("temps.node.name"),
            trace_id: log.trace_id.clone(),
            span_id: log.span_id.clone(),
        }
    }
}

/// A page of a sandbox's recorded connections, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressActivityPage {
    /// The sandbox's public id.
    pub sandbox_id: String,
    pub records: Vec<EgressActivityRecord>,
    pub page: u64,
    pub per_page: u64,
    /// How many records match, across all pages.
    pub total: u64,
    /// Another page follows.
    pub has_more: bool,
}

impl EgressRecordingService {
    /// A page of the caller's sandbox's recorded outbound connections,
    /// newest first. `page` starts at 1; `per_page` defaults to 20 and is
    /// capped at 100.
    ///
    /// A sandbox without a project has no records (it cannot record), so
    /// its page is empty rather than an error.
    pub async fn activity(
        &self,
        public_id_value: &str,
        user_id: i32,
        page: Option<u64>,
        per_page: Option<u64>,
        outcome: Option<EgressOutcomeFilter>,
    ) -> Result<EgressActivityPage, EgressRecordingError> {
        let row = self.owned_sandbox(public_id_value, user_id).await?;
        let page = page.unwrap_or(1).max(1);
        let per_page = per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE);
        let empty = |total, has_more| EgressActivityPage {
            sandbox_id: row.public_id.clone(),
            records: Vec::new(),
            page,
            per_page,
            total,
            has_more,
        };
        let Some(project_id) = row.project_id else {
            return Ok(empty(0, false));
        };
        let Some(store) = &self.store else {
            return Err(EgressRecordingError::OtelUnavailable {
                sandbox_id: row.public_id.clone(),
            });
        };
        let query = LogQuery {
            project_id,
            sandbox_id: Some(row.id),
            attribute_filter: outcome.map(|outcome| LogAttributeFilter {
                key: "temps.egress.outcome".to_string(),
                ty: None,
                operator: LogAttributeOperator::default(),
                value: outcome.as_str().to_string(),
            }),
            // One more than the page, to know whether another follows.
            limit: Some(per_page + 1),
            offset: Some((page - 1).saturating_mul(per_page)),
            ..Default::default()
        };
        let unreadable = |source| EgressRecordingError::Query {
            sandbox_id: row.public_id.clone(),
            source,
        };
        let total = store.count(query.clone()).await.map_err(unreadable)?;
        let mut logs = store.query(query).await.map_err(unreadable)?;
        let has_more = logs.len() as u64 > per_page;
        logs.truncate(per_page as usize);
        // The column is the authority; the attribute is checked as well so
        // a record can never show on a sandbox it does not name.
        let records = logs
            .iter()
            .filter(|log| {
                log.attributes
                    .get(SANDBOX_ID_ATTRIBUTE)
                    .is_some_and(|value| *value == row.public_id)
            })
            .map(EgressActivityRecord::from)
            .collect();
        Ok(EgressActivityPage {
            records,
            ..empty(total, has_more)
        })
    }
}
