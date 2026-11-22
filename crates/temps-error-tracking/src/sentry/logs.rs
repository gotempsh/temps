// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adapter for the official batched Sentry log protocol. No OTLP encoding or
//! secondary storage is involved. Native type metadata accompanies the exact
//! string values for typed queries; `temps.sentry.attributes` retains the
//! original typed wire representation for compatibility with older readers.
//!
//! Contract baseline: sentry-javascript 10.17.0, source revision
//! eb6e2946eb45583dd4c02ec92f8912e5bad51943, packages/core/src/logs/envelope.ts
//! and types-hoist/log.ts; also 11.8.0, revision
//! c6492ef78c7bfb97931e43075763ce84ee8fda7e. Version-2 batches are accepted
//! with opt-in HTTP inference shared by logs, spans, and metrics.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{de, Deserialize, Deserializer};
use serde_json::value::RawValue;
use temps_otel::types::{LogAttributeType, LogRecord, LogSeverity, ResourceInfo};

use super::envelope::{EnvelopeError, ItemHeader};
use crate::providers::AuthContext;

pub const MAX_LOGS: usize = 1000;
const MAX_ATTRIBUTES: usize = 128;
const RESERVED_PREFIX: &str = "temps.sentry.";

#[derive(Deserialize)]
pub(super) struct Batch<'a> {
    #[serde(borrow, deserialize_with = "bounded_logs")]
    pub(super) items: Vec<&'a RawValue>,
    pub(super) version: Option<u32>,
    pub(super) ingest_settings: Option<IngestSettings>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct IngestSettings {
    pub(super) infer_ip: Option<String>,
    pub(super) infer_user_agent: Option<String>,
}

pub(super) fn bounded_logs<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<&'de RawValue>, D::Error> {
    struct Logs;
    impl<'de> de::Visitor<'de> for Logs {
        type Value = Vec<&'de RawValue>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 1000 log records")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element()? {
                if items.len() == MAX_LOGS {
                    return Err(de::Error::custom("log count limit exceeded"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    d.deserialize_seq(Logs)
}

pub(super) fn bounded_attributes<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, &'de RawValue>, D::Error> {
    struct Attributes;
    impl<'de> de::Visitor<'de> for Attributes {
        type Value = BTreeMap<String, &'de RawValue>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 128 unique attributes")
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if values.len() == MAX_ATTRIBUTES
                    || values.contains_key(&key)
                    || key.len() > 128
                    || key.contains('\0')
                {
                    return Err(de::Error::custom("attribute key/count limit or duplicate"));
                }
                let value: &RawValue = map.next_value()?;
                if value.get().len() > 16 * 1024 {
                    return Err(de::Error::custom("attribute value limit exceeded"));
                }
                values.insert(key, value);
            }
            Ok(values)
        }
    }
    d.deserialize_map(Attributes)
}

#[derive(Deserialize)]
struct WireLog<'a> {
    #[serde(borrow)]
    timestamp: &'a RawValue,
    trace_id: String,
    span_id: Option<String>,
    level: String,
    body: String,
    severity_number: Option<u8>,
    #[serde(default, borrow, deserialize_with = "bounded_attributes")]
    attributes: BTreeMap<String, &'a RawValue>,
}

#[derive(Deserialize)]
struct Attribute<'a> {
    #[serde(rename = "type")]
    ty: String,
    #[serde(borrow)]
    value: &'a RawValue,
}

fn invalid(reason: &str) -> EnvelopeError {
    EnvelopeError::InvalidPayload(format!("log batch: {reason}"))
}

fn json_error(e: serde_json::Error) -> EnvelopeError {
    // serde's Display can contain a submitted value. Log locations only.
    invalid(&format!(
        "invalid JSON/schema at line {} column {}",
        e.line(),
        e.column()
    ))
}

