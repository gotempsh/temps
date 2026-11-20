// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Client Reports protocol 1.25.0. These are approximate SDK self-reports,
//! never server acceptance counts, billing totals, or recovered telemetry.
use chrono::{DateTime, Datelike, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientReport {
    /// Exact report bytes at a stable item position under a nonzero event ID.
    /// A standalone report has no identity: equal payloads must both count.
    pub source_key: Option<String>,
    pub entries: Vec<ClientDiscard>,
}

pub fn decode_report(
    payload: &[u8],
    event_id: Option<&str>,
    item_index: usize,
) -> Result<ClientReport, &'static str> {
    let entries = decode(payload)?;
    let source_key = event_id
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil())
        .map(|id| {
            use sha2::{Digest, Sha256};
            let mut hash = Sha256::new();
            hash.update(b"temps.sentry.client-report.v1\0");
            hash.update(id.as_bytes());
            hash.update((item_index as u64).to_be_bytes());
            hash.update(payload);
            hex::encode(hash.finalize())
        });
    Ok(ClientReport {
        source_key,
        entries,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientDiscard {
    pub reason: &'static str,
    pub category: &'static str,
    pub unit: &'static str,
    pub quantity: i64,
    pub reported_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct Report<'a> {
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    discarded_events: Vec<Discard>,
}
#[derive(Deserialize)]
struct Discard {
    reason: String,
    category: String,
    quantity: u64,
}

pub fn decode(payload: &[u8]) -> Result<Vec<ClientDiscard>, &'static str> {
    if payload.len() > 4096 {
        return Err("client report exceeds 4 KiB");
    }
    let report: Report<'_> =
        serde_json::from_slice(payload).map_err(|_| "invalid client report JSON/schema")?;
    if report.discarded_events.len() > 100 {
        return Err("at most 100 discarded outcomes per report");
    }
    let reported_at = match report.timestamp {
        None => Utc::now(),
        Some(raw) => {
            if raw.get().len() > 64 {
                return Err("invalid client report timestamp");
            }
            if raw.get().starts_with('"') {
                let text: String = serde_json::from_str(raw.get())
                    .map_err(|_| "invalid client report timestamp")?;
                DateTime::parse_from_rfc3339(&text)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|_| "invalid client report timestamp")?
            } else {
                super::logs::timestamp(raw.get()).ok_or("invalid client report timestamp")?
            }
        }
    };
    if !(1970..=9999).contains(&reported_at.year()) {
        return Err("client report timestamp outside supported range");
    }
    let mut discarded = Vec::with_capacity(report.discarded_events.len());
    for entry in report.discarded_events {
        if entry.reason.is_empty()
            || entry.category.is_empty()
            || entry.reason.len() > 128
            || entry.category.len() > 128
            || !entry
                .reason
                .bytes()
                .chain(entry.category.bytes())
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            || entry.quantity > 1_000_000_000_000
        {
            return Err("invalid client discard reason, category or quantity");
        }
        let reason = match entry.reason.as_str() {
            "before_send" => "before_send",
            "event_processor" => "event_processor",
            "sample_rate" => "sample_rate",
            "queue_overflow" => "queue_overflow",
            "ratelimit_backoff" => "ratelimit_backoff",
            "network_error" => "network_error",
            "send_error" => "send_error",
            "internal_sdk_error" => "internal_sdk_error",
            "insufficient_data" => "insufficient_data",
            "backpressure" => "backpressure",
            "buffer_overflow" => "buffer_overflow",
            "ignored" => "ignored",
            "invalid" => "invalid",
            "no_parent_span" => "no_parent_span",
            "callback_error" => "callback_error",
            _ => "other",
        };
        let category = match entry.category.as_str() {
            "error" => "error",
            "transaction" => "transaction",
            "span" => "span",
            "attachment" => "attachment",
            "session" => "session",
            "profile" => "profile",
            "profile_chunk" => "profile_chunk",
            "profile_chunk_ui" => "profile_chunk_ui",
            "profile_duration" => "profile_duration",
            "profile_duration_ui" => "profile_duration_ui",
            "profile_indexed" => "profile_indexed",
            "transaction_processed" => "transaction_processed",
            "transaction_indexed" => "transaction_indexed",
            "span_indexed" => "span_indexed",
            "attachment_item" => "attachment_item",
            "metric_bucket" => "metric_bucket",
            "replay" => "replay",
            "monitor" => "monitor",
            "feedback" => "feedback",
            "security" => "security",
            "default" => "default",
            "log_item" => "log_item",
            "log_byte" => "log_byte",
            "trace_metric" => "trace_metric",
            "trace_metric_byte" => "trace_metric_byte",
            _ => "other",
        };
        let unit = match category {
            "attachment" | "log_byte" | "trace_metric_byte" => "bytes",
            "profile_duration" | "profile_duration_ui" => "milliseconds",
            "other" | "default" => "unknown",
            _ => "items",
        };
        if entry.quantity > 0 {
            discarded.push(ClientDiscard {
                reason,
                category,
                unit,
                quantity: entry.quantity as i64,
                reported_at,
            });
        }
    }
    Ok(discarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn client_reports_keep_units_integer_precision_and_bound_cardinality() {
        let report = json!({"timestamp":"2026-10-20T10:00:00Z","discarded_events":[
            {"reason":"before_send","category":"error","quantity":3},
            {"reason":"buffer_overflow","category":"log_byte","quantity":900000000001_u64},
            {"reason":"future_reason","category":"future_category","quantity":2}]});
        let decoded = decode(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!((decoded[0].quantity, decoded[0].unit), (3, "items"));
        assert_eq!(
            (decoded[1].quantity, decoded[1].unit),
            (900000000001, "bytes")
        );
        assert_eq!(
            (decoded[2].reason, decoded[2].category, decoded[2].unit),
            ("other", "other", "unknown")
        );
        for quantity in [json!(-1), json!(1.5), json!(1_000_000_000_001_u64)] {
            assert!(decode(&serde_json::to_vec(&json!({"discarded_events":[{"reason":"before_send","category":"error","quantity":quantity}]})).unwrap()).is_err());
        }
        assert!(decode(br#"{"discarded_events":[]}"#).unwrap().is_empty());
        assert!(decode(&vec![b' '; 4097]).is_err());
        assert!(decode(br#"{"timestamp":null,"discarded_events":[]}"#).is_ok());
    }
}
