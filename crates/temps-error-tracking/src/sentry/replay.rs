// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Browser rrweb replay adapter. Baseline: @sentry/browser 11.8.0 and
//! @sentry/replay 11.8.0, c6492ef78c7bfb97931e43075763ce84ee8fda7e,
//! replay/src/util/{createReplayEnvelope,prepareRecordingData,sendReplayRequest}.
//! Mobile/video recordings have a different format and remain unsupported.

use super::{envelope::EnvelopeError, logs, traces};
use crate::providers::AuthContext;
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read};
use temps_analytics_session_replay::services::service::{Screen, SessionMetadata, Viewport};

pub struct ReplaySegment {
    pub session_id: String,
    pub segment_id: u32,
    pub metadata: SessionMetadata,
    pub events: Vec<Value>,
}

#[derive(Deserialize)]
struct ReplayEvent<'a> {
    replay_id: String,
    segment_id: u32,
    platform: Option<String>,
    #[serde(borrow)]
    replay_start_timestamp: &'a RawValue,
    #[serde(borrow)]
    timestamp: &'a RawValue,
    #[serde(default)]
    urls: Vec<String>,
    request: Option<Value>,
    user: Option<Value>,
    release: Option<String>,
    environment: Option<String>,
    #[serde(default)]
    contexts: BTreeMap<String, Value>,
    #[serde(default)]
    trace_ids: Vec<String>,
    #[serde(default)]
    error_ids: Vec<String>,
}
#[derive(Deserialize)]
struct RecordingHeader {
    segment_id: u32,
}

fn invalid() -> EnvelopeError {
    EnvelopeError::InvalidPayload("invalid or unsupported browser replay segment".into())
}

pub fn decode(
    event: &[u8],
    recording: &[u8],
    auth: &AuthContext,
) -> Result<ReplaySegment, EnvelopeError> {
    let event: ReplayEvent<'_> = serde_json::from_slice(event).map_err(|_| invalid())?;
    if !logs::valid_id(&event.replay_id, 32)
        || event.segment_id > 9999
        || event.platform.as_deref().is_some_and(|p| p != "javascript")
        || event.urls.len() > 100
        || event.trace_ids.len() > 1000
        || event.error_ids.len() > 1000
        || event.contexts.len() > 128
    {
        return Err(invalid());
    }
    let start = traces::time(event.replay_start_timestamp)?;
    let end = traces::time(event.timestamp)?;
    if end < start || end - start > chrono::Duration::hours(24) {
        return Err(invalid());
    }
    let header_end = recording
        .iter()
        .take(8193)
        .position(|b| *b == b'\n')
        .ok_or_else(invalid)?;
    if header_end > 8192 {
        return Err(invalid());
    }
    let header: RecordingHeader =
        serde_json::from_slice(&recording[..header_end]).map_err(|_| invalid())?;
    if header.segment_id != event.segment_id {
        return Err(invalid());
    }
    let data = &recording[header_end + 1..];
    // The SDK emits either a JSON array or zlib-compressed JSON bytes after
    // the recording header. Bounded decompression also covers binary framing.
    let mut decompressed = Vec::new();
    let data = if data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        data
    } else {
        if data.starts_with(&[0x1f, 0x8b]) {
            flate2::read::GzDecoder::new(data)
                .take(10 * 1024 * 1024 + 1)
                .read_to_end(&mut decompressed)
                .map_err(|_| invalid())?;
        } else {
            flate2::read::ZlibDecoder::new(data)
                .take(10 * 1024 * 1024 + 1)
                .read_to_end(&mut decompressed)
                .map_err(|_| invalid())?;
        }
        decompressed.as_slice()
    };
    if data.len() > 10 * 1024 * 1024 {
        return Err(invalid());
    }
    let events: Vec<&RawValue> = {
        #[derive(Deserialize)]
        struct Events<'a>(#[serde(borrow, deserialize_with = "bounded_events")] Vec<&'a RawValue>);
        serde_json::from_slice::<Events<'_>>(data)
            .map_err(|_| invalid())?
            .0
    };
    let mut decoded = Vec::with_capacity(events.len() + 1);
    let mut viewport = Viewport {
        width: 0,
        height: 0,
    };
    let mut url = event.urls.first().cloned().unwrap_or_default();
    for raw in events {
        let mut value: Value = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        let source_timestamp = value
            .get("timestamp")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite())
            .ok_or_else(invalid)?;
        let ty = value
            .get("type")
            .and_then(Value::as_u64)
            .ok_or_else(invalid)?;
        // The JS SDK's custom breadcrumb/performance events can carry epoch
        // seconds, while rrweb snapshots use milliseconds. Match its own
        // timestampToMs rule, and retain the source timestamp when adapting.
        let timestamp = if ty == 5 && source_timestamp <= 9_999_999_999.0 {
            source_timestamp * 1000.0
        } else {
            source_timestamp
        };
        if !value.is_object()
            || ty > 6
            || value.get("_temps_source_order").is_some()
            || value.get("_temps_sentry_timestamp").is_some()
            || !(0.0..=253402300799000.0).contains(&timestamp)
        {
            return Err(invalid());
        }
        if timestamp != source_timestamp {
            value["_temps_sentry_timestamp"] = value["timestamp"].clone();
            value["timestamp"] = serde_json::json!(timestamp);
        }
        if ty == 4 {
            if let Some(meta) = value.get("data") {
                viewport.width = meta
                    .get("width")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(65535) as u32;
                viewport.height = meta
                    .get("height")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(65535) as u32;
                if let Some(href) = meta.get("href").and_then(Value::as_str) {
                    url = href.into();
                }
            }
        }
        decoded.push(value);
    }
    if url.len() > 8192 || url.contains('\0') {
        return Err(invalid());
    }
    // No analytics SDK identity is required. A manually supplied Sentry user
    // ID is pseudonymized within the DSN scope; anonymous identity is per replay.
    let scope = format!(
        "{}:{:?}:{:?}",
        auth.project_id, auth.environment_id, auth.deployment_id
    );
    let identity = event
        .user
        .as_ref()
        .and_then(|u| u.get("id"))
        .map(Value::to_string)
        .unwrap_or_else(|| event.replay_id.clone());
    let visitor_id = format!(
        "sentry-{}",
        hex::encode(Sha256::digest(format!("{scope}:{identity}").as_bytes()))
    );
    let user_agent = event
        .request
        .as_ref()
        .and_then(|r| r.get("headers"))
        .and_then(Value::as_object)
        .and_then(|h| h.iter().find(|(k, _)| k.eq_ignore_ascii_case("user-agent")))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("")
        .to_owned();
    if user_agent.len() > 4096 || user_agent.contains('\0') {
        return Err(invalid());
    }
    // A native rrweb custom event retains replay correlation and resource
    // metadata without a second replay store or a Sentry-only query API.
    decoded.push(serde_json::json!({"type":5,"timestamp":end.timestamp_millis(),"data":{"tag":"sentry.replay","payload":{"replay_id":event.replay_id,"segment_id":event.segment_id,"release":event.release,"environment":event.environment,"trace_ids":event.trace_ids,"error_ids":event.error_ids,"urls":event.urls,"user":event.user,"contexts":event.contexts}}}));
    Ok(ReplaySegment {
        session_id: format!(
            "sentry-{}-{}-{}-{}",
            auth.project_id,
            auth.environment_id.unwrap_or(0),
            auth.deployment_id.unwrap_or(0),
            event.replay_id.to_ascii_lowercase()
        ),
        segment_id: event.segment_id,
        metadata: SessionMetadata {
            visitor_id,
            user_agent,
            language: String::new(),
            timezone: String::new(),
            screen: Screen {
                width: 0,
                height: 0,
                color_depth: 0,
            },
            viewport,
            timestamp: start.to_rfc3339(),
            url,
        },
        events: decoded,
    })
}