/// Decimal seconds to nanoseconds without a floating-point round trip. Source
/// text is also retained: sub-nanosecond precision is unavailable in DateTime,
/// and native database timestamps may be microseconds.
pub(super) fn timestamp(raw: &str) -> Option<DateTime<Utc>> {
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (raw, 0),
    };
    let fraction = mantissa.split_once('.').map_or(0, |(_, f)| f.len());
    let digits = mantissa.replace('.', "");
    let value = digits.parse::<i128>().ok()?;
    let scale = 9_i32
        .checked_add(exponent)?
        .checked_sub(i32::try_from(fraction).ok()?)?;
    let nanos = if scale >= 0 {
        value.checked_mul(10_i128.checked_pow(scale.try_into().ok()?)?)?
    } else {
        value.checked_div(10_i128.checked_pow(scale.unsigned_abs())?)?
    };
    DateTime::from_timestamp(
        nanos.div_euclid(1_000_000_000).try_into().ok()?,
        nanos.rem_euclid(1_000_000_000).try_into().ok()?,
    )
}

pub(super) fn valid_id(id: &str, length: usize) -> bool {
    id.len() == length && id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0')
}

pub(super) fn decode_attributes(
    values: &BTreeMap<String, &RawValue>,
) -> Result<BTreeMap<String, String>, EnvelopeError> {
    decode_attributes_with_arrays(values, false)
}

pub(super) fn decode_attributes_with_arrays(
    values: &BTreeMap<String, &RawValue>,
    allow_arrays: bool,
) -> Result<BTreeMap<String, String>, EnvelopeError> {
    let mut attributes = BTreeMap::new();
    for (key, raw) in values {
        if key.starts_with(RESERVED_PREFIX) {
            return Err(invalid("reserved attribute namespace"));
        }
        let attribute: Attribute<'_> = serde_json::from_str(raw.get()).map_err(json_error)?;
        let value = match attribute.ty.as_str() {
            "string" => {
                serde_json::from_str::<String>(attribute.value.get()).map_err(json_error)?
            }
            "boolean" => serde_json::from_str::<bool>(attribute.value.get())
                .map_err(json_error)?
                .to_string(),
            "integer" => {
                serde_json::from_str::<i64>(attribute.value.get()).map_err(json_error)?;
                attribute.value.get().to_owned()
            }
            "double" => {
                let value: f64 = serde_json::from_str(attribute.value.get()).map_err(json_error)?;
                if !value.is_finite() {
                    return Err(invalid("non-finite attribute"));
                }
                attribute.value.get().to_owned()
            }
            "array" if allow_arrays => {
                let values: Vec<serde_json::Value> =
                    serde_json::from_str(attribute.value.get()).map_err(json_error)?;
                if values.len() > 128
                    || values
                        .iter()
                        .any(|v| !v.is_string() && !v.is_boolean() && !v.is_number())
                {
                    return Err(invalid("unsupported array attribute"));
                }
                attribute.value.get().to_owned()
            }
            _ => return Err(invalid("unsupported attribute type")),
        };
        if value.contains('\0') {
            return Err(invalid("NUL characters cannot be stored in native logs"));
        }
        attributes.insert(key.clone(), value);
    }
    Ok(attributes)
}

