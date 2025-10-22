//! Envelope parsing using relay-event-schema types
//!
//! This module provides a lightweight envelope parser that uses the official
//! relay-event-schema types for parsing Sentry events, transactions, sessions, etc.
//!
//! The Sentry envelope format is a simple text-based protocol:
//! ```text
//! {envelope_header}\n
//! {item_header}\n
//! {item_payload}\n
//! {item_header}\n
//! {item_payload}\n
//! ...
//! ```

use chrono::{DateTime, Utc};
use relay_event_schema::protocol::{
    ClientReport, Event, EventId, SessionAggregates, SessionUpdate, Span,
};
use relay_protocol::{Annotated, FromValue};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EnvelopeError {
    #[error("unexpected end of file")]
    UnexpectedEof,
    #[error("missing envelope header")]
    MissingHeader,
    #[error("missing newline after header or payload")]
    MissingNewline,
    #[error("invalid envelope header")]
    InvalidHeader(String),
    #[error("{0} header mismatch between envelope and request")]
    HeaderMismatch(&'static str),
    #[error("invalid item header")]
    InvalidItemHeader(#[source] serde_json::Error),
    #[error("internal/reserved item type used")]
    InternalItemType,
    #[error("failed to write header")]
    HeaderIoFailed(#[source] serde_json::Error),
    #[error("failed to write payload")]
    PayloadIoFailed(#[source] std::io::Error),
    #[error("Invalid item payload: {0}")]
    InvalidPayload(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EnvelopeHeaders {
    /// Unique identifier of the event associated to this envelope.
    ///
    /// Envelopes without contained events do not contain an event id.  This is for instance
    /// the case for session metrics.
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<EventId>,

    /// Data retention in days for the items of this envelope.
    ///
    /// This value is always overwritten in processing mode by the value specified in the project
    /// configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retention: Option<u16>,

    /// Data retention in days for the items of this envelope.
    ///
    /// This value is always overwritten in processing mode by the value specified in the project
    /// configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    downsampled_retention: Option<u16>,

    /// Timestamp when the event has been sent, according to the SDK.
    ///
    /// This can be used to perform drift correction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_at: Option<DateTime<Utc>>,
}

/// The type of an envelope item.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    /// Event payload encoded in JSON.
    Event,
    /// Transaction event payload encoded in JSON.
    Transaction,
    /// Security report event payload encoded in JSON.
    Security,
    /// Raw payload of an arbitrary attachment.
    Attachment,
    /// Multipart form data collected into a stream of JSON tuples.
    FormData,
    /// Security report as sent by the browser in JSON.
    RawSecurity,
    /// NEL report as sent by the browser.
    Nel,
    /// Raw compressed UE4 crash report.
    UnrealReport,
    /// User feedback encoded as JSON.
    UserReport,
    /// Session update data.
    Session,
    /// Aggregated session data.
    Sessions,
    /// Individual metrics in text encoding.
    Statsd,
    /// Buckets of preaggregated metrics encoded as JSON.
    MetricBuckets,
    /// Client internal report (eg: outcomes).
    ClientReport,
    /// Profile event payload encoded as JSON.
    Profile,
    /// Replay metadata and breadcrumb payload.
    ReplayEvent,
    /// Replay Recording data.
    ReplayRecording,
    /// Replay Video data.
    ReplayVideo,
    /// Monitor check-in encoded as JSON.
    CheckIn,
    /// A log for the log product, not internal logs.
    Log,
    /// A trace metric item.
    TraceMetric,
    /// A standalone span.
    Span,
    /// UserReport as an Event
    #[serde(rename = "feedback")]
    UserReportV2,
    /// ProfileChunk is a chunk of a profiling session.
    ProfileChunk,
    /// A new item type that is yet unknown by this version of Relay.
    #[serde(other)]
    Unknown,
}

impl ItemType {
    /// Returns the variant name of the item type.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Transaction => "transaction",
            Self::Security => "security",
            Self::Attachment => "attachment",
            Self::FormData => "form_data",
            Self::RawSecurity => "raw_security",
            Self::Nel => "nel",
            Self::UnrealReport => "unreal_report",
            Self::UserReport => "user_report",
            Self::UserReportV2 => "feedback",
            Self::Session => "session",
            Self::Sessions => "sessions",
            Self::Statsd => "statsd",
            Self::MetricBuckets => "metric_buckets",
            Self::ClientReport => "client_report",
            Self::Profile => "profile",
            Self::ReplayEvent => "replay_event",
            Self::ReplayRecording => "replay_recording",
            Self::ReplayVideo => "replay_video",
            Self::CheckIn => "check_in",
            Self::Log => "log",
            Self::TraceMetric => "trace_metric",
            Self::Span => "span",
            Self::ProfileChunk => "profile_chunk",
            Self::Unknown => "unknown",
        }
    }

    /// Returns the item type as a string.
    pub fn as_str(&self) -> &str {
        self.name()
    }
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemHeader {
    #[serde(rename = "type")]
    pub ty: ItemType,

    #[serde(default)]
    pub length: Option<usize>,
}

/// Represents an item in a Sentry envelope
#[derive(Debug)]
pub enum EnvelopeItem {
    /// Error event
    Event(Annotated<Event>),

    /// Transaction event
    Transaction(Annotated<Event>),

    /// Session update
    Session(SessionUpdate),

    /// Client report (acknowledged but not processed)
    ClientReport(ClientReport),

    /// Span (standalone)
    Span(Annotated<Span>),

    /// SessionAggregates
    SessionAggregates(SessionAggregates),
}

#[derive(Debug)]
/// A parsed Sentry envelope
pub struct Envelope {
    header: EnvelopeHeaders,
    items: Vec<EnvelopeItem>,
}

impl Envelope {
    /// Parse an envelope from bytes
    pub fn from_slice(data: &[u8]) -> Result<Self, EnvelopeError> {
        let mut lines = Vec::new();

        // Split by newlines
        let text = String::from_utf8_lossy(data);
        for line in text.lines() {
            lines.push(line);
        }

        if lines.is_empty() {
            return Err(EnvelopeError::InvalidHeader("Empty envelope".to_string()));
        }

        // Parse envelope header (first line)
        let header: EnvelopeHeaders = serde_json::from_str(lines[0])
            .map_err(|e| EnvelopeError::InvalidHeader(format!("Failed to parse header: {}", e)))?;

        let mut items = Vec::new();
        let mut i = 1;

        // Parse items
        while i < lines.len() {
            if lines[i].trim().is_empty() {
                i += 1;
                continue;
            }

            // Parse item header
            let item_header: ItemHeader = match serde_json::from_str(lines[i]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse item header at line {}: {}. Skipping.",
                        i,
                        e
                    );
                    i += 1;
                    continue;
                }
            };

            i += 1;

            // Get item payload
            if i >= lines.len() {
                tracing::warn!("Item header without payload at line {}. Skipping.", i - 1);
                break;
            }

            let payload = lines[i];
            i += 1;

            // Parse item based on type
            let item = match &item_header.ty {
                ItemType::Event => {
                    let val: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse event: {}", e))
                    })?;
                    Some(EnvelopeItem::Event(Event::from_value(val.into())))
                }
                ItemType::Transaction => {
                    let val: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse transaction: {}", e))
                    })?;
                    Some(EnvelopeItem::Transaction(Event::from_value(val.into())))
                }
                ItemType::Session => {
                    let session = SessionUpdate::parse(payload.as_bytes()).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse session: {}", e))
                    })?;
                    Some(EnvelopeItem::Session(session))
                }
                ItemType::ClientReport => {
                    let report = ClientReport::parse(payload.as_bytes()).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!(
                            "Failed to parse client report: {}",
                            e
                        ))
                    })?;
                    Some(EnvelopeItem::ClientReport(report))
                }
                ItemType::Span => {
                    let val: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse span: {}", e))
                    })?;
                    let span = Span::from_value(val.into());
                    Some(EnvelopeItem::Span(span))
                }
                ItemType::Sessions => {
                    let aggregates = SessionAggregates::parse(payload.as_bytes()).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!(
                            "Failed to parse session aggregates: {}",
                            e
                        ))
                    })?;
                    Some(EnvelopeItem::SessionAggregates(aggregates))
                }
                // Unimplemented item types - skip them
                ItemType::Security
                | ItemType::Attachment
                | ItemType::FormData
                | ItemType::RawSecurity
                | ItemType::Nel
                | ItemType::UnrealReport
                | ItemType::UserReport
                | ItemType::Statsd
                | ItemType::MetricBuckets
                | ItemType::Profile
                | ItemType::ReplayEvent
                | ItemType::ReplayRecording
                | ItemType::ReplayVideo
                | ItemType::CheckIn
                | ItemType::Log
                | ItemType::TraceMetric
                | ItemType::UserReportV2
                | ItemType::ProfileChunk
                | ItemType::Unknown => {
                    tracing::debug!("Skipping unimplemented item type: {}", item_header.ty);
                    None
                }
            };

            if let Some(item) = item {
                items.push(item);
            }
        }

        Ok(Envelope { header, items })
    }

    /// Get the envelope header
    pub fn header(&self) -> &EnvelopeHeaders {
        &self.header
    }

    /// Iterate over envelope items
    pub fn items(&self) -> impl Iterator<Item = &EnvelopeItem> {
        self.items.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    //     #[test]
    //     fn test_parse_sentry_sdk_envelope() {
    //         let envelope_data = include_str!("../../../sentry_debug/envelope_1760375895525.bin");
    //         let envelope = Envelope::from_slice(envelope_data.as_bytes());
    //         let envelope = envelope.unwrap();
    //         let items = envelope.items();
    //         for item in items {
    //             println!("item: {:?}", item);
    //         }
    //         println!("envelope: {:?}", envelope.header());
    //     }

    //     #[test]
    //     fn test_parse_simple_envelope() {
    //         let envelope_data = r#"{"event_id":"9ec79c33ec9942ab8353589fcb2e04dc"}
    // {"type":"event"}
    // {"event_id":"9ec79c33ec9942ab8353589fcb2e04dc","level":"error","platform":"other"}
    // "#;

    //         let envelope = Envelope::from_slice(envelope_data.as_bytes());
    //         assert!(envelope.is_ok(), "Should parse simple envelope");

    //         let envelope = envelope.unwrap();
    //         assert_eq!(envelope.items().count(), 1);
    //     }

    //     #[test]
    //     fn test_parse_real_event() {
    //         // Test with real Sentry event
    //         let event_json = include_str!("../../../sentry_debug/event_20251013_132350_265.json");

    //         // Wrap in envelope format with a real UUID
    //         let real_uuid = uuid::Uuid::new_v4().to_string();
    //         let envelope_data = format!(
    //             "{{\"event_id\":\"{}\"}}\n{{\"type\":\"event\"}}\n{}\n",
    //             real_uuid, event_json
    //         );
    //         println!("{}", envelope_data);
    //         let envelope = Envelope::from_slice(envelope_data.as_bytes());

    //         // Should parse (relay types handle the complex structure)
    //         match &envelope {
    //             Ok(env) => {
    //                 println!("✅ Parsed envelope with {} items", env.items().count());
    //                 for item in env.items() {
    //                     match item {
    //                         EnvelopeItem::Event(_) => println!("  - Event item"),
    //                         EnvelopeItem::Transaction(_) => println!("  - Transaction item"),
    //                         EnvelopeItem::Session(_) => println!("  - Session item"),
    //                         EnvelopeItem::SessionAggregates(_) => println!("  - Sessions item"),
    //                         EnvelopeItem::ClientReport(_) => println!("  - ClientReport item"),
    //                         EnvelopeItem::Span(_) => println!("  - Span item"),
    //                     }
    //                 }
    //             }
    //             Err(e) => {
    //                 println!("❌ Failed to parse: {}", e);
    //             }
    //         }
    //     }
    #[test]
    fn parse_event_with_stacktrace() {
        let event_json = include_str!("../../resources/stacktrace_event.json");
        let event_value: serde_json::Value = serde_json::from_str(event_json).unwrap();
        let event_value = Event::from_value(event_value.into());
        let event = event_value
            .value()
            .unwrap_or_else(|| panic!("Should parse event"));
        println!("{}", event_value.to_json_pretty().unwrap());
        // let stacktrace = event.stacktrace.value();
        // assert!(stacktrace.is_some(), "Should parse stacktrace");
        // let stacktrace = stacktrace.unwrap();
        // println!("Stacktrace: {:?}", stacktrace);
    }
}
