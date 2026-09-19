// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire frames for console access through Temps Cloud (ADR-045 §2).
//!
//! A linked instance that enables `cloud.console_access_enabled` opens a
//! second, dedicated outbound WebSocket (`{backend}/v1/console-proxy`),
//! negotiated with [`crate::Capability::ConsoleProxy`] in its own [`crate::Hello`]
//! — never layered onto the heartbeat connection, for the same
//! head-of-line-blocking reason heartbeat is already split out on its own.
//! Cloud then relays a browser's HTTP/WebSocket traffic down that connection
//! to an in-process router handle on the instance, with no inbound port ever
//! opened on the instance.
//!
//! This module is the single source of truth for that wire contract; the
//! Cloud backend pins this crate rather than maintaining a parallel
//! definition, exactly as it already does for [`crate::messages::SpanRecord`]
//! and [`crate::Capability`].
//!
//! # Two frame families
//!
//! **Control frames** are ordinary [`crate::messages::Envelope`]s
//! (`Message::Text`, JSON) — one open/close/flow-control message per
//! stream-level event. Every control frame type in this module exposes a
//! `KIND` associated constant naming its `Envelope::kind`, so both sides
//! build and parse them the same way every other envelope on this channel
//! already is: `Envelope::new(ConsoleStreamOpen::KIND, &open)` to encode,
//! `envelope.decode::<ConsoleStreamOpen>(ConsoleStreamOpen::KIND)` to parse.
//! An unknown `kind` is dropped, never a fatal parse error, matching this
//! crate's stated forward-compatibility rule.
//!
//! **Data frames** carry no JSON and no base64. Request-body chunks,
//! response-body chunks, and post-upgrade relay bytes are `Message::Binary`
//! with a fixed [`CONSOLE_DATA_FRAME_HEADER_LEN`]-byte header followed
//! immediately by the payload — see [`ConsoleDataFrame`] for the exact byte
//! layout and the documented resolution of an inconsistency in the frame
//! description this module implements.
//!
//! # Limits
//!
//! Every constant in this module is enforced by the instance regardless of
//! what Cloud sends or claims (ADR-045's "Invariants" section) — a
//! compromised or misbehaving Cloud cannot get an instance to exceed its own
//! bounds by simply asking.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Limits — instance-enforced regardless of what Cloud claims
// ---------------------------------------------------------------------------

/// Concurrent console-proxy streams a single link may have open. A 17th
/// `ConsoleStreamOpen` is refused with [`ConsoleRefusalReason::TooManyStreams`].
pub const CONSOLE_MAX_CONCURRENT_STREAMS: usize = 16;

/// Largest console-proxy frame — control or data — this instance will accept
/// or produce, bounding one allocation per frame. A binary frame whose total
/// wire size (header + payload) would exceed this is never sent, and one that
/// arrives over this size is rejected by [`ConsoleDataFrame::decode`] with
/// [`ConsoleFrameError::Oversize`].
pub const CONSOLE_MAX_FRAME_BYTES: usize = 64 * 1024;

/// Total header-byte budget for one [`ConsoleStreamOpen`]/[`ConsoleResponseHead`]
/// `headers` list — ordinary HTTP header-size sanity, independent of
/// [`CONSOLE_MAX_FRAME_BYTES`] (a header-heavy request should be rejected for
/// being header-heavy, not because it happened to also blow the generic frame
/// cap).
pub const CONSOLE_MAX_HEADER_BYTES: usize = 16 * 1024;

/// A stream with no activity in either direction for this long is closed by
/// the instance rather than left open indefinitely — the backstop against a
/// wedged flow-control credit exchange, independent of the credit logic
/// itself.
pub const CONSOLE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Initial, per-stream, per-direction flow-control credit. The sender must
/// never have more than this many un-acknowledged payload bytes in flight for
/// one stream; the receiver replenishes with [`ConsoleWindowUpdate`] as it
/// consumes bytes. Credit-based, not drop-based: unlike a dropped metrics
/// sample, dropping HTTP body bytes corrupts the response.
pub const CONSOLE_STREAM_WINDOW_BYTES: u32 = 256 * 1024;

// ---------------------------------------------------------------------------
// Control frames — JSON envelopes
// ---------------------------------------------------------------------------