fn bounded_events<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<&'de RawValue>, D::Error> {
    struct Events;
    impl<'de> serde::de::Visitor<'de> for Events {
        type Value = Vec<&'de RawValue>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 19999 replay events")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut events = Vec::new();
            while let Some(event) = seq.next_element()? {
                if events.len() >= 19999 {
                    return Err(serde::de::Error::custom("replay event limit exceeded"));
                }
                events.push(event);
            }
            Ok(events)
        }
    }
    d.deserialize_seq(Events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn auth() -> AuthContext {
        AuthContext {
            project_id: 41,
            environment_id: Some(42),
            deployment_id: Some(43),
            rate_limit_per_minute: None,
        }
    }
    fn event() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"platform":"javascript","replay_id":"11111111111111111111111111111111","segment_id":3,"replay_start_timestamp":1790000000,"timestamp":1790000001,"release":"synthetic-release","urls":["https://synthetic.example/"],"trace_ids":["22222222222222222222222222222222"]})).unwrap()
    }
    #[test]
    fn compressed_and_multiline_plain_recordings_map_to_native_rrweb_and_metadata() {
        let json=br#"[{"type":4,"timestamp":1790000000000,"data":{"width":800,"height":600,"href":"https://synthetic.example/"}},
{"type":5,"timestamp":1790000001000,"data":{"tag":"synthetic"}}]"#;
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressor.write_all(json).unwrap();
        let compressed = compressor.finish().unwrap();
        for data in [json.as_slice(), compressed.as_slice()] {
            let mut recording = b"{\"segment_id\":3}\n".to_vec();
            recording.extend_from_slice(data);
            let segment = decode(&event(), &recording, &auth()).unwrap();
            assert_eq!(segment.events.len(), 3);
            assert_eq!(segment.metadata.viewport.width, 800);
            assert!(segment.session_id.starts_with("sentry-41-42-43-"));
            assert!(segment.metadata.visitor_id.starts_with("sentry-"));
            assert_eq!(
                segment.events[2].pointer("/data/payload/release"),
                Some(&serde_json::json!("synthetic-release"))
            );
        }
    }
    #[test]
    fn rejects_mismatched_sequences_truncated_binary_and_decompression_bombs() {
        assert!(decode(&event(), b"{\"segment_id\":2}\n[]", &auth()).is_err());
        assert!(decode(&event(), b"{\"segment_id\":3}\n\x78\x9c", &auth()).is_err());
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        compressor
            .write_all(&vec![b' '; 10 * 1024 * 1024 + 1])
            .unwrap();
        let mut data = b"{\"segment_id\":3}\n".to_vec();
        data.extend(compressor.finish().unwrap());
        assert!(decode(&event(), &data, &auth()).is_err());
    }
}
