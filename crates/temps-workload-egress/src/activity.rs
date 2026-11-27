// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One workload's recorded outbound connections, read for its own page.
//!
//! The records are the same OpenTelemetry log records the project's logs
//! show (see [`crate::records`]). They are read here by the workload
//! columns the platform stores them with, newest first, so a workload's
//! activity never needs its project's logs filtered by attribute, and a
//! workload without a project still has its activity.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_otel::log_attributes::{LogAttributeFilter, LogAttributeOperator};
use temps_otel::types::{LogQuery, LogRecord};
use utoipa::ToSchema;

use temps_workload::WorkloadRef;

use crate::adapter::RecordedWorkload;
use crate::error::EgressRecordingError;
use crate::service::EgressRecordingService;

/// Records per page when the caller does not say.
pub const DEFAULT_PER_PAGE: u64 = 20;
/// The most records one page returns.
pub const MAX_PER_PAGE: u64 = 100;

/// Only the connections that ended this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
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
    /// The destination host the workload asked for (`server.address`).
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

/// A page of a workload's recorded connections, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressActivityPage {
    /// What the workload is: `sandbox`, or `deployment` for an
    /// environment's deployments.
    pub workload_kind: String,
    /// The workload's id in its own API (`sbx_...`, an environment id).
    pub workload_id: String,
    pub records: Vec<EgressActivityRecord>,
    pub page: u64,
    pub per_page: u64,
    /// How many records match, across all pages.
    pub total: u64,
    /// Another page follows.
    pub has_more: bool,
}

/// How long a workload's record count is reused. The count is an exact
/// scan of everything the workload recorded; the console asks for the first
/// page every 10 seconds while recording is on, from every tab. The records
/// on a page are always read fresh: only "of N" can lag, by this much.
pub const TOTAL_REUSE: Duration = Duration::from_secs(30);

/// The most counts kept; past it the expired ones are dropped, and all of
/// them if none has expired. One per workload and filter being viewed.
const MAX_CACHED_TOTALS: usize = 1024;

/// What a count is kept for: a workload and an outcome filter.
type TotalKey = (WorkloadRef, Option<EgressOutcomeFilter>);

/// Recent record counts, with when each was counted.
#[derive(Default)]
pub(crate) struct RecentTotals(std::sync::Mutex<HashMap<TotalKey, (u64, Instant)>>);

impl RecentTotals {
    fn get(&self, workload: WorkloadRef, outcome: Option<EgressOutcomeFilter>) -> Option<u64> {
        let totals = self.0.lock().unwrap_or_else(|e| e.into_inner());
        totals
            .get(&(workload, outcome))
            .filter(|(_, at)| at.elapsed() < TOTAL_REUSE)
            .map(|(total, _)| *total)
    }

    fn put(&self, workload: WorkloadRef, outcome: Option<EgressOutcomeFilter>, total: u64) {
        let mut totals = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if totals.len() >= MAX_CACHED_TOTALS {
            totals.retain(|_, (_, at)| at.elapsed() < TOTAL_REUSE);
            if totals.len() >= MAX_CACHED_TOTALS {
                totals.clear();
            }
        }
        totals.insert((workload, outcome), (total, Instant::now()));
    }

    #[cfg(test)]
    pub(crate) fn expire(&self) {
        let mut totals = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for (_, at) in totals.values_mut() {
            *at = Instant::now() - TOTAL_REUSE;
        }
    }
}

impl EgressRecordingService {
    /// A page of `workload`'s recorded outbound connections, newest first.
    /// The caller has resolved the workload and checked the user may see
    /// it. `page` starts at 1; `per_page` defaults to 20 and is capped at
    /// 100.
    pub async fn activity(
        &self,
        workload: &RecordedWorkload,
        page: Option<u64>,
        per_page: Option<u64>,
        outcome: Option<EgressOutcomeFilter>,
    ) -> Result<EgressActivityPage, EgressRecordingError> {
        let adapter = self.adapter(workload.workload.kind)?;
        let page = page.unwrap_or(1).max(1);
        let per_page = per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE);
        let Some(store) = &self.store else {
            return Err(EgressRecordingError::OtelUnavailable {
                noun: adapter.noun(),
                id: workload.public_id.clone(),
            });
        };
        // A page past what any store can address (the offset is a signed
        // 64-bit integer in SQL) holds nothing: it is answered as empty,
        // with the real total, rather than sent as a negative offset.
        let offset = (page - 1)
            .checked_mul(per_page)
            .filter(|offset| i64::try_from(*offset).is_ok());
        let query = LogQuery {
            // Read by the workload alone; the project is not applied.
            project_id: workload.project_id.unwrap_or_default(),
            workload: Some(workload.workload),
            // The caller authorized this workload itself.
            workload_in_any_project: true,
            attribute_filter: outcome.map(|outcome| LogAttributeFilter {
                key: "temps.egress.outcome".to_string(),
                ty: None,
                operator: LogAttributeOperator::default(),
                value: outcome.as_str().to_string(),
            }),
            // One more than the page, to know whether another follows.
            limit: Some(per_page + 1),
            offset: Some(offset.unwrap_or(0)),
            ..Default::default()
        };
        let unreadable = |source| EgressRecordingError::Query {
            noun: adapter.noun(),
            id: workload.public_id.clone(),
            source,
        };
        let total = match self.totals.get(workload.workload, outcome) {
            Some(total) => total,
            None => {
                let total = store.count(query.clone()).await.map_err(unreadable)?;
                self.totals.put(workload.workload, outcome, total);
                total
            }
        };
        if offset.is_none() {
            return Ok(EgressActivityPage {
                workload_kind: workload.workload.kind.as_str().to_string(),
                workload_id: workload.public_id.clone(),
                records: Vec::new(),
                page,
                per_page,
                total,
                has_more: false,
            });
        }
        let mut logs = store.query(query).await.map_err(unreadable)?;
        let has_more = logs.len() as u64 > per_page;
        logs.truncate(per_page as usize);
        // The columns are the authority; the attribute is checked as well
        // so a record can never show on a workload it does not name.
        let records = logs
            .iter()
            .filter(|log| {
                log.attributes
                    .get(adapter.id_attribute())
                    .is_some_and(|value| *value == workload.public_id)
            })
            .map(EgressActivityRecord::from)
            .collect();
        Ok(EgressActivityPage {
            workload_kind: workload.workload.kind.as_str().to_string(),
            workload_id: workload.public_id.clone(),
            records,
            page,
            per_page,
            total,
            has_more,
        })
    }
}