/// Cloud → instance: open one HTTP request (optionally upgrading) as a new
/// stream. Refused outright, before any request object is built, when
/// `headers`' declared `Host` doesn't match this connection's pinned
/// `ConsoleOidcConfig.console_host`, or when no `ConsoleOidcConfig` has been
/// applied yet — see [`ConsoleRefusalReason::HostMismatch`] and
/// [`ConsoleRefusalReason::NotConfigured`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleStreamOpen {
    pub stream_id: Uuid,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Option<String>,
    /// Bounded by [`CONSOLE_MAX_HEADER_BYTES`] total. Carries `Host`, which
    /// the dispatcher validates against the connection's pinned hostname
    /// rather than trusting it outright (ADR-045 §3).
    pub headers: Vec<(String, String)>,
    /// The browser's real IP, as Cloud saw it — never the Hub's own address.
    /// Forwarded as `X-Forwarded-For` behind a synthetic loopback
    /// `ConnectInfo` so the instance's own client-IP trust gate resolves it
    /// correctly (ADR-045 §3). `None` when Cloud could not determine it.
    #[serde(default)]
    pub client_ip: Option<String>,
    pub upgrade_requested: bool,
}

impl ConsoleStreamOpen {
    pub const KIND: &'static str = "console_stream_open";
}

/// Instance → Cloud: the response headers for a stream, sent as soon as the
/// router produces them — before the body is known to be complete, so a
/// long-lived tail never waits behind its own eventual end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleResponseHead {
    pub stream_id: Uuid,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub upgraded: bool,
}

impl ConsoleResponseHead {
    pub const KIND: &'static str = "console_response_head";
}

/// Why a stream ended. Mirrors [`crate::Unavailable`]'s
/// `#[serde(tag = "reason")]` + `#[serde(other)]` shape so a reason
/// introduced by a newer peer never fails the whole frame to decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsoleStreamEndReason {
    /// The body finished normally; nothing more will arrive for this stream.
    Complete,
    /// The instance is shutting down. Sent on every currently open stream
    /// before the console-proxy socket closes (ADR-045 §1's graceful
    /// shutdown), so Cloud can offer the browser a retry instead of treating
    /// this as an unexplained hard cut.
    GoingAway,
    /// The stream failed for a reason worth surfacing to the operator.
    Error { detail: String },
    /// A reason introduced by a newer peer.
    #[serde(other)]
    Unknown,
}

/// Either side → the other: no more data will follow for this stream.
///
/// `reason` is flattened so its own `reason` tag key lands at the top level
/// (`{"stream_id":..,"reason":"complete"}`) instead of nesting a second
/// `"reason"` object inside a field already named `reason`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleStreamEnd {
    pub stream_id: Uuid,
    #[serde(flatten)]
    pub reason: ConsoleStreamEndReason,
}

impl ConsoleStreamEnd {
    pub const KIND: &'static str = "console_stream_end";
}

/// Cloud → instance: the browser tab went away or the request was aborted
/// before it finished; stop dispatching work for this stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleStreamCancel {
    pub stream_id: Uuid,
}

impl ConsoleStreamCancel {
    pub const KIND: &'static str = "console_stream_cancel";
}

/// Why the instance refused to dispatch a [`ConsoleStreamOpen`] at all — the
/// router is never called for any of these. Mirrors [`crate::Unavailable`]'s
/// shape for the same forward-compatibility reason as
/// [`ConsoleStreamEndReason`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsoleRefusalReason {
    /// The stream's declared `Host` header doesn't match the connection's
    /// pinned `ConsoleOidcConfig.console_host` (ADR-045 §3). Never trusted
    /// per-request; checked before a request object is even built.
    HostMismatch,
    /// A `ConsoleStreamOpen` arrived before `ConsoleOidcConfig` was applied on
    /// this connection — fail-closed, not fail-open: there is no pin to
    /// compare a `Host` against yet, so nothing is dispatched to the router.
    NotConfigured,
    /// This link already has [`CONSOLE_MAX_CONCURRENT_STREAMS`] streams open.
    TooManyStreams,
    /// `cloud.console_access_enabled` is off, or the console-proxy capability
    /// was never negotiated on this connection.
    Disabled,
    /// A tunneled WebSocket-upgrade request's `Origin` header did not equal
    /// `https://<console_host>` — rejected before the router sees it
    /// (ADR-045 Security Model).
    OriginMismatch,
    /// A `ConsoleStreamOpen` reused a `stream_id` that already has an open
    /// stream on this connection. Never overwrites the existing stream's
    /// table entry — that would desync the two sides' bookkeeping and
    /// silently lose whatever the original stream was doing — always
    /// refused outright instead.
    DuplicateStream,
    /// A reason introduced by a newer peer.
    #[serde(other)]
    Unknown,
}

