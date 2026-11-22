// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Envelope parsing using relay-event-schema types
//!
//! This module provides a lightweight envelope parser that uses the official
//! relay-event-schema types for parsing Sentry events, transactions, sessions, etc.
//!
//! Headers are newline-delimited JSON; length-delimited payloads may contain
//! arbitrary binary data or newlines. Lengths always count bytes:
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
use std::{collections::BTreeMap, fmt};
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
    #[error("envelope exceeds the 10 MiB body, 8 KiB header, or 1000 item limit")]
    LimitExceeded,
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

    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,

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

    #[serde(default, deserialize_with = "optional_length")]
    pub length: Option<usize>,
    pub content_type: Option<String>,
    pub item_count: Option<usize>,
    /// Retain extension headers (filename, attachment_type, etc.).
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

fn optional_length<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    usize::deserialize(d).map(Some)
}

/// Represents an item in a Sentry envelope
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum EnvelopeItem<'a> {
    /// Structured log batch, decoded by the native log adapter.
    Log(&'a [u8]),
    /// Unknown/unsupported payloads remain byte-exact and are never reported stored.
    Unsupported(&'a [u8]),
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
pub struct Envelope<'a> {
    header: EnvelopeHeaders,
    items: Vec<EnvelopeItem<'a>>,
    item_headers: Vec<ItemHeader>,
    item_payloads: Vec<&'a [u8]>,
}

impl<'a> Envelope<'a> {
    /// Parse an envelope from bytes
    pub fn from_slice(data: &'a [u8]) -> Result<Self, EnvelopeError> {
        Self::parse(data, true)
    }

    /// Frame every item, but decode only items handled by the active native
    /// adapters. Unsupported payload schemas must not block otherwise valid
    /// errors/logs (or allocate Relay objects that will immediately be dropped).
    pub(crate) fn parse_for_ingestion(data: &'a [u8]) -> Result<Self, EnvelopeError> {
        Self::parse(data, false)
    }

    fn parse(data: &'a [u8], decode_legacy: bool) -> Result<Self, EnvelopeError> {
        if data.len() > 10 * 1024 * 1024 {
            return Err(EnvelopeError::LimitExceeded);
        }
        if data.is_empty() {
            return Err(EnvelopeError::MissingHeader);
        }
        let (header_bytes, mut offset) = read_line(data, 0);
        if header_bytes.len() > MAX_ENVELOPE_HEADER_PEEK_BYTES {
            return Err(EnvelopeError::LimitExceeded);
        }
        let header: EnvelopeHeaders = serde_json::from_slice(header_bytes)
            .map_err(|_| EnvelopeError::InvalidHeader("invalid JSON object".into()))?;
        let mut items = Vec::new();
        let mut item_headers = Vec::new();
        let mut item_payloads = Vec::new();
        while offset < data.len() {
            if items.len() >= 1000 {
                return Err(EnvelopeError::LimitExceeded);
            }
            let (header_bytes, payload_offset) = read_line(data, offset);
            if header_bytes.len() > MAX_ENVELOPE_HEADER_PEEK_BYTES {
                return Err(EnvelopeError::LimitExceeded);
            }
            let mut item_header: ItemHeader =
                serde_json::from_slice(header_bytes).map_err(EnvelopeError::InvalidItemHeader)?;
            // Keep the actual future type name as well as the bounded enum used
            // for dispatch/diagnostics. Never use untrusted names as metric labels.
            if item_header.ty == ItemType::Unknown {
                let raw: serde_json::Value = serde_json::from_slice(header_bytes)
                    .map_err(EnvelopeError::InvalidItemHeader)?;
                if let Some(ty) = raw.get("type") {
                    item_header.other.insert("type".into(), ty.clone());
                }
            }
            if payload_offset == data.len() && !data.ends_with(b"\n") {
                return Err(EnvelopeError::UnexpectedEof);
            }
            let payload = match item_header.length {
                Some(length) => {
                    let end = payload_offset
                        .checked_add(length)
                        .filter(|end| *end <= data.len())
                        .ok_or(EnvelopeError::UnexpectedEof)?;
                    offset = end;
                    if end < data.len() {
                        if data[end] != b'\n' {
                            return Err(EnvelopeError::MissingNewline);
                        }
                        offset += 1;
                    }
                    &data[payload_offset..end]
                }
                None => {
                    let (payload, next) = read_line(data, payload_offset);
                    offset = next;
                    payload
                }
            };
            if !decode_legacy && !matches!(&item_header.ty, ItemType::Event | ItemType::Log) {
                items.push(EnvelopeItem::Unsupported(payload));
                item_headers.push(item_header);
                item_payloads.push(payload);
                continue;
            }
            // Parse item based on type
            let item = match &item_header.ty {
                ItemType::Event => {
                    let val: serde_json::Value = serde_json::from_slice(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse event: {}", e))
                    })?;
                    let mut event = Event::from_value(val.into());
                    apply_envelope_event_id(&mut event, header.event_id);
                    Some(EnvelopeItem::Event(event))
                }
                ItemType::Transaction => {
                    let val: serde_json::Value = serde_json::from_slice(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse transaction: {}", e))
                    })?;
                    let mut transaction = Event::from_value(val.into());
                    apply_envelope_event_id(&mut transaction, header.event_id);
                    Some(EnvelopeItem::Transaction(transaction))
                }
                ItemType::Session => {
                    let session = SessionUpdate::parse(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse session: {}", e))
                    })?;
                    Some(EnvelopeItem::Session(session))
                }
                ItemType::ClientReport => {
                    let report = ClientReport::parse(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!(
                            "Failed to parse client report: {}",
                            e
                        ))
                    })?;
                    Some(EnvelopeItem::ClientReport(report))
                }
                ItemType::Span => {
                    let val: serde_json::Value = serde_json::from_slice(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!("Failed to parse span: {}", e))
                    })?;
                    let span = Span::from_value(val.into());
                    Some(EnvelopeItem::Span(span))
                }
                ItemType::Sessions => {
                    let aggregates = SessionAggregates::parse(payload).map_err(|e| {
                        EnvelopeError::InvalidPayload(format!(
                            "Failed to parse session aggregates: {}",
                            e
                        ))
                    })?;
                    Some(EnvelopeItem::SessionAggregates(aggregates))
                }
                ItemType::Log => Some(EnvelopeItem::Log(payload)),
                // Preserve unsupported types for bounded dispatch outcomes.
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
                | ItemType::TraceMetric
                | ItemType::UserReportV2
                | ItemType::ProfileChunk
                | ItemType::Unknown => Some(EnvelopeItem::Unsupported(payload)),
            };

            if let Some(item) = item {
                items.push(item);
                item_headers.push(item_header);
                item_payloads.push(payload);
            }
        }

        Ok(Envelope {
            header,
            items,
            item_headers,
            item_payloads,
        })
    }

    pub fn items_with_headers(&self) -> impl Iterator<Item = (&ItemHeader, &EnvelopeItem<'a>)> {
        self.item_headers.iter().zip(self.items.iter())
    }

    /// Borrow original wire payloads without copying or normalizing unknown fields.
    pub fn items_with_payloads(
        &self,
    ) -> impl Iterator<Item = (&ItemHeader, &EnvelopeItem<'a>, &'a [u8])> {
        self.items_with_headers()
            .zip(self.item_payloads.iter())
            .map(|((header, item), payload)| (header, item, *payload))
    }

    /// Get the envelope header
    pub fn event_id(&self) -> Option<String> {
        self.header.event_id.map(|id| id.to_string())
    }

    pub fn header(&self) -> &EnvelopeHeaders {
        &self.header
    }

    /// Iterate over envelope items
    pub fn items(&self) -> impl Iterator<Item = &EnvelopeItem<'a>> {
        self.items.iter()
    }
}

