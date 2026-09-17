// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    /// Raw compressed Unreal Engine 4 crash report.
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
#[allow(clippy::large_enum_variant)]
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

/// Upper bound on the envelope header line inspected by [`peek_envelope_dsn`].
///
/// Sentry envelope headers are a few hundred bytes. Refusing to hand anything
/// larger to `serde_json` keeps the tunnel's credential sniff O(1) on an
/// ingest path that anyone on the internet can reach.
const MAX_ENVELOPE_HEADER_PEEK_BYTES: usize = 8 * 1024;

/// Minimal view of an envelope header used to recover the originating DSN.
///
/// Deliberately separate from [`EnvelopeHeaders`]: this is parsed *before*
/// the envelope is authenticated, so it must stay as small and as cheap as
/// possible, and it must not force the full envelope to be parsed twice.
#[derive(Deserialize)]
struct EnvelopeDsnPeek {
    #[serde(default)]
    dsn: Option<String>,
}

/// Read only the `dsn` field of an envelope header, without parsing the rest
/// of the envelope.
///
/// Browser SDKs configured with `Sentry.init({ tunnel })` embed the full DSN
/// in the envelope header precisely so the tunnel endpoint can tell which
/// project the payload belongs to. Returns `None` when the header is absent,
/// oversized, not valid UTF-8, not valid JSON, or carries no `dsn` field --
/// every one of those is "no credential offered", never an error, because the
/// caller falls back to `Host` resolution in that case.
pub fn peek_envelope_dsn(data: &[u8]) -> Option<String> {
    let header_line = match data.iter().position(|byte| *byte == b'\n') {
        Some(index) => &data[..index],
        None => data,
    };

    if header_line.is_empty() || header_line.len() > MAX_ENVELOPE_HEADER_PEEK_BYTES {
        return None;
    }

    let header_line = std::str::from_utf8(header_line).ok()?.trim_end();

    serde_json::from_str::<EnvelopeDsnPeek>(header_line)
        .ok()?
        .dsn
        .filter(|dsn| !dsn.is_empty())
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
                    let mut event = Event::from_value(val.into());
                    apply_envelope_event_id(&mut event, header.event_id);
                    Some(EnvelopeItem::Event(event))
                }
                ItemType::Transaction => {
                    let val: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse transaction: {}", e))
                    })?;
                    let mut transaction = Event::from_value(val.into());
                    apply_envelope_event_id(&mut transaction, header.event_id);
                    Some(EnvelopeItem::Transaction(transaction))
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