/// Instance → Cloud: a [`ConsoleStreamOpen`] was rejected outright.
///
/// `reason` is flattened for the same reason as [`ConsoleStreamEnd::reason`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleStreamRefused {
    pub stream_id: Uuid,
    #[serde(flatten)]
    pub reason: ConsoleRefusalReason,
}

impl ConsoleStreamRefused {
    pub const KIND: &'static str = "console_stream_refused";
}

/// Either side → the other: replenish flow-control credit for one direction
/// of one stream by this many bytes. The sender of data frames must never
/// exceed the receiver's last-advertised credit
/// ([`CONSOLE_STREAM_WINDOW_BYTES`] initially).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsoleWindowUpdate {
    pub stream_id: Uuid,
    pub additional_bytes: u32,
}

impl ConsoleWindowUpdate {
    pub const KIND: &'static str = "console_window_update";
}

/// Cloud → instance, sent once right after the console-proxy `Hello`
/// completes, and again on every reconnect (ADR-045 §4). `CloudService`
/// upserts the managed `oidc_providers` row from this idempotently, so an
/// instance that missed a client-secret rotation while offline converges to
/// Cloud's current configuration the moment it reconnects.
#[derive(Clone, Serialize, Deserialize)]
pub struct ConsoleOidcConfig {
    pub issuer: String,
    pub client_id: String,
    /// Encrypted at rest immediately on receipt via `EncryptionService`, the
    /// same as every other provider's secret. Never logged — the [`Debug`]
    /// impl below redacts it exactly like [`crate::messages::EnrollRequest`]
    /// redacts `enrollment_code`.
    pub client_secret: String,
    pub jwks_uri: String,
    /// The hostname Cloud allocated for this instance's console. Pinned for
    /// the life of the connection: every subsequent [`ConsoleStreamOpen`]'s
    /// declared `Host` must match it exactly, or the stream is refused with
    /// [`ConsoleRefusalReason::HostMismatch`] (ADR-045 §3).
    pub console_host: String,
}

impl ConsoleOidcConfig {
    pub const KIND: &'static str = "console_oidc_config";
}

impl std::fmt::Debug for ConsoleOidcConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleOidcConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("jwks_uri", &self.jwks_uri)
            .field("console_host", &self.console_host)
            .finish()
    }
}

/// Cloud → instance: console access was disabled or the link was
/// disconnected from Cloud's side. Idempotent with the instance's own local
/// `cloud.console_access_enabled` disable path (ADR-045 §4/§5) — both run the
/// same delete-provider-then-invalidate-sessions sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleOidcRevoke;

impl ConsoleOidcRevoke {
    pub const KIND: &'static str = "console_oidc_revoke";
}

// ---------------------------------------------------------------------------
// Data frames — binary, no JSON, no base64
// ---------------------------------------------------------------------------

/// Fixed byte length of a [`ConsoleDataFrame`] header, before the payload.
///
/// # Layout (17 bytes total)
///
/// | Bytes | Field | Meaning |
/// |---|---|---|
/// | `0` | `version:kind` | High nibble: [`CONSOLE_DATA_FRAME_VERSION`]. Low nibble: [`ConsoleFrameKind`] discriminant. |
/// | `1..17` | `stream_id` | The same [`Uuid`] used in every control frame for this stream, big-endian (`Uuid::as_u128().to_be_bytes()`), so a receiver can route a data frame to its stream-table entry with no separate id translation. |
///
/// The payload is everything after byte 16 to the end of the `Message::Binary`
/// this header was read from; there is no separate length field.
///
/// # Documented resolution of an inconsistency in the frame description
///
/// The design this module implements describes the header as `frame_kind: u8`,
/// `stream_id: u128`, `sequence: u32` — three fields that sum to 21 bytes —
/// while also calling it "a fixed 17-byte header". Those two statements
/// cannot both be literally true. This module resolves the conflict by
/// keeping the two fields whose types are given explicitly and are load-
/// bearing (`stream_id` must stay a full [`Uuid`] to correlate with control
/// frames without a lookup table; a frame kind is required to tell a
/// request-body chunk from a response-body chunk from post-upgrade relay
/// bytes) and dropping the `sequence: u32` field, packing `version`+`kind`
/// into the one remaining byte as nibbles instead of two separate bytes.
/// `sequence` is redundant on this transport regardless: every data frame for
/// a given stream travels over the same single WebSocket connection, which
/// already delivers messages to the application in the order they were sent
/// — there is no reordering for a sequence number to detect. This yields
/// exactly 17 bytes (`1 + 16`), matching the stated header length precisely,
/// and needs no explicit length field because `Message::Binary`'s own frame
/// boundary already gives the payload length losslessly; re-declaring it
/// inside the header would be pure redundancy with nothing independent to
/// validate it against. Anyone implementing the Cloud side must follow this
/// exact 17-byte layout, not the inconsistent three-field description.
pub const CONSOLE_DATA_FRAME_HEADER_LEN: usize = 17;