/// Read a UNIX-delimited line without interpreting payload bytes as text.
fn read_line(data: &[u8], offset: usize) -> (&[u8], usize) {
    match data[offset..].iter().position(|b| *b == b'\n') {
        Some(length) => (&data[offset..offset + length], offset + length + 1),
        None => (&data[offset..], data.len()),
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
    fn active_dispatch_does_not_validate_unsupported_payload_schemas() {
        let data = b"{}\n{\"type\":\"session\",\"length\":3}\n\xff\x00\n\n{\"type\":\"event\"}\n{\"event_id\":\"11111111111111111111111111111111\",\"level\":\"error\"}";
        let envelope = Envelope::parse_for_ingestion(data).unwrap();
        assert_eq!(envelope.items().count(), 2);
        assert!(matches!(
            envelope.items().next(),
            Some(EnvelopeItem::Unsupported(_))
        ));
        assert!(Envelope::from_slice(data).is_err());
    }

    #[test]
    fn byte_framing_preserves_binary_multiline_headers_and_following_error() {
        let payload = b"\xff\x00first\nsecond\r\n";
        let mut data = format!("{{\"event_id\":\"11111111111111111111111111111111\"}}\n{{\"type\":\"attachment\",\"length\":{},\"filename\":\"synthetic.bin\",\"attachment_type\":\"event.attachment\"}}\n", payload.len()).into_bytes();
        data.extend_from_slice(payload);
        data.extend_from_slice(
            b"\n{\"type\":\"event\"}\n{\"message\":\"Synthetic error\",\"level\":\"error\"}",
        );
        data.extend_from_slice(b"\n{\"type\":\"log\",\"item_count\":0,\"content_type\":\"application/vnd.sentry.items.log+json\"}\n{\"items\":[]}");
        let envelope = Envelope::from_slice(&data).unwrap();
        let items: Vec<_> = envelope.items_with_headers().collect();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].0.other["filename"], "synthetic.bin");
        assert!(matches!(items[0].1, EnvelopeItem::Unsupported(bytes) if *bytes == payload));
        assert!(matches!(items[1].1, EnvelopeItem::Event(_)));
        assert!(matches!(items[2].1, EnvelopeItem::Log(bytes) if *bytes == b"{\"items\":[]}"));
    }

    #[test]
    fn length_is_bytes_not_characters_and_allows_multiline_json() {
        let payload = "{\n\"message\":\"Synthetic ☃\",\n\"level\":\"error\"\n}";
        let data = format!(
            "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
            payload.len(),
            payload
        );
        assert_eq!(
            Envelope::from_slice(data.as_bytes())
                .unwrap()
                .items()
                .count(),
            1
        );
    }

    #[test]
    fn implicit_payload_preserves_carriage_return_and_accepts_eof() {
        for data in [
            b"{}\n{\"type\":\"future\"}\nabc\r\n".as_slice(),
            b"{}\n{\"type\":\"future\"}\nabc\r",
        ] {
            let envelope = Envelope::from_slice(data).unwrap();
            let (header, item) = envelope.items_with_headers().next().unwrap();
            assert_eq!(header.other["type"], "future");
            assert!(matches!(item, EnvelopeItem::Unsupported(bytes) if *bytes == b"abc\r"));
        }
    }

    #[test]
    fn empty_envelopes_and_explicit_zero_length_payloads() {
        for data in [b"{}".as_slice(), b"{}\n"] {
            assert_eq!(Envelope::from_slice(data).unwrap().items().count(), 0);
        }
        for suffix in ["", "\n", "\n{\"type\":\"future\",\"length\":0}\n"] {
            let data = format!("{{}}\n{{\"type\":\"attachment\",\"length\":0}}\n{suffix}");
            assert!(Envelope::from_slice(data.as_bytes()).is_ok(), "{suffix:?}");
        }
    }

    #[test]
    fn malformed_frames_are_rejected_without_resynchronizing_into_payloads() {
        for data in [
            "",
            "[]",
            "{}\n\n",
            "{}\nnot-json\n{}",
            "{}\n{\"type\":\"attachment\",\"length\":null}\nx",
            "{}\n{\"type\":\"attachment\",\"length\":-1}\nx",
            "{}\n{\"type\":\"attachment\",\"length\":1.5}\nx",
            "{}\n{\"type\":\"attachment\",\"length\":999999999999999999999999999}\nx",
            "{}\n{\"type\":\"attachment\",\"length\":4}\nabc",
            "{}\n{\"type\":\"attachment\",\"length\":1}\nab",
            "{}\n{\"type\":\"attachment\",\"length\":1}\na\r\n",
            "{}\n{\"type\":\"attachment\",\"length\":0}",
        ] {
            assert!(Envelope::from_slice(data.as_bytes()).is_err(), "{data:?}");
        }
        assert!(Envelope::from_slice(b"{}\n{\"type\":\"event\"}\n\xff").is_err());
    }

    #[test]
    fn byte_header_and_item_limits_apply_to_unknown_items_too() {
        assert!(matches!(
            Envelope::from_slice(&vec![b'x'; 10 * 1024 * 1024 + 1]),
            Err(EnvelopeError::LimitExceeded)
        ));
        let data = format!("{{\"padding\":\"{}\"}}", "x".repeat(8192));
        assert!(matches!(
            Envelope::from_slice(data.as_bytes()),
            Err(EnvelopeError::LimitExceeded)
        ));
        let data = format!(
            "{{}}\n{}",
            "{\"type\":\"future\",\"length\":0}\n\n".repeat(1001)
        );
        assert!(matches!(
            Envelope::from_slice(data.as_bytes()),
            Err(EnvelopeError::LimitExceeded)
        ));
    }

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
        let envelope = Envelope::from_slice(br#"{}
{"type":"event"}
{"exception":{"values":[{"type":"SyntheticError","value":"Synthetic failure","stacktrace":{"frames":[{"filename":"app:///application.js","function":"syntheticOperation","lineno":7,"in_app":true}]}}]}}
"#).unwrap();
        let EnvelopeItem::Event(event) = envelope.items().next().unwrap() else {
            panic!("expected error event");
        };
        let event = event.value().unwrap();
        let exception = event.exceptions.value().unwrap().values.value().unwrap()[0]
            .value()
            .unwrap();
        let frame = exception
            .stacktrace
            .value()
            .unwrap()
            .frames
            .value()
            .unwrap()[0]
            .value()
            .unwrap();
        assert_eq!(
            frame.filename.value().map(|path| path.as_str()),
            Some("app:///application.js")
        );
        assert_eq!(frame.function.as_str(), Some("syntheticOperation"));
        assert_eq!(frame.lineno.value(), Some(&7));
    }
}