pub fn decode_logs(
    header: &ItemHeader,
    payload: &[u8],
    auth: &AuthContext,
) -> Result<Vec<LogRecord>, EnvelopeError> {
    if header.content_type.as_deref() != Some("application/vnd.sentry.items.log+json") {
        return Err(invalid("unsupported content_type"));
    }
    if header.item_count.is_none_or(|count| count > MAX_LOGS) {
        return Err(invalid("missing or excessive item_count"));
    }
    let batch: Batch<'_> = serde_json::from_slice(payload).map_err(json_error)?;
    if batch
        .version
        .is_some_and(|version| version != 1 && version != 2)
    {
        return Err(invalid("unsupported log protocol version"));
    }
    super::inference::validate(batch.ingest_settings.as_ref())?;
    if header.item_count != Some(batch.items.len()) {
        return Err(invalid("item_count does not match items"));
    }
    let observed_timestamp = Utc::now();
    let mut records = Vec::with_capacity(batch.items.len());
    for item in batch.items {
        let log: WireLog<'_> = serde_json::from_str(item.get()).map_err(json_error)?;
        if log.body.len() > 64 * 1024
            || log.body.contains('\0')
            || !valid_id(&log.trace_id, 32)
            || log.span_id.as_deref().is_some_and(|id| !valid_id(id, 16))
        {
            return Err(invalid("body limit/NUL violation or invalid trace/span ID"));
        }
        let (severity, base) = match log.level.as_str() {
            "trace" => (LogSeverity::Trace, 1),
            "debug" => (LogSeverity::Debug, 5),
            "info" => (LogSeverity::Info, 9),
            "warn" => (LogSeverity::Warn, 13),
            "error" => (LogSeverity::Error, 17),
            "fatal" => (LogSeverity::Fatal, 21),
            _ => return Err(invalid("unsupported level")),
        };
        let number = log.severity_number.unwrap_or(base);
        if !(base..=base + 3).contains(&number) {
            return Err(invalid("severity_number disagrees with level"));
        }
        if log.timestamp.get().len() > 64 {
            return Err(invalid("timestamp length exceeded"));
        }
        let time = timestamp(log.timestamp.get()).ok_or_else(|| invalid("invalid timestamp"))?;
        let mut attributes = decode_attributes(&log.attributes)?;
        // Official JS SDKs attach the active span as a typed attribute, rather
        // than a top-level span_id. Keep the source attribute and populate the
        // native correlation column so log-to-span queries work as well.
        let parent_span_id = log
            .attributes
            .get("sentry.trace.parent_span_id")
            .map(|raw| {
                let attribute: Attribute<'_> =
                    serde_json::from_str(raw.get()).map_err(json_error)?;
                if attribute.ty != "string" {
                    return Err(invalid("parent span attribute must be a string"));
                }
                let id: String = serde_json::from_str(attribute.value.get()).map_err(json_error)?;
                if !valid_id(&id, 16) {
                    return Err(invalid("invalid parent span attribute"));
                }
                Ok(id.to_ascii_lowercase())
            })
            .transpose()?;
        let span_id = log.span_id.map(|id| id.to_ascii_lowercase());
        if let (Some(explicit), Some(parent)) = (&span_id, &parent_span_id) {
            if explicit != parent {
                return Err(invalid("conflicting span identifiers"));
            }
        }
        let span_id = span_id.or(parent_span_id);
        let resource = ResourceInfo {
            service_name: attributes
                .get("service.name")
                .cloned()
                .unwrap_or_else(|| "sentry".into()),
            service_version: attributes.get("sentry.release").cloned(),
            deployment_environment: attributes.get("sentry.environment").cloned(),
            ..Default::default()
        };
        attributes.insert(
            "temps.sentry.attributes".into(),
            serde_json::to_string(&log.attributes).map_err(json_error)?,
        );
        attributes.insert("temps.sentry.timestamp".into(), log.timestamp.get().into());
        attributes.insert("temps.sentry.severity_number".into(), number.to_string());
        if let Some(id) = auth.environment_id {
            attributes.insert("temps.sentry.environment_id".into(), id.to_string());
        }
        let mut attribute_types: BTreeMap<String, LogAttributeType> = attributes
            .keys()
            .map(|key| (key.clone(), LogAttributeType::String))
            .collect();
        for (key, raw) in &log.attributes {
            let attribute: Attribute<'_> = serde_json::from_str(raw.get()).map_err(json_error)?;
            let ty = match attribute.ty.as_str() {
                "integer" => LogAttributeType::Integer,
                "double" => LogAttributeType::Double,
                "boolean" => LogAttributeType::Boolean,
                "string" => LogAttributeType::String,
                _ => return Err(invalid("unsupported attribute type")),
            };
            attribute_types.insert(key.clone(), ty);
        }
        attribute_types.insert(
            "temps.sentry.severity_number".into(),
            LogAttributeType::Integer,
        );
        attribute_types.insert("temps.sentry.timestamp".into(), LogAttributeType::Double);
        if auth.environment_id.is_some() {
            attribute_types.insert(
                "temps.sentry.environment_id".into(),
                LogAttributeType::Integer,
            );
        }
        records.push(LogRecord {
            ingest_identity: None,
            attribute_types,
            project_id: auth.project_id,
            deployment_id: auth.deployment_id,
            resource,
            timestamp: time,
            observed_timestamp,
            severity,
            severity_text: log.level,
            body: log.body,
            trace_id: Some(log.trace_id.to_ascii_lowercase()),
            span_id,
            attributes,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> AuthContext {
        AuthContext {
            project_id: 41,
            environment_id: Some(42),
            deployment_id: Some(43),
            rate_limit_per_minute: None,
        }
    }

    fn header(count: usize) -> ItemHeader {
        serde_json::from_value(serde_json::json!({"type":"log", "item_count":count, "content_type":"application/vnd.sentry.items.log+json"})).unwrap()
    }

    fn log() -> serde_json::Value {
        serde_json::json!({"timestamp":1790000000.125, "trace_id":"11111111111111111111111111111111", "span_id":"2222222222222222", "body":"Synthetic log", "level":"info"})
    }

    #[test]
    fn typed_attributes_templates_resource_and_precision_survive_mapping() {
        let payload = br#"{"items":[{"timestamp":1790000000.123456789,"trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","level":"warn","severity_number":16,"body":"Synthetic item 7","attributes":{"large":{"type":"integer","value":9007199254740993},"fraction":{"type":"double","value":0.1234567890123456789},"flag":{"type":"boolean","value":true},"sentry.message.template":{"type":"string","value":"Synthetic item %s"},"sentry.message.parameter.0":{"type":"integer","value":7},"sentry.release":{"type":"string","value":"synthetic-1"},"sentry.environment":{"type":"string","value":"test"},"service.name":{"type":"string","value":"synthetic-service"}}}]}"#;
        let records = decode_logs(&header(1), payload, &auth()).unwrap();
        let record = &records[0];
        assert_eq!((record.project_id, record.deployment_id), (41, Some(43)));
        assert_eq!(record.attributes["temps.sentry.environment_id"], "42");
        assert_eq!(record.timestamp.timestamp_subsec_nanos(), 123456789);
        assert_eq!(
            record.attributes["temps.sentry.timestamp"],
            "1790000000.123456789"
        );
        assert_eq!(record.attributes["large"], "9007199254740993");
        assert_eq!(record.attribute_types["large"], LogAttributeType::Integer);
        assert_eq!(record.attribute_types["fraction"], LogAttributeType::Double);
        assert_eq!(record.attribute_types["flag"], LogAttributeType::Boolean);
        assert_eq!(
            record.attribute_types["sentry.message.template"],
            LogAttributeType::String
        );
        assert_eq!(record.attributes["fraction"], "0.1234567890123456789");
        assert!(record.attributes["temps.sentry.attributes"].contains("9007199254740993"));
        assert!(record.attributes["temps.sentry.attributes"].contains("0.1234567890123456789"));
        assert_eq!(record.attributes["temps.sentry.severity_number"], "16");
        assert_eq!(record.resource.service_name, "synthetic-service");
        assert_eq!(
            record.resource.service_version.as_deref(),
            Some("synthetic-1")
        );
        assert_eq!(
            record.trace_id.as_deref(),
            Some("11111111111111111111111111111111")
        );
        assert_eq!(record.span_id.as_deref(), Some("2222222222222222"));
    }

    #[test]
    fn timestamp_conversion_handles_exponents_negative_and_subnanosecond_values() {
        assert_eq!(timestamp("1.25e2").unwrap().timestamp(), 125);
        assert_eq!(
            timestamp("-0.1").unwrap().timestamp_subsec_nanos(),
            900000000
        );
        assert_eq!(
            timestamp("1790000000.1234567899")
                .unwrap()
                .timestamp_subsec_nanos(),
            123456789
        );
        assert!(timestamp("1e999").is_none());
        assert!(timestamp("\"123\"").is_none());
    }

    #[test]
    fn official_sdk_parent_span_attribute_populates_native_correlation() {
        let mut value = log();
        value.as_object_mut().unwrap().remove("span_id");
        value["attributes"] = serde_json::json!({
            "sentry.trace.parent_span_id": {"type":"string", "value":"ABCDEF1234567890"}
        });
        let payload = serde_json::to_vec(&serde_json::json!({"items":[value]})).unwrap();
        let records = decode_logs(&header(1), &payload, &auth()).unwrap();
        assert_eq!(records[0].span_id.as_deref(), Some("abcdef1234567890"));
        assert_eq!(
            records[0].attributes["sentry.trace.parent_span_id"],
            "ABCDEF1234567890"
        );
        assert!(records[0].attributes["temps.sentry.attributes"].contains("ABCDEF1234567890"));
    }

    #[test]
    fn rejects_invalid_or_conflicting_parent_span_attributes() {
        for attribute in [
            serde_json::json!({"type":"string", "value":"invalid"}),
            serde_json::json!({"type":"string", "value":"0000000000000000"}),
            serde_json::json!({"type":"integer", "value":2222222222222222_i64}),
            serde_json::json!({"type":"string", "value":"3333333333333333"}),
        ] {
            let mut value = log();
            value["attributes"] = serde_json::json!({"sentry.trace.parent_span_id":attribute});
            let payload = serde_json::to_vec(&serde_json::json!({"items":[value]})).unwrap();
            assert!(decode_logs(&header(1), &payload, &auth()).is_err());
        }
        let mut value = log();
        value["attributes"] = serde_json::json!({
            "sentry.trace.parent_span_id":{"type":"string", "value":"2222222222222222"}
        });
        let payload = serde_json::to_vec(&serde_json::json!({"items":[value]})).unwrap();
        assert_eq!(
            decode_logs(&header(1), &payload, &auth()).unwrap()[0]
                .span_id
                .as_deref(),
            Some("2222222222222222")
        );
    }

    #[test]
    fn rejects_count_content_type_schema_and_future_inference_contracts() {
        let payload = serde_json::to_vec(&serde_json::json!({"items":[log()]})).unwrap();
        assert!(decode_logs(&header(2), &payload, &auth()).is_err());
        let mut bad_header = header(1);
        bad_header.content_type = None;
        assert!(decode_logs(&bad_header, &payload, &auth()).is_err());
        assert!(decode_logs(&header(1), br#"{"items":[{"body":"synthetic"}]}"#, &auth()).is_err());
        assert!(decode_logs(&header(0), br#"{"items":[],"version":3}"#, &auth()).is_err());
        assert!(decode_logs(&header(0), br#"{"items":[],"version":2,"ingest_settings":{"infer_ip":"never","infer_user_agent":"never"}}"#, &auth()).is_ok());
        assert!(decode_logs(
            &header(0),
            br#"{"items":[],"ingest_settings":{"infer_ip":"invalid"}}"#,
            &auth()
        )
        .is_err());
        assert!(decode_logs(&header(0), b"{\"items\":[]}", &auth())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rejects_invalid_levels_ids_typed_values_and_reserved_metadata() {
        let mut cases = Vec::new();
        for (key, value) in [
            ("level", serde_json::json!("other")),
            (
                "trace_id",
                serde_json::json!("00000000000000000000000000000000"),
            ),
            ("span_id", serde_json::json!("invalid")),
            ("severity_number", serde_json::json!(17)),
            ("timestamp", serde_json::json!("now")),
            ("body", serde_json::json!("x".repeat(65537))),
            ("body", serde_json::json!("synthetic\0log")),
        ] {
            let mut value_log = log();
            value_log[key] = value;
            cases.push(value_log);
        }
        for attrs in [
            serde_json::json!({"test":{"type":"integer", "value":"SECRET_SYNTHETIC"}}),
            serde_json::json!({"test":{"type":"integer", "value":1.5}}),
            serde_json::json!({"test":{"type":"boolean", "value":1}}),
            serde_json::json!({"test":{"type":"array", "value":[]}}),
            serde_json::json!({"temps.sentry.timestamp":{"type":"string", "value":"forged"}}),
        ] {
            let mut value_log = log();
            value_log["attributes"] = attrs;
            cases.push(value_log);
        }
        for value_log in cases {
            let payload = serde_json::to_vec(&serde_json::json!({"items":[value_log]})).unwrap();
            let error = decode_logs(&header(1), &payload, &auth()).unwrap_err();
            assert!(!error.to_string().contains("SECRET_SYNTHETIC"));
        }
    }

    #[test]
    fn caps_batch_and_attribute_counts_before_materializing_records() {
        let payload =
            serde_json::to_vec(&serde_json::json!({"items":vec![log(); MAX_LOGS + 1]})).unwrap();
        assert!(decode_logs(&header(MAX_LOGS), &payload, &auth()).is_err());
        let mut value_log = log();
        let attrs: BTreeMap<_, _> = (0..129)
            .map(|i| {
                (
                    format!("key{i}"),
                    serde_json::json!({"type":"integer","value":i}),
                )
            })
            .collect();
        value_log["attributes"] = serde_json::to_value(attrs).unwrap();
        let payload = serde_json::to_vec(&serde_json::json!({"items":[value_log]})).unwrap();
        assert!(decode_logs(&header(1), &payload, &auth()).is_err());
    }
}