/// High nibble of a [`ConsoleDataFrame`] header's first byte. Bumped only if
/// this specific binary framing changes incompatibly — independent of
/// [`crate::PROTOCOL_VERSION`], since data frames are the highest-volume
/// message on this connection and a framing change here should not force a
/// renegotiation of every other capability.
pub const CONSOLE_DATA_FRAME_VERSION: u8 = 1;

/// What a [`ConsoleDataFrame`]'s payload is a chunk of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConsoleFrameKind {
    /// A chunk of the HTTP request body, instance-bound.
    RequestBodyChunk = 0,
    /// A chunk of the HTTP response body, Cloud-bound.
    ResponseBodyChunk = 1,
    /// Raw bytes relayed in either direction after a WebSocket upgrade
    /// completes (ADR-045 §3's duplex/hyper-upgrade relay).
    WsRelay = 2,
}

impl ConsoleFrameKind {
    const fn from_nibble(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::RequestBodyChunk),
            1 => Some(Self::ResponseBodyChunk),
            2 => Some(Self::WsRelay),
            _ => None,
        }
    }
}

/// One binary console-proxy data frame: header plus payload, as described in
/// [`CONSOLE_DATA_FRAME_HEADER_LEN`]'s doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleDataFrame {
    pub frame_kind: ConsoleFrameKind,
    pub stream_id: Uuid,
    pub payload: bytes::Bytes,
}

impl ConsoleDataFrame {
    /// Build a frame, rejecting one whose encoded size would exceed
    /// [`CONSOLE_MAX_FRAME_BYTES`] rather than producing an oversize frame
    /// that every well-behaved decoder on the other end would reject anyway.
    pub fn new(
        frame_kind: ConsoleFrameKind,
        stream_id: Uuid,
        payload: bytes::Bytes,
    ) -> Result<Self, ConsoleFrameError> {
        let total_len = CONSOLE_DATA_FRAME_HEADER_LEN + payload.len();
        if total_len > CONSOLE_MAX_FRAME_BYTES {
            return Err(ConsoleFrameError::Oversize {
                len: total_len,
                max: CONSOLE_MAX_FRAME_BYTES,
            });
        }
        Ok(Self {
            frame_kind,
            stream_id,
            payload,
        })
    }

    /// Encode this frame as the exact bytes to send in one `Message::Binary`.
    pub fn encode(&self) -> bytes::Bytes {
        use bytes::BufMut;

        let mut buf =
            bytes::BytesMut::with_capacity(CONSOLE_DATA_FRAME_HEADER_LEN + self.payload.len());
        let header_byte = (CONSOLE_DATA_FRAME_VERSION << 4) | (self.frame_kind as u8);
        buf.put_u8(header_byte);
        buf.put_u128(self.stream_id.as_u128());
        buf.put_slice(&self.payload);
        buf.freeze()
    }