/// Apply the canonical envelope event ID to an event or transaction payload.
///
/// Sentry requires the ID in the envelope header and permits payloads to omit
/// it. When both locations contain an ID, the envelope header takes precedence.
fn apply_envelope_event_id(event: &mut Annotated<Event>, event_id: Option<EventId>) {
    if let (Some(event), Some(event_id)) = (event.value_mut(), event_id) {
        event.id.set_value(Some(event_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_reads_dsn_from_envelope_header() {
        let data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"dsn\":\"https://abc123@temps.example/7\"}\n{\"type\":\"event\"}\n{}\n";
        assert_eq!(
            peek_envelope_dsn(data.as_bytes()),
            Some("https://abc123@temps.example/7".to_string())
        );
    }

    #[test]
    fn peek_returns_none_without_a_dsn_field() {
        let data =
            "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{}\n";
        assert_eq!(peek_envelope_dsn(data.as_bytes()), None);
    }

    #[test]
    fn peek_returns_none_for_junk_input() {
        assert_eq!(peek_envelope_dsn(b""), None);
        assert_eq!(peek_envelope_dsn(b"not json\n{}\n"), None);
        assert_eq!(peek_envelope_dsn(&[0xff, 0xfe, b'\n']), None);
        // An empty `dsn` is "no credential offered", not an empty credential.
        assert_eq!(peek_envelope_dsn(b"{\"dsn\":\"\"}\n"), None);
    }

    #[test]
    fn peek_ignores_an_oversized_header_line() {
        let mut data = format!(
            "{{\"dsn\":\"https://abc123@temps.example/7\",\"pad\":\"{}\"}}",
            "x".repeat(9000)
        );
        data.push('\n');
        assert_eq!(peek_envelope_dsn(data.as_bytes()), None);
    }

    #[test]
    fn peek_does_not_read_past_the_header_line() {
        // A `dsn` appearing in an item payload must never be mistaken for the
        // envelope's own credential.
        let data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n{\"type\":\"event\"}\n{\"dsn\":\"https://forged@temps.example/1\"}\n";
        assert_eq!(peek_envelope_dsn(data.as_bytes()), None);
    }

    #[test]
    fn test_parse_simple_envelope() {
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n\
            {\"type\":\"event\"}\n\
            {\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"level\":\"error\",\"platform\":\"other\"}\n";

        let envelope = Envelope::from_slice(envelope_data.as_bytes());
        assert!(envelope.is_ok(), "Should parse simple envelope");

        let envelope = envelope.unwrap();
        assert_eq!(envelope.items().count(), 1);
    }

    #[test]
    fn event_uses_event_id_from_envelope_header() {
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n\
            {\"type\":\"event\",\"content_type\":\"application/json\"}\n\
            {\"level\":\"error\",\"platform\":\"php\",\"message\":\"Synthetic test\"}\n";

        let envelope = Envelope::from_slice(envelope_data.as_bytes())
            .unwrap_or_else(|error| panic!("PHP SDK envelope should parse: {error}"));
        let event = match envelope.items().next() {
            Some(EnvelopeItem::Event(event)) => event,
            _ => panic!("expected one event item"),
        };

        assert_eq!(
            event
                .value()
                .and_then(|event| event.id.value())
                .map(ToString::to_string)
                .as_deref(),
            Some("9ec79c33ec9942ab8353589fcb2e04dc")
        );
    }

    #[test]
    fn envelope_header_event_id_takes_precedence_over_event_payload() {
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n\
            {\"type\":\"event\"}\n\
            {\"event_id\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"level\":\"error\",\"platform\":\"other\"}\n";

        let envelope = Envelope::from_slice(envelope_data.as_bytes())
            .unwrap_or_else(|error| panic!("event envelope should parse: {error}"));
        let event = match envelope.items().next() {
            Some(EnvelopeItem::Event(event)) => event,
            _ => panic!("expected one event item"),
        };

        assert_eq!(
            event
                .value()
                .and_then(|event| event.id.value())
                .map(ToString::to_string)
                .as_deref(),
            Some("9ec79c33ec9942ab8353589fcb2e04dc")
        );
    }

    #[test]
    fn transaction_uses_event_id_from_envelope_header() {
        let envelope_data = "{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\"}\n\
            {\"type\":\"transaction\"}\n\
            {\"type\":\"transaction\",\"transaction\":\"GET /example\",\"platform\":\"php\"}\n";

        let envelope = Envelope::from_slice(envelope_data.as_bytes())
            .unwrap_or_else(|error| panic!("transaction envelope should parse: {error}"));
        let transaction = match envelope.items().next() {
            Some(EnvelopeItem::Transaction(transaction)) => transaction,
            _ => panic!("expected one transaction item"),
        };

        assert_eq!(
            transaction
                .value()
                .and_then(|event| event.id.value())
                .map(ToString::to_string)
                .as_deref(),
            Some("9ec79c33ec9942ab8353589fcb2e04dc")
        );
    }

    #[test]
    fn parse_event_with_stacktrace() {
        let event_json = include_str!("../../resources/stacktrace_event.json");
        let event_value: serde_json::Value = serde_json::from_str(event_json).unwrap();
        let event_value = Event::from_value(event_value.into());
        let _event = event_value
            .value()
            .unwrap_or_else(|| panic!("Should parse event"));
        println!("{}", event_value.to_json_pretty().unwrap());
        // let stacktrace = event.stacktrace.value();
        // assert!(stacktrace.is_some(), "Should parse stacktrace");
        // let stacktrace = stacktrace.unwrap();
        // println!("Stacktrace: {:?}", stacktrace);
    }
}