    /// Decode one complete `Message::Binary` payload into a frame.
    pub fn decode(bytes: &[u8]) -> Result<Self, ConsoleFrameError> {
        if bytes.len() > CONSOLE_MAX_FRAME_BYTES {
            return Err(ConsoleFrameError::Oversize {
                len: bytes.len(),
                max: CONSOLE_MAX_FRAME_BYTES,
            });
        }
        if bytes.len() < CONSOLE_DATA_FRAME_HEADER_LEN {
            return Err(ConsoleFrameError::Truncated {
                received: bytes.len(),
                needed: CONSOLE_DATA_FRAME_HEADER_LEN,
            });
        }

        let header_byte = bytes[0];
        let version = header_byte >> 4;
        if version != CONSOLE_DATA_FRAME_VERSION {
            return Err(ConsoleFrameError::UnsupportedVersion { version });
        }

        let kind_nibble = header_byte & 0x0F;
        let frame_kind = ConsoleFrameKind::from_nibble(kind_nibble)
            .ok_or(ConsoleFrameError::UnknownFrameKind { kind: kind_nibble })?;

        let mut stream_id_bytes = [0u8; 16];
        stream_id_bytes.copy_from_slice(&bytes[1..CONSOLE_DATA_FRAME_HEADER_LEN]);
        let stream_id = Uuid::from_u128(u128::from_be_bytes(stream_id_bytes));

        let payload = bytes::Bytes::copy_from_slice(&bytes[CONSOLE_DATA_FRAME_HEADER_LEN..]);

        Ok(Self {
            frame_kind,
            stream_id,
            payload,
        })
    }
}

/// Every way [`ConsoleDataFrame::decode`] can reject a byte slice.
///
/// Deliberately has no `LengthMismatch` variant: this header carries no
/// independent length field (see [`CONSOLE_DATA_FRAME_HEADER_LEN`]'s doc
/// comment), so there is nothing for a declared length to disagree with —
/// [`ConsoleFrameError::Truncated`] and [`ConsoleFrameError::Oversize`]
/// already cover every length-related failure this framing can exhibit.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConsoleFrameError {
    #[error("console data frame truncated: received {received} bytes, need at least {needed}")]
    Truncated { received: usize, needed: usize },
    #[error("console data frame has unsupported version {version}, expected {expected}", expected = CONSOLE_DATA_FRAME_VERSION)]
    UnsupportedVersion { version: u8 },
    #[error("console data frame has unknown frame kind {kind}")]
    UnknownFrameKind { kind: u8 },
    #[error("console data frame of {len} bytes exceeds the {max}-byte limit")]
    Oversize { len: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Envelope;

    fn uuid() -> Uuid {
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("valid uuid literal")
    }

    // -- Control frames: envelope kind strings are a stable wire contract --

    #[test]
    fn envelope_kind_strings_are_stable() {
        assert_eq!(ConsoleStreamOpen::KIND, "console_stream_open");
        assert_eq!(ConsoleResponseHead::KIND, "console_response_head");
        assert_eq!(ConsoleStreamEnd::KIND, "console_stream_end");
        assert_eq!(ConsoleStreamCancel::KIND, "console_stream_cancel");
        assert_eq!(ConsoleStreamRefused::KIND, "console_stream_refused");
        assert_eq!(ConsoleWindowUpdate::KIND, "console_window_update");
        assert_eq!(ConsoleOidcConfig::KIND, "console_oidc_config");
        assert_eq!(ConsoleOidcRevoke::KIND, "console_oidc_revoke");
    }

    #[test]
    fn console_stream_open_round_trips_through_an_envelope() {
        let open = ConsoleStreamOpen {
            stream_id: uuid(),
            method: "GET".into(),
            path: "/api/projects".into(),
            query: Some("page=2".into()),
            headers: vec![
                ("host".into(), "console.example.invalid".into()),
                ("cookie".into(), "session=abc".into()),
            ],
            client_ip: Some("203.0.113.7".into()),
            upgrade_requested: false,
        };
        let envelope = Envelope::new(ConsoleStreamOpen::KIND, &open).expect("must encode");
        assert_eq!(envelope.kind, "console_stream_open");
        let decoded: ConsoleStreamOpen = envelope
            .decode(ConsoleStreamOpen::KIND)
            .expect("must decode");
        assert_eq!(decoded.stream_id, open.stream_id);
        assert_eq!(decoded.method, "GET");
        assert_eq!(decoded.query.as_deref(), Some("page=2"));
        assert_eq!(decoded.headers.len(), 2);
        assert_eq!(decoded.client_ip.as_deref(), Some("203.0.113.7"));
        assert!(!decoded.upgrade_requested);
    }

    #[test]
    fn console_stream_open_tolerates_missing_optional_fields() {
        let envelope: Envelope = serde_json::from_value(serde_json::json!({
            "kind": "console_stream_open",
            "payload": {
                "stream_id": uuid(),
                "method": "GET",
                "path": "/",
                "headers": [],
                "upgrade_requested": false
            }
        }))
        .expect("must decode envelope");
        let open: ConsoleStreamOpen = envelope
            .decode(ConsoleStreamOpen::KIND)
            .expect("must decode payload");
        assert!(open.query.is_none());
        assert!(open.client_ip.is_none());
    }

    #[test]
    fn console_response_head_round_trips() {
        let head = ConsoleResponseHead {
            stream_id: uuid(),
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            upgraded: false,
        };
        let envelope = Envelope::new(ConsoleResponseHead::KIND, &head).expect("must encode");
        let decoded: ConsoleResponseHead = envelope
            .decode(ConsoleResponseHead::KIND)
            .expect("must decode");
        assert_eq!(decoded.status, 200);
        assert!(!decoded.upgraded);
    }

    #[test]
    fn console_stream_end_reasons_round_trip() {
        for reason in [
            ConsoleStreamEndReason::Complete,
            ConsoleStreamEndReason::GoingAway,
            ConsoleStreamEndReason::Error {
                detail: "upstream reset".into(),
            },
        ] {
            let end = ConsoleStreamEnd {
                stream_id: uuid(),
                reason: reason.clone(),
            };
            let envelope = Envelope::new(ConsoleStreamEnd::KIND, &end).expect("must encode");
            let decoded: ConsoleStreamEnd = envelope
                .decode(ConsoleStreamEnd::KIND)
                .expect("must decode");
            assert_eq!(decoded.reason, reason);
        }
    }

    #[test]
    fn console_stream_end_unknown_reason_is_tolerated() {
        let envelope: Envelope = serde_json::from_value(serde_json::json!({
            "kind": "console_stream_end",
            "payload": { "stream_id": uuid(), "reason": "future_reason" }
        }))
        .expect("must decode envelope");
        let end: ConsoleStreamEnd = envelope
            .decode(ConsoleStreamEnd::KIND)
            .expect("must decode payload");
        assert_eq!(end.reason, ConsoleStreamEndReason::Unknown);
    }

    #[test]
    fn console_stream_cancel_round_trips() {
        let cancel = ConsoleStreamCancel { stream_id: uuid() };
        let envelope = Envelope::new(ConsoleStreamCancel::KIND, &cancel).expect("must encode");
        let decoded: ConsoleStreamCancel = envelope
            .decode(ConsoleStreamCancel::KIND)
            .expect("must decode");
        assert_eq!(decoded.stream_id, cancel.stream_id);
    }

    #[test]
    fn console_stream_refused_reasons_round_trip() {
        for reason in [
            ConsoleRefusalReason::HostMismatch,
            ConsoleRefusalReason::NotConfigured,
            ConsoleRefusalReason::TooManyStreams,
            ConsoleRefusalReason::Disabled,
            ConsoleRefusalReason::OriginMismatch,
            ConsoleRefusalReason::DuplicateStream,
        ] {
            let refused = ConsoleStreamRefused {
                stream_id: uuid(),
                reason: reason.clone(),
            };
            let envelope =
                Envelope::new(ConsoleStreamRefused::KIND, &refused).expect("must encode");
            let decoded: ConsoleStreamRefused = envelope
                .decode(ConsoleStreamRefused::KIND)
                .expect("must decode");
            assert_eq!(decoded.reason, reason);
        }
    }

    #[test]
    fn console_stream_refused_unknown_reason_is_tolerated() {
        let envelope: Envelope = serde_json::from_value(serde_json::json!({
            "kind": "console_stream_refused",
            "payload": { "stream_id": uuid(), "reason": "future_refusal" }
        }))
        .expect("must decode envelope");
        let refused: ConsoleStreamRefused = envelope
            .decode(ConsoleStreamRefused::KIND)
            .expect("must decode payload");
        assert_eq!(refused.reason, ConsoleRefusalReason::Unknown);
    }

    #[test]
    fn console_window_update_round_trips() {
        let update = ConsoleWindowUpdate {
            stream_id: uuid(),
            additional_bytes: 65536,
        };
        let envelope = Envelope::new(ConsoleWindowUpdate::KIND, &update).expect("must encode");
        let decoded: ConsoleWindowUpdate = envelope
            .decode(ConsoleWindowUpdate::KIND)
            .expect("must decode");
        assert_eq!(decoded.additional_bytes, 65536);
    }

    #[test]
    fn console_oidc_config_round_trips() {
        let config = ConsoleOidcConfig {
            issuer: "https://issuer.example.invalid".into(),
            client_id: "instance-client".into(),
            client_secret: "super-secret-value".into(),
            jwks_uri: "https://issuer.example.invalid/jwks.json".into(),
            console_host: "console-abc123.example.invalid".into(),
        };
        let envelope = Envelope::new(ConsoleOidcConfig::KIND, &config).expect("must encode");
        let decoded: ConsoleOidcConfig = envelope
            .decode(ConsoleOidcConfig::KIND)
            .expect("must decode");
        assert_eq!(decoded.client_secret, "super-secret-value");
        assert_eq!(decoded.console_host, "console-abc123.example.invalid");
    }

    /// Doc example: how a caller is expected to build and read this frame.
    /// Mirrors the pattern used for every other envelope on this channel.
    #[test]
    fn console_oidc_config_doc_example() {
        let config = ConsoleOidcConfig {
            issuer: "https://issuer.example.invalid".into(),
            client_id: "instance-client".into(),
            client_secret: "hunter2".into(),
            jwks_uri: "https://issuer.example.invalid/jwks.json".into(),
            console_host: "console-abc123.example.invalid".into(),
        };
        let envelope = Envelope::new(ConsoleOidcConfig::KIND, &config).expect("must encode");
        let json = serde_json::to_string(&envelope).expect("must serialize");
        assert!(json.contains("\"kind\":\"console_oidc_config\""));

        let decoded_envelope: Envelope = serde_json::from_str(&json).expect("must parse");
        let decoded: ConsoleOidcConfig = decoded_envelope
            .decode(ConsoleOidcConfig::KIND)
            .expect("must decode");
        assert_eq!(decoded.issuer, config.issuer);
    }

    #[test]
    fn console_oidc_config_debug_output_redacts_the_client_secret() {
        let config = ConsoleOidcConfig {
            issuer: "https://issuer.example.invalid".into(),
            client_id: "instance-client".into(),
            client_secret: "super-secret-value".into(),
            jwks_uri: "https://issuer.example.invalid/jwks.json".into(),
            console_host: "console-abc123.example.invalid".into(),
        };
        let debug_output = format!("{config:?}");
        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("client_secret: \"[REDACTED]\""));
        // Non-secret fields remain visible — the point of redaction is the
        // secret, not making the whole struct opaque.
        assert!(debug_output.contains("instance-client"));
        assert!(debug_output.contains("console-abc123.example.invalid"));
    }

    #[test]
    fn console_oidc_revoke_round_trips_with_an_empty_payload() {
        let envelope =
            Envelope::new(ConsoleOidcRevoke::KIND, &ConsoleOidcRevoke).expect("must encode");
        let _decoded: ConsoleOidcRevoke = envelope
            .decode(ConsoleOidcRevoke::KIND)
            .expect("must decode");
    }

    // -- Unknown envelope kinds are still tolerated on this channel --

    #[test]
    fn an_unrecognized_console_envelope_kind_is_dropped_not_fatal() {
        let envelope = Envelope::new(
            "console_future_frame",
            &serde_json::json!({"anything": "goes"}),
        )
        .expect("must encode");
        let decoded: Option<ConsoleStreamOpen> = envelope.decode(ConsoleStreamOpen::KIND);
        assert!(decoded.is_none());
    }

    // -- Binary data frames --

    #[test]
    fn console_data_frame_encode_decode_round_trips_for_every_kind() {
        for kind in [
            ConsoleFrameKind::RequestBodyChunk,
            ConsoleFrameKind::ResponseBodyChunk,
            ConsoleFrameKind::WsRelay,
        ] {
            let frame = ConsoleDataFrame::new(kind, uuid(), bytes::Bytes::from_static(b"hello"))
                .expect("must build");
            let encoded = frame.encode();
            assert_eq!(encoded.len(), CONSOLE_DATA_FRAME_HEADER_LEN + 5);
            let decoded = ConsoleDataFrame::decode(&encoded).expect("must decode");
            assert_eq!(decoded, frame);
        }
    }

    #[test]
    fn console_data_frame_round_trips_an_empty_payload() {
        let frame = ConsoleDataFrame::new(
            ConsoleFrameKind::ResponseBodyChunk,
            uuid(),
            bytes::Bytes::new(),
        )
        .expect("must build");
        let encoded = frame.encode();
        assert_eq!(encoded.len(), CONSOLE_DATA_FRAME_HEADER_LEN);
        let decoded = ConsoleDataFrame::decode(&encoded).expect("must decode");
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn console_data_frame_header_byte_layout_is_exact() {
        let frame = ConsoleDataFrame::new(
            ConsoleFrameKind::WsRelay,
            uuid(),
            bytes::Bytes::from_static(b"xy"),
        )
        .expect("must build");
        let encoded = frame.encode();
        assert_eq!(encoded[0], (CONSOLE_DATA_FRAME_VERSION << 4) | 2);
        assert_eq!(
            u128::from_be_bytes(encoded[1..17].try_into().expect("16 bytes")),
            uuid().as_u128()
        );
        assert_eq!(&encoded[17..], b"xy");
    }

    #[test]
    fn console_data_frame_new_rejects_oversize_payload() {
        let too_big = vec![0u8; CONSOLE_MAX_FRAME_BYTES];
        let err = ConsoleDataFrame::new(
            ConsoleFrameKind::RequestBodyChunk,
            uuid(),
            bytes::Bytes::from(too_big),
        )
        .expect_err("must reject");
        assert!(matches!(err, ConsoleFrameError::Oversize { .. }));
    }

    #[test]
    fn console_data_frame_decode_rejects_oversize_input() {
        let too_big = vec![0u8; CONSOLE_MAX_FRAME_BYTES + 1];
        let err = ConsoleDataFrame::decode(&too_big).expect_err("must reject");
        assert_eq!(
            err,
            ConsoleFrameError::Oversize {
                len: CONSOLE_MAX_FRAME_BYTES + 1,
                max: CONSOLE_MAX_FRAME_BYTES,
            }
        );
    }

    #[test]
    fn console_data_frame_decode_rejects_truncated_input() {
        let too_short = vec![0u8; CONSOLE_DATA_FRAME_HEADER_LEN - 1];
        let err = ConsoleDataFrame::decode(&too_short).expect_err("must reject");
        assert_eq!(
            err,
            ConsoleFrameError::Truncated {
                received: CONSOLE_DATA_FRAME_HEADER_LEN - 1,
                needed: CONSOLE_DATA_FRAME_HEADER_LEN,
            }
        );
    }

    #[test]
    fn console_data_frame_decode_rejects_empty_input() {
        let err = ConsoleDataFrame::decode(&[]).expect_err("must reject");
        assert_eq!(
            err,
            ConsoleFrameError::Truncated {
                received: 0,
                needed: CONSOLE_DATA_FRAME_HEADER_LEN,
            }
        );
    }

    #[test]
    fn console_data_frame_decode_rejects_unsupported_version() {
        let mut bytes = vec![0u8; CONSOLE_DATA_FRAME_HEADER_LEN];
        bytes[0] = 0xF0; // version nibble 15, kind nibble 0
        let err = ConsoleDataFrame::decode(&bytes).expect_err("must reject");
        assert_eq!(err, ConsoleFrameError::UnsupportedVersion { version: 0x0F });
    }

    #[test]
    fn console_data_frame_decode_rejects_unknown_frame_kind() {
        let mut bytes = vec![0u8; CONSOLE_DATA_FRAME_HEADER_LEN];
        bytes[0] = (CONSOLE_DATA_FRAME_VERSION << 4) | 0x0F; // kind nibble 15 is unassigned
        let err = ConsoleDataFrame::decode(&bytes).expect_err("must reject");
        assert_eq!(err, ConsoleFrameError::UnknownFrameKind { kind: 0x0F });
    }

    #[test]
    fn console_data_frame_preserves_the_correct_stream_id_even_on_unknown_kind() {
        // A receiver logging a rejected frame benefits from knowing which
        // stream it was for, but this decoder deliberately keeps the error
        // itself minimal (see `ConsoleFrameError`'s doc comment) — this test
        // just pins that byte 1..17 is unambiguously the stream id regardless
        // of whether the kind nibble was recognized.
        let mut bytes = vec![0u8; CONSOLE_DATA_FRAME_HEADER_LEN];
        bytes[0] = (CONSOLE_DATA_FRAME_VERSION << 4) | 0x0F;
        bytes[1..17].copy_from_slice(&uuid().as_u128().to_be_bytes());
        let err = ConsoleDataFrame::decode(&bytes).expect_err("must reject");
        assert_eq!(err, ConsoleFrameError::UnknownFrameKind { kind: 0x0F });
    }
}
