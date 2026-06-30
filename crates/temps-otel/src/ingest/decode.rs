//! Protobuf decoding and decompression for OTLP payloads.
//!
//! Handles gzip and zstd decompression, then decodes protobuf
//! into our internal domain types.

use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use prost::Message;
use std::collections::BTreeMap;
use std::io::Read;

use crate::error::OtelError;
use crate::proto;
use crate::types::*;

/// Maximum decompressed size to prevent decompression bombs (10 MB)
const MAX_DECOMPRESSED_SIZE: usize = 10 * 1024 * 1024;

/// Decompress a request body based on Content-Encoding header.
///
/// SECURITY: Uses size-limited readers to prevent decompression bomb attacks where
/// a small compressed payload expands to consume all available memory.
pub fn decompress(body: &Bytes, encoding: Option<&str>) -> Result<Bytes, OtelError> {
    match encoding {
        Some("gzip") => {
            let decoder = flate2::read::GzDecoder::new(&body[..]);
            let mut limited_reader = decoder.take(MAX_DECOMPRESSED_SIZE as u64 + 1);
            let mut decompressed = Vec::new();
            limited_reader.read_to_end(&mut decompressed).map_err(|e| {
                OtelError::DecompressionFailed {
                    encoding: "gzip".into(),
                    reason: e.to_string(),
                }
            })?;
            if decompressed.len() > MAX_DECOMPRESSED_SIZE {
                return Err(OtelError::DecompressionFailed {
                    encoding: "gzip".into(),
                    reason: format!(
                        "Decompressed data exceeds maximum allowed size of {} bytes",
                        MAX_DECOMPRESSED_SIZE
                    ),
                });
            }
            Ok(Bytes::from(decompressed))
        }
        Some("zstd") => {
            let decompressed =
                zstd::decode_all(&body[..]).map_err(|e| OtelError::DecompressionFailed {
                    encoding: "zstd".into(),
                    reason: e.to_string(),
                })?;
            if decompressed.len() > MAX_DECOMPRESSED_SIZE {
                return Err(OtelError::DecompressionFailed {
                    encoding: "zstd".into(),
                    reason: format!(
                        "Decompressed data exceeds maximum allowed size of {} bytes",
                        MAX_DECOMPRESSED_SIZE
                    ),
                });
            }
            Ok(Bytes::from(decompressed))
        }
        Some("") | None => Ok(body.clone()),
        Some(other) => Err(OtelError::UnsupportedEncoding {
            encoding: other.to_string(),
        }),
    }
}

/// Decode an OTLP metrics export request from protobuf bytes.
pub fn decode_metrics_request(
    data: &[u8],
    project_id: i32,
    deployment_id: Option<i32>,
) -> Result<Vec<MetricPoint>, OtelError> {
    let request = proto::collector::metrics::v1::ExportMetricsServiceRequest::decode(data)
        .map_err(|e| OtelError::ProtobufDecode {
            reason: format!("Failed to decode ExportMetricsServiceRequest: {}", e),
        })?;

    let mut points = Vec::new();

    for rm in &request.resource_metrics {
        let resource = extract_resource_info(rm.resource.as_ref());

        for sm in &rm.scope_metrics {
            for metric in &sm.metrics {
                extract_metric_points(metric, &resource, project_id, deployment_id, &mut points);
            }
        }
    }

    Ok(points)
}

/// Decode an OTLP traces export request from protobuf bytes.
pub fn decode_traces_request(
    data: &[u8],
    project_id: i32,
    deployment_id: Option<i32>,
) -> Result<Vec<SpanRecord>, OtelError> {
    let request =
        proto::collector::trace::v1::ExportTraceServiceRequest::decode(data).map_err(|e| {
            OtelError::ProtobufDecode {
                reason: format!("Failed to decode ExportTraceServiceRequest: {}", e),
            }
        })?;

    let mut spans = Vec::new();

    for rs in &request.resource_spans {
        let resource = extract_resource_info(rs.resource.as_ref());

        for ss in &rs.scope_spans {
            for span in &ss.spans {
                if let Some(record) =
                    extract_span_record(span, &resource, project_id, deployment_id)
                {
                    spans.push(record);
                }
            }
        }
    }

    Ok(spans)
}

/// Decode an OTLP logs export request from protobuf bytes.
pub fn decode_logs_request(
    data: &[u8],
    project_id: i32,
    deployment_id: Option<i32>,
) -> Result<Vec<LogRecord>, OtelError> {
    let request =
        proto::collector::logs::v1::ExportLogsServiceRequest::decode(data).map_err(|e| {
            OtelError::ProtobufDecode {
                reason: format!("Failed to decode ExportLogsServiceRequest: {}", e),
            }
        })?;

    let mut records = Vec::new();

    for rl in &request.resource_logs {
        let resource = extract_resource_info(rl.resource.as_ref());

        for sl in &rl.scope_logs {
            for log_record in &sl.log_records {
                if let Some(record) =
                    extract_log_record(log_record, &resource, project_id, deployment_id)
                {
                    records.push(record);
                }
            }
        }
    }

    Ok(records)
}

// ── Helpers ─────────────────────────────────────────────────────────

fn extract_resource_info(resource: Option<&proto::resource::v1::Resource>) -> ResourceInfo {
    let Some(resource) = resource else {
        return ResourceInfo::default();
    };

    let mut info = ResourceInfo::default();
    let mut attrs = BTreeMap::new();

    for kv in &resource.attributes {
        let value = kv.value.as_ref().map(any_value_to_attribute);
        if let Some(val) = &value {
            match kv.key.as_str() {
                "service.name" => info.service_name = val.to_string(),
                "service.version" => info.service_version = Some(val.to_string()),
                "deployment.environment" | "deployment.environment.name" => {
                    info.deployment_environment = Some(val.to_string())
                }
                _ => {}
            }
            attrs.insert(kv.key.clone(), val.clone());
        }
    }

    info.attributes = attrs;
    info
}

fn any_value_to_attribute(val: &proto::common::v1::AnyValue) -> AttributeValue {
    match &val.value {
        Some(proto::common::v1::any_value::Value::StringValue(s)) => {
            AttributeValue::String(s.clone())
        }
        Some(proto::common::v1::any_value::Value::BoolValue(b)) => AttributeValue::Bool(*b),
        Some(proto::common::v1::any_value::Value::IntValue(i)) => AttributeValue::Int(*i),
        Some(proto::common::v1::any_value::Value::DoubleValue(d)) => AttributeValue::Double(*d),
        Some(proto::common::v1::any_value::Value::BytesValue(b)) => {
            AttributeValue::Bytes(b.clone())
        }
        Some(proto::common::v1::any_value::Value::ArrayValue(arr)) => {
            let items: Vec<AttributeValue> =
                arr.values.iter().map(any_value_to_attribute).collect();
            AttributeValue::Array(items)
        }
        Some(proto::common::v1::any_value::Value::KvlistValue(kvl)) => {
            let mut map = BTreeMap::new();
            for kv in &kvl.values {
                if let Some(v) = &kv.value {
                    map.insert(kv.key.clone(), any_value_to_attribute(v));
                }
            }
            AttributeValue::Map(map)
        }
        None => AttributeValue::String(String::new()),
    }
}

fn kv_to_string_map(attrs: &[proto::common::v1::KeyValue]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for kv in attrs {
        if let Some(val) = &kv.value {
            map.insert(kv.key.clone(), any_value_to_string(val));
        }
    }
    map
}

fn any_value_to_string(val: &proto::common::v1::AnyValue) -> String {
    match &val.value {
        Some(proto::common::v1::any_value::Value::StringValue(s)) => s.clone(),
        Some(proto::common::v1::any_value::Value::BoolValue(b)) => b.to_string(),
        Some(proto::common::v1::any_value::Value::IntValue(i)) => i.to_string(),
        Some(proto::common::v1::any_value::Value::DoubleValue(d)) => d.to_string(),
        Some(proto::common::v1::any_value::Value::BytesValue(b)) => hex::encode(b),
        // Preserve complex values as JSON text rather than flattening to "".
        Some(proto::common::v1::any_value::Value::ArrayValue(_))
        | Some(proto::common::v1::any_value::Value::KvlistValue(_)) => {
            any_value_to_json(val).to_string()
        }
        None => String::new(),
    }
}

/// Render an OTLP `AnyValue` as a `serde_json::Value`, preserving nested
/// arrays and key/value lists so complex attributes survive as JSON text.
fn any_value_to_json(val: &proto::common::v1::AnyValue) -> serde_json::Value {
    use serde_json::Value as J;
    match &val.value {
        Some(proto::common::v1::any_value::Value::StringValue(s)) => J::String(s.clone()),
        Some(proto::common::v1::any_value::Value::BoolValue(b)) => J::Bool(*b),
        Some(proto::common::v1::any_value::Value::IntValue(i)) => J::Number((*i).into()),
        Some(proto::common::v1::any_value::Value::DoubleValue(d)) => {
            serde_json::Number::from_f64(*d)
                .map(J::Number)
                .unwrap_or(J::Null)
        }
        Some(proto::common::v1::any_value::Value::BytesValue(b)) => J::String(hex::encode(b)),
        Some(proto::common::v1::any_value::Value::ArrayValue(arr)) => {
            J::Array(arr.values.iter().map(any_value_to_json).collect())
        }
        Some(proto::common::v1::any_value::Value::KvlistValue(kvl)) => {
            let mut map = serde_json::Map::new();
            for kv in &kvl.values {
                if let Some(v) = &kv.value {
                    map.insert(kv.key.clone(), any_value_to_json(v));
                }
            }
            J::Object(map)
        }
        None => J::Null,
    }
}

fn nanos_to_datetime(nanos: u64) -> DateTime<Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let nsecs = (nanos % 1_000_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .unwrap_or_else(Utc::now)
}

fn extract_metric_points(
    metric: &proto::metrics::v1::Metric,
    resource: &ResourceInfo,
    project_id: i32,
    deployment_id: Option<i32>,
    points: &mut Vec<MetricPoint>,
) {
    let description = if metric.description.is_empty() {
        None
    } else {
        Some(metric.description.clone())
    };

    // Build a skeleton MetricPoint with the shared per-point fields populated.
    // `start_time_unix_nano == 0` is treated as "not set" per the OTLP convention.
    let base = |start_time_unix_nano: u64,
                time_unix_nano: u64,
                attrs: &[proto::common::v1::KeyValue],
                metric_type: MetricType,
                flags: u32| {
        let mut p = MetricPoint::skeleton(
            project_id,
            deployment_id,
            resource.clone(),
            metric.name.clone(),
            metric_type,
            metric.unit.clone(),
            nanos_to_datetime(time_unix_nano),
            kv_to_string_map(attrs),
        );
        p.description = description.clone();
        p.flags = flags;
        if start_time_unix_nano > 0 {
            p.start_time = Some(nanos_to_datetime(start_time_unix_nano));
        }
        p
    };

    match &metric.data {
        Some(proto::metrics::v1::metric::Data::Gauge(gauge)) => {
            for dp in &gauge.data_points {
                let mut p = base(
                    dp.start_time_unix_nano,
                    dp.time_unix_nano,
                    &dp.attributes,
                    MetricType::Gauge,
                    dp.flags,
                );
                p.value = Some(number_data_point_value(dp));
                p.exemplars = number_exemplars(&dp.exemplars);
                points.push(p);
            }
        }
        Some(proto::metrics::v1::metric::Data::Sum(sum)) => {
            let temporality = AggregationTemporality::from_proto(sum.aggregation_temporality);
            for dp in &sum.data_points {
                let mut p = base(
                    dp.start_time_unix_nano,
                    dp.time_unix_nano,
                    &dp.attributes,
                    MetricType::Sum,
                    dp.flags,
                );
                p.temporality = Some(temporality);
                p.is_monotonic = Some(sum.is_monotonic);
                p.value = Some(number_data_point_value(dp));
                p.exemplars = number_exemplars(&dp.exemplars);
                points.push(p);
            }
        }
        Some(proto::metrics::v1::metric::Data::Histogram(hist)) => {
            let temporality = AggregationTemporality::from_proto(hist.aggregation_temporality);
            for dp in &hist.data_points {
                let mut p = base(
                    dp.start_time_unix_nano,
                    dp.time_unix_nano,
                    &dp.attributes,
                    MetricType::Histogram,
                    dp.flags,
                );
                p.temporality = Some(temporality);
                p.histogram_count = Some(dp.count);
                p.histogram_sum = dp.sum;
                p.histogram_min = dp.min;
                p.histogram_max = dp.max;
                p.histogram_bounds = Some(dp.explicit_bounds.clone());
                p.histogram_bucket_counts = Some(dp.bucket_counts.clone());
                p.exemplars = number_exemplars(&dp.exemplars);
                // Keep writing the synthetic mean as the scalar `value` so the
                // anomaly detector / continuous aggregates keep working. A later
                // Timescale phase de-synthesizes this once those readers move
                // to the structured columns.
                if dp.count > 0 {
                    p.value = dp.sum.map(|s| s / dp.count as f64);
                }
                points.push(p);
            }
        }
        Some(proto::metrics::v1::metric::Data::ExponentialHistogram(eh)) => {
            let temporality = AggregationTemporality::from_proto(eh.aggregation_temporality);
            for dp in &eh.data_points {
                let mut p = base(
                    dp.start_time_unix_nano,
                    dp.time_unix_nano,
                    &dp.attributes,
                    MetricType::ExponentialHistogram,
                    dp.flags,
                );
                p.temporality = Some(temporality);
                p.histogram_count = Some(dp.count);
                p.histogram_sum = dp.sum;
                p.histogram_min = dp.min;
                p.histogram_max = dp.max;
                p.exp_scale = Some(dp.scale);
                p.exp_zero_count = Some(dp.zero_count);
                p.exp_zero_threshold = Some(dp.zero_threshold);
                if let Some(pos) = &dp.positive {
                    p.exp_positive_offset = Some(pos.offset);
                    p.exp_positive_counts = Some(pos.bucket_counts.clone());
                }
                if let Some(neg) = &dp.negative {
                    p.exp_negative_offset = Some(neg.offset);
                    p.exp_negative_counts = Some(neg.bucket_counts.clone());
                }
                p.exemplars = number_exemplars(&dp.exemplars);
                // Keep the synthetic mean (see Histogram note above).
                if dp.count > 0 {
                    p.value = dp.sum.map(|s| s / dp.count as f64);
                }
                points.push(p);
            }
        }
        Some(proto::metrics::v1::metric::Data::Summary(summary)) => {
            for dp in &summary.data_points {
                let mut p = base(
                    dp.start_time_unix_nano,
                    dp.time_unix_nano,
                    &dp.attributes,
                    MetricType::Summary,
                    dp.flags,
                );
                p.histogram_count = Some(dp.count);
                p.histogram_sum = Some(dp.sum);
                p.summary_quantiles = Some(
                    dp.quantile_values
                        .iter()
                        .map(|q| (q.quantile, q.value))
                        .collect(),
                );
                // Keep the synthetic mean (see Histogram note above).
                if dp.count > 0 {
                    p.value = Some(dp.sum / dp.count as f64);
                }
                points.push(p);
            }
        }
        None => {}
    }
}

fn number_data_point_value(dp: &proto::metrics::v1::NumberDataPoint) -> f64 {
    match &dp.value {
        Some(proto::metrics::v1::number_data_point::Value::AsDouble(d)) => *d,
        Some(proto::metrics::v1::number_data_point::Value::AsInt(i)) => *i as f64,
        None => 0.0,
    }
}

fn exemplar_value(ex: &proto::metrics::v1::Exemplar) -> f64 {
    match &ex.value {
        Some(proto::metrics::v1::exemplar::Value::AsDouble(d)) => *d,
        Some(proto::metrics::v1::exemplar::Value::AsInt(i)) => *i as f64,
        None => 0.0,
    }
}

/// Convert a slice of OTLP exemplars into our domain `Exemplar` records,
/// hex-encoding trace/span IDs and dropping empty ones.
fn number_exemplars(exemplars: &[proto::metrics::v1::Exemplar]) -> Vec<Exemplar> {
    exemplars
        .iter()
        .map(|ex| Exemplar {
            timestamp: nanos_to_datetime(ex.time_unix_nano),
            value: exemplar_value(ex),
            trace_id: if ex.trace_id.is_empty() {
                None
            } else {
                Some(hex::encode(&ex.trace_id))
            },
            span_id: if ex.span_id.is_empty() {
                None
            } else {
                Some(hex::encode(&ex.span_id))
            },
            attributes: kv_to_string_map(&ex.filtered_attributes),
        })
        .collect()
}

fn extract_span_record(
    span: &proto::trace::v1::Span,
    resource: &ResourceInfo,
    project_id: i32,
    deployment_id: Option<i32>,
) -> Option<SpanRecord> {
    let start_time = nanos_to_datetime(span.start_time_unix_nano);
    let end_time = nanos_to_datetime(span.end_time_unix_nano);
    let duration_ms =
        (span.end_time_unix_nano as f64 - span.start_time_unix_nano as f64) / 1_000_000.0;

    let status_code = span
        .status
        .as_ref()
        .map(|s| match s.code() {
            proto::trace::v1::status::StatusCode::Ok => SpanStatusCode::Ok,
            proto::trace::v1::status::StatusCode::Error => SpanStatusCode::Error,
            _ => SpanStatusCode::Unset,
        })
        .unwrap_or(SpanStatusCode::Unset);

    let status_message = span
        .status
        .as_ref()
        .map(|s| s.message.clone())
        .unwrap_or_default();

    let kind = match span.kind() {
        proto::trace::v1::span::SpanKind::Internal => SpanKind::Internal,
        proto::trace::v1::span::SpanKind::Server => SpanKind::Server,
        proto::trace::v1::span::SpanKind::Client => SpanKind::Client,
        proto::trace::v1::span::SpanKind::Producer => SpanKind::Producer,
        proto::trace::v1::span::SpanKind::Consumer => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    };

    let events: Vec<SpanEvent> = span
        .events
        .iter()
        .map(|e| SpanEvent {
            timestamp: nanos_to_datetime(e.time_unix_nano),
            name: e.name.clone(),
            attributes: kv_to_string_map(&e.attributes),
        })
        .collect();

    let parent_span_id = if span.parent_span_id.is_empty() {
        None
    } else {
        Some(hex::encode(&span.parent_span_id))
    };

    Some(SpanRecord {
        project_id,
        deployment_id,
        resource: resource.clone(),
        trace_id: hex::encode(&span.trace_id),
        span_id: hex::encode(&span.span_id),
        parent_span_id,
        name: span.name.clone(),
        kind,
        start_time,
        end_time,
        duration_ms,
        status_code,
        status_message,
        attributes: kv_to_string_map(&span.attributes),
        events,
    })
}

fn extract_log_record(
    log: &proto::logs::v1::LogRecord,
    resource: &ResourceInfo,
    project_id: i32,
    deployment_id: Option<i32>,
) -> Option<LogRecord> {
    let timestamp = nanos_to_datetime(log.time_unix_nano);
    let observed_timestamp = if log.observed_time_unix_nano > 0 {
        nanos_to_datetime(log.observed_time_unix_nano)
    } else {
        timestamp
    };

    let severity = match log.severity_number() {
        proto::logs::v1::SeverityNumber::Trace
        | proto::logs::v1::SeverityNumber::Trace2
        | proto::logs::v1::SeverityNumber::Trace3
        | proto::logs::v1::SeverityNumber::Trace4 => LogSeverity::Trace,

        proto::logs::v1::SeverityNumber::Debug
        | proto::logs::v1::SeverityNumber::Debug2
        | proto::logs::v1::SeverityNumber::Debug3
        | proto::logs::v1::SeverityNumber::Debug4 => LogSeverity::Debug,

        proto::logs::v1::SeverityNumber::Info
        | proto::logs::v1::SeverityNumber::Info2
        | proto::logs::v1::SeverityNumber::Info3
        | proto::logs::v1::SeverityNumber::Info4 => LogSeverity::Info,

        proto::logs::v1::SeverityNumber::Warn
        | proto::logs::v1::SeverityNumber::Warn2
        | proto::logs::v1::SeverityNumber::Warn3
        | proto::logs::v1::SeverityNumber::Warn4 => LogSeverity::Warn,

        proto::logs::v1::SeverityNumber::Error
        | proto::logs::v1::SeverityNumber::Error2
        | proto::logs::v1::SeverityNumber::Error3
        | proto::logs::v1::SeverityNumber::Error4 => LogSeverity::Error,

        proto::logs::v1::SeverityNumber::Fatal
        | proto::logs::v1::SeverityNumber::Fatal2
        | proto::logs::v1::SeverityNumber::Fatal3
        | proto::logs::v1::SeverityNumber::Fatal4 => LogSeverity::Fatal,

        _ => LogSeverity::Info,
    };

    let body = log
        .body
        .as_ref()
        .map(any_value_to_string)
        .unwrap_or_default();

    let trace_id = if log.trace_id.is_empty() {
        None
    } else {
        Some(hex::encode(&log.trace_id))
    };

    let span_id = if log.span_id.is_empty() {
        None
    } else {
        Some(hex::encode(&log.span_id))
    };

    Some(LogRecord {
        project_id,
        deployment_id,
        resource: resource.clone(),
        timestamp,
        observed_timestamp,
        severity,
        severity_text: log.severity_text.clone(),
        body,
        trace_id,
        span_id,
        attributes: kv_to_string_map(&log.attributes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn test_nanos_to_datetime() {
        let nanos: u64 = 1_700_000_000_000_000_000;
        let dt = nanos_to_datetime(nanos);
        assert_eq!(dt.timestamp(), 1_700_000_000);
        assert_eq!(dt.timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn test_nanos_to_datetime_zero() {
        let dt = nanos_to_datetime(0);
        assert_eq!(dt.timestamp(), 0);
    }

    #[test]
    fn test_nanos_to_datetime_subsecond() {
        let nanos: u64 = 1_700_000_000_500_000_000; // 0.5s
        let dt = nanos_to_datetime(nanos);
        assert_eq!(dt.timestamp(), 1_700_000_000);
        assert_eq!(dt.timestamp_subsec_nanos(), 500_000_000);
    }

    #[test]
    fn test_decompress_identity() {
        let data = Bytes::from("hello world");
        let result = decompress(&data, None).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn test_decompress_empty_encoding() {
        let data = Bytes::from("hello world");
        let result = decompress(&data, Some("")).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn test_decompress_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"hello compressed world";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&Bytes::from(compressed), Some("gzip")).unwrap();
        assert_eq!(&result[..], original);
    }

    #[test]
    fn test_decompress_zstd() {
        let original = b"hello zstd compressed world";
        let compressed = zstd::encode_all(&original[..], 3).unwrap();

        let result = decompress(&Bytes::from(compressed), Some("zstd")).unwrap();
        assert_eq!(&result[..], original);
    }

    #[test]
    fn test_decompress_unsupported() {
        let data = Bytes::from("data");
        let result = decompress(&data, Some("br"));
        assert!(matches!(result, Err(OtelError::UnsupportedEncoding { .. })));
    }

    #[test]
    fn test_decode_invalid_protobuf() {
        let garbage = b"not a protobuf message";
        let result = decode_traces_request(garbage, 1, None);
        assert!(matches!(result, Err(OtelError::ProtobufDecode { .. })));
    }

    // ── Trace tree decode tests ─────────────────────────────────────

    #[test]
    fn test_decode_trace_tree_four_spans() {
        let (trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 42, Some(7)).unwrap();

        assert_eq!(spans.len(), 4, "Should decode 4 spans");

        // All spans share the same trace ID
        for span in &spans {
            assert_eq!(span.trace_id, trace_id_hex);
            assert_eq!(span.project_id, 42);
            assert_eq!(span.deployment_id, Some(7));
            assert_eq!(span.resource.service_name, "my-api-service");
        }
    }

    #[test]
    fn test_decode_trace_tree_parent_child_relationships() {
        let (_trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 1, None).unwrap();

        // Build lookup by name
        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            spans.iter().map(|s| (s.name.as_str(), s)).collect();

        let root = by_name["GET /api/users"];
        let child_db = by_name["SELECT * FROM users"];
        let child_http = by_name["POST /external/validate"];
        let grandchild = by_name["parse_response"];

        // Root has no parent
        assert!(
            root.parent_span_id.is_none(),
            "Root span should have no parent"
        );

        // Both children point to root
        assert_eq!(
            child_db.parent_span_id.as_deref(),
            Some(root.span_id.as_str()),
            "DB child should be parented to root"
        );
        assert_eq!(
            child_http.parent_span_id.as_deref(),
            Some(root.span_id.as_str()),
            "HTTP child should be parented to root"
        );

        // Grandchild points to the HTTP child
        assert_eq!(
            grandchild.parent_span_id.as_deref(),
            Some(child_http.span_id.as_str()),
            "Grandchild should be parented to the HTTP child"
        );
    }

    #[test]
    fn test_decode_trace_tree_span_kinds() {
        let (_trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 1, None).unwrap();

        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            spans.iter().map(|s| (s.name.as_str(), s)).collect();

        assert_eq!(by_name["GET /api/users"].kind, SpanKind::Server);
        assert_eq!(by_name["SELECT * FROM users"].kind, SpanKind::Client);
        assert_eq!(by_name["POST /external/validate"].kind, SpanKind::Client);
        assert_eq!(by_name["parse_response"].kind, SpanKind::Internal);
    }

    #[test]
    fn test_decode_trace_tree_durations() {
        let (_trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 1, None).unwrap();

        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            spans.iter().map(|s| (s.name.as_str(), s)).collect();

        assert!((by_name["GET /api/users"].duration_ms - 100.0).abs() < 0.01);
        assert!((by_name["SELECT * FROM users"].duration_ms - 20.0).abs() < 0.01);
        assert!((by_name["POST /external/validate"].duration_ms - 50.0).abs() < 0.01);
        assert!((by_name["parse_response"].duration_ms - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_decode_trace_tree_status_codes() {
        let (_trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 1, None).unwrap();

        // All spans in our sample have OK status
        for span in &spans {
            assert_eq!(span.status_code, SpanStatusCode::Ok);
        }
    }

    #[test]
    fn test_decode_trace_with_error_span() {
        let trace_id: [u8; 16] = [1; 16];
        let span_id: [u8; 8] = [2; 8];

        let error_span = test_support::span(
            &trace_id,
            &span_id,
            &[],
            "failing-operation",
            2, // SERVER
            1_700_000_000_000_000_000,
            1_700_000_000_200_000_000,
            2, // ERROR
        );

        let res = test_support::resource("error-service");
        let request = test_support::trace_request(res, vec![error_span]);
        let encoded = test_support::encode_proto(&request);

        let spans = decode_traces_request(&encoded, 1, None).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].status_code, SpanStatusCode::Error);
        assert_eq!(spans[0].name, "failing-operation");
        assert!((spans[0].duration_ms - 200.0).abs() < 0.01);
    }

    #[test]
    fn test_decode_trace_tree_can_reconstruct_tree() {
        let (_trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode_traces_request(&encoded, 1, None).unwrap();

        let roots = test_support::find_roots(&spans);
        assert_eq!(roots.len(), 1, "Should have exactly one root span");
        assert_eq!(roots[0].name, "GET /api/users");

        let tree = test_support::build_tree(&spans);

        // Root has 2 children
        let root_children = tree.get(&roots[0].span_id).unwrap();
        assert_eq!(root_children.len(), 2, "Root should have 2 children");

        // Find the HTTP child's span_id
        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            spans.iter().map(|s| (s.name.as_str(), s)).collect();
        let http_child = by_name["POST /external/validate"];

        // HTTP child has 1 grandchild
        let http_children = tree.get(&http_child.span_id).unwrap();
        assert_eq!(http_children.len(), 1, "HTTP child should have 1 child");

        // DB child has no children
        let db_child = by_name["SELECT * FROM users"];
        assert!(
            !tree.contains_key(&db_child.span_id),
            "DB child should be a leaf"
        );
    }

    // ── Resource extraction tests ───────────────────────────────────

    #[test]
    fn test_resource_extraction() {
        let res = proto::resource::v1::Resource {
            attributes: vec![
                test_support::kv("service.name", "payment-service"),
                test_support::kv("service.version", "1.2.3"),
                test_support::kv("deployment.environment", "production"),
            ],
            dropped_attributes_count: 0,
        };

        let info = extract_resource_info(Some(&res));
        assert_eq!(info.service_name, "payment-service");
        assert_eq!(info.service_version.as_deref(), Some("1.2.3"));
        assert_eq!(info.deployment_environment.as_deref(), Some("production"));
    }

    #[test]
    fn test_resource_extraction_none() {
        let info = extract_resource_info(None);
        assert_eq!(info.service_name, "unknown");
        assert!(info.service_version.is_none());
    }

    // ── Metrics decode tests ────────────────────────────────────────

    #[test]
    fn test_decode_gauge_metric() {
        use prost::Message;

        let request = proto::collector::metrics::v1::ExportMetricsServiceRequest {
            resource_metrics: vec![proto::metrics::v1::ResourceMetrics {
                resource: Some(test_support::resource("cpu-monitor")),
                scope_metrics: vec![proto::metrics::v1::ScopeMetrics {
                    scope: None,
                    metrics: vec![proto::metrics::v1::Metric {
                        name: "cpu.usage".into(),
                        description: String::new(),
                        unit: "percent".into(),
                        data: Some(proto::metrics::v1::metric::Data::Gauge(
                            proto::metrics::v1::Gauge {
                                data_points: vec![proto::metrics::v1::NumberDataPoint {
                                    time_unix_nano: 1_700_000_000_000_000_000,
                                    value: Some(
                                        proto::metrics::v1::number_data_point::Value::AsDouble(
                                            75.5,
                                        ),
                                    ),
                                    attributes: vec![test_support::kv("host", "web-1")],
                                    ..Default::default()
                                }],
                            },
                        )),
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };

        let encoded = request.encode_to_vec();
        let points = decode_metrics_request(&encoded, 5, None).unwrap();

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].metric_name, "cpu.usage");
        assert_eq!(points[0].metric_type, MetricType::Gauge);
        assert_eq!(points[0].unit, "percent");
        assert_eq!(points[0].value, Some(75.5));
        assert_eq!(points[0].project_id, 5);
        assert_eq!(points[0].resource.service_name, "cpu-monitor");
        assert_eq!(
            points[0].attributes.get("host").map(|s| s.as_str()),
            Some("web-1")
        );
    }

    fn metrics_request_with(
        data: proto::metrics::v1::metric::Data,
        name: &str,
        unit: &str,
        description: &str,
    ) -> Vec<u8> {
        use prost::Message;
        let request = proto::collector::metrics::v1::ExportMetricsServiceRequest {
            resource_metrics: vec![proto::metrics::v1::ResourceMetrics {
                resource: Some(test_support::resource("metrics-svc")),
                scope_metrics: vec![proto::metrics::v1::ScopeMetrics {
                    scope: None,
                    metrics: vec![proto::metrics::v1::Metric {
                        name: name.into(),
                        description: description.into(),
                        unit: unit.into(),
                        data: Some(data),
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };
        request.encode_to_vec()
    }

    #[test]
    fn test_decode_sum_monotonicity_and_temporality() {
        let data = proto::metrics::v1::metric::Data::Sum(proto::metrics::v1::Sum {
            data_points: vec![proto::metrics::v1::NumberDataPoint {
                start_time_unix_nano: 1_700_000_000_000_000_000,
                time_unix_nano: 1_700_000_001_000_000_000,
                value: Some(proto::metrics::v1::number_data_point::Value::AsInt(99)),
                attributes: vec![test_support::kv("route", "/api")],
                ..Default::default()
            }],
            aggregation_temporality: 2, // CUMULATIVE
            is_monotonic: true,
        });
        let encoded = metrics_request_with(data, "requests.total", "1", "Total requests");
        let points = decode_metrics_request(&encoded, 5, Some(3)).unwrap();

        assert_eq!(points.len(), 1);
        let p = &points[0];
        assert_eq!(p.metric_type, MetricType::Sum);
        assert_eq!(p.value, Some(99.0));
        assert_eq!(p.temporality, Some(AggregationTemporality::Cumulative));
        assert_eq!(p.is_monotonic, Some(true));
        assert_eq!(p.description.as_deref(), Some("Total requests"));
        assert!(p.start_time.is_some());
        assert_eq!(p.deployment_id, Some(3));
        assert_eq!(p.attributes.get("route").map(|s| s.as_str()), Some("/api"));
    }

    #[test]
    fn test_decode_explicit_histogram() {
        let data = proto::metrics::v1::metric::Data::Histogram(proto::metrics::v1::Histogram {
            data_points: vec![proto::metrics::v1::HistogramDataPoint {
                start_time_unix_nano: 0,
                time_unix_nano: 1_700_000_000_000_000_000,
                count: 10,
                sum: Some(123.0),
                bucket_counts: vec![2, 3, 5],
                explicit_bounds: vec![1.0, 5.0],
                min: Some(0.5),
                max: Some(9.0),
                flags: 0,
                ..Default::default()
            }],
            aggregation_temporality: 1, // DELTA
        });
        let encoded = metrics_request_with(data, "latency", "ms", "");
        let points = decode_metrics_request(&encoded, 1, None).unwrap();

        assert_eq!(points.len(), 1);
        let p = &points[0];
        assert_eq!(p.metric_type, MetricType::Histogram);
        assert_eq!(p.temporality, Some(AggregationTemporality::Delta));
        assert_eq!(p.histogram_count, Some(10));
        assert_eq!(p.histogram_sum, Some(123.0));
        assert_eq!(p.histogram_min, Some(0.5));
        assert_eq!(p.histogram_max, Some(9.0));
        assert_eq!(p.histogram_bounds.as_deref(), Some(&[1.0, 5.0][..]));
        assert_eq!(p.histogram_bucket_counts.as_deref(), Some(&[2, 3, 5][..]));
        // Synthetic mean preserved for the anomaly detector.
        assert_eq!(p.value, Some(12.3));
        // start_time_unix_nano == 0 is treated as unset.
        assert!(p.start_time.is_none());
        // Description empty → None.
        assert!(p.description.is_none());
    }

    #[test]
    fn test_decode_exponential_histogram() {
        let data = proto::metrics::v1::metric::Data::ExponentialHistogram(
            proto::metrics::v1::ExponentialHistogram {
                data_points: vec![proto::metrics::v1::ExponentialHistogramDataPoint {
                    start_time_unix_nano: 0,
                    time_unix_nano: 1_700_000_000_000_000_000,
                    count: 8,
                    sum: Some(40.0),
                    scale: 3,
                    zero_count: 1,
                    zero_threshold: 1e-6,
                    positive: Some(
                        proto::metrics::v1::exponential_histogram_data_point::Buckets {
                            offset: 2,
                            bucket_counts: vec![1, 2, 3],
                        },
                    ),
                    negative: Some(
                        proto::metrics::v1::exponential_histogram_data_point::Buckets {
                            offset: -1,
                            bucket_counts: vec![4, 5],
                        },
                    ),
                    min: Some(0.1),
                    max: Some(10.0),
                    flags: 0,
                    ..Default::default()
                }],
                aggregation_temporality: 2, // CUMULATIVE
            },
        );
        let encoded = metrics_request_with(data, "exp.hist", "ms", "");
        let points = decode_metrics_request(&encoded, 1, None).unwrap();

        assert_eq!(points.len(), 1);
        let p = &points[0];
        assert_eq!(p.metric_type, MetricType::ExponentialHistogram);
        assert_eq!(p.temporality, Some(AggregationTemporality::Cumulative));
        assert_eq!(p.histogram_count, Some(8));
        assert_eq!(p.histogram_sum, Some(40.0));
        assert_eq!(p.exp_scale, Some(3));
        assert_eq!(p.exp_zero_count, Some(1));
        assert_eq!(p.exp_zero_threshold, Some(1e-6));
        assert_eq!(p.exp_positive_offset, Some(2));
        assert_eq!(p.exp_positive_counts.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(p.exp_negative_offset, Some(-1));
        assert_eq!(p.exp_negative_counts.as_deref(), Some(&[4, 5][..]));
        // Synthetic mean preserved.
        assert_eq!(p.value, Some(5.0));
    }

    #[test]
    fn test_decode_summary_quantiles() {
        use proto::metrics::v1::summary_data_point::ValueAtQuantile;
        let data = proto::metrics::v1::metric::Data::Summary(proto::metrics::v1::Summary {
            data_points: vec![proto::metrics::v1::SummaryDataPoint {
                start_time_unix_nano: 0,
                time_unix_nano: 1_700_000_000_000_000_000,
                count: 4,
                sum: 20.0,
                quantile_values: vec![
                    ValueAtQuantile {
                        quantile: 0.5,
                        value: 4.0,
                    },
                    ValueAtQuantile {
                        quantile: 0.99,
                        value: 9.0,
                    },
                ],
                flags: 0,
                ..Default::default()
            }],
        });
        let encoded = metrics_request_with(data, "rpc.duration", "ms", "");
        let points = decode_metrics_request(&encoded, 1, None).unwrap();

        assert_eq!(points.len(), 1);
        let p = &points[0];
        assert_eq!(p.metric_type, MetricType::Summary);
        assert_eq!(p.histogram_count, Some(4));
        assert_eq!(p.histogram_sum, Some(20.0));
        assert_eq!(p.summary_quantiles, Some(vec![(0.5, 4.0), (0.99, 9.0)]));
        // Synthetic mean preserved.
        assert_eq!(p.value, Some(5.0));
    }

    #[test]
    fn test_decode_metric_exemplars() {
        let trace_id = vec![0xaa; 16];
        let span_id = vec![0xbb; 8];
        let data = proto::metrics::v1::metric::Data::Sum(proto::metrics::v1::Sum {
            data_points: vec![proto::metrics::v1::NumberDataPoint {
                time_unix_nano: 1_700_000_000_000_000_000,
                value: Some(proto::metrics::v1::number_data_point::Value::AsDouble(7.0)),
                exemplars: vec![proto::metrics::v1::Exemplar {
                    time_unix_nano: 1_700_000_000_500_000_000,
                    value: Some(proto::metrics::v1::exemplar::Value::AsDouble(7.0)),
                    span_id: span_id.clone(),
                    trace_id: trace_id.clone(),
                    filtered_attributes: vec![test_support::kv("k", "v")],
                }],
                ..Default::default()
            }],
            aggregation_temporality: 1,
            is_monotonic: false,
        });
        let encoded = metrics_request_with(data, "with.exemplar", "1", "");
        let points = decode_metrics_request(&encoded, 1, None).unwrap();

        assert_eq!(points.len(), 1);
        let p = &points[0];
        assert_eq!(p.is_monotonic, Some(false));
        assert_eq!(p.exemplars.len(), 1);
        let ex = &p.exemplars[0];
        assert_eq!(ex.value, 7.0);
        assert_eq!(
            ex.trace_id.as_deref(),
            Some(hex::encode(&trace_id).as_str())
        );
        assert_eq!(ex.span_id.as_deref(), Some(hex::encode(&span_id).as_str()));
        assert_eq!(ex.attributes.get("k").map(|s| s.as_str()), Some("v"));
    }

    #[test]
    fn test_complex_attribute_value_preserved_as_json() {
        // An array-valued attribute should survive as JSON text, not "".
        let arr = proto::common::v1::AnyValue {
            value: Some(proto::common::v1::any_value::Value::ArrayValue(
                proto::common::v1::ArrayValue {
                    values: vec![
                        proto::common::v1::AnyValue {
                            value: Some(proto::common::v1::any_value::Value::IntValue(1)),
                        },
                        proto::common::v1::AnyValue {
                            value: Some(proto::common::v1::any_value::Value::StringValue(
                                "two".into(),
                            )),
                        },
                    ],
                },
            )),
        };
        let rendered = any_value_to_string(&arr);
        assert_eq!(rendered, "[1,\"two\"]");
    }

    // ── Logs decode tests ───────────────────────────────────────────

    #[test]
    fn test_decode_log_record() {
        use prost::Message;

        let trace_id = vec![0xaa; 16];
        let span_id = vec![0xbb; 8];

        let request = proto::collector::logs::v1::ExportLogsServiceRequest {
            resource_logs: vec![proto::logs::v1::ResourceLogs {
                resource: Some(test_support::resource("log-producer")),
                scope_logs: vec![proto::logs::v1::ScopeLogs {
                    scope: None,
                    log_records: vec![proto::logs::v1::LogRecord {
                        time_unix_nano: 1_700_000_000_000_000_000,
                        observed_time_unix_nano: 1_700_000_000_001_000_000,
                        severity_number: 17, // ERROR
                        severity_text: "ERROR".into(),
                        body: Some(proto::common::v1::AnyValue {
                            value: Some(proto::common::v1::any_value::Value::StringValue(
                                "Connection refused".into(),
                            )),
                        }),
                        trace_id: trace_id.clone(),
                        span_id: span_id.clone(),
                        attributes: vec![test_support::kv("component", "db-pool")],
                        ..Default::default()
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };

        let encoded = request.encode_to_vec();
        let records = decode_logs_request(&encoded, 10, None).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].severity, LogSeverity::Error);
        assert_eq!(records[0].severity_text, "ERROR");
        assert_eq!(records[0].body, "Connection refused");
        assert_eq!(
            records[0].trace_id.as_deref(),
            Some(hex::encode(&trace_id).as_str())
        );
        assert_eq!(
            records[0].span_id.as_deref(),
            Some(hex::encode(&span_id).as_str())
        );
        assert_eq!(records[0].resource.service_name, "log-producer");
        assert_eq!(records[0].project_id, 10);
    }

    #[test]
    fn test_decode_log_severity_mapping() {
        // Verify all 24 OTel severity numbers map correctly
        let cases = vec![
            (1, LogSeverity::Trace),  // TRACE
            (4, LogSeverity::Trace),  // TRACE4
            (5, LogSeverity::Debug),  // DEBUG
            (8, LogSeverity::Debug),  // DEBUG4
            (9, LogSeverity::Info),   // INFO
            (12, LogSeverity::Info),  // INFO4
            (13, LogSeverity::Warn),  // WARN
            (16, LogSeverity::Warn),  // WARN4
            (17, LogSeverity::Error), // ERROR
            (20, LogSeverity::Error), // ERROR4
            (21, LogSeverity::Fatal), // FATAL
            (24, LogSeverity::Fatal), // FATAL4
        ];

        for (severity_number, expected) in cases {
            let request = proto::collector::logs::v1::ExportLogsServiceRequest {
                resource_logs: vec![proto::logs::v1::ResourceLogs {
                    resource: Some(test_support::resource("test")),
                    scope_logs: vec![proto::logs::v1::ScopeLogs {
                        scope: None,
                        log_records: vec![proto::logs::v1::LogRecord {
                            time_unix_nano: 1_700_000_000_000_000_000,
                            severity_number,
                            body: Some(proto::common::v1::AnyValue {
                                value: Some(proto::common::v1::any_value::Value::StringValue(
                                    "test".into(),
                                )),
                            }),
                            ..Default::default()
                        }],
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                }],
            };

            let encoded = request.encode_to_vec();
            let records = decode_logs_request(&encoded, 1, None).unwrap();
            assert_eq!(
                records[0].severity, expected,
                "Severity number {} should map to {:?}",
                severity_number, expected
            );
        }
    }

    // === Decompression bomb protection tests ===

    #[test]
    fn test_decompress_gzip_normal() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = b"Hello, OTel World!";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&Bytes::from(compressed), Some("gzip"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), data);
    }

    #[test]
    fn test_decompress_no_encoding() {
        let data = Bytes::from_static(b"raw protobuf");
        let result = decompress(&data, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"raw protobuf");
    }

    #[test]
    fn test_decompress_gzip_bomb_rejected() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create a gzip bomb: highly compressible data exceeding MAX_DECOMPRESSED_SIZE
        let large_data = vec![0u8; MAX_DECOMPRESSED_SIZE + 1024];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&large_data).unwrap();
        let compressed = encoder.finish().unwrap();

        assert!(
            compressed.len() < MAX_DECOMPRESSED_SIZE,
            "Compressed size should be much smaller than the decompressed limit"
        );

        let result = decompress(&Bytes::from(compressed), Some("gzip"));
        assert!(
            result.is_err(),
            "Gzip decompression bomb should be rejected"
        );
        match result.unwrap_err() {
            OtelError::DecompressionFailed { encoding, reason } => {
                assert_eq!(encoding, "gzip");
                assert!(
                    reason.contains("exceeds maximum"),
                    "Error should mention size limit, got: {}",
                    reason
                );
            }
            other => panic!("Expected DecompressionFailed, got: {:?}", other),
        }
    }

    #[test]
    fn test_decompress_gzip_at_exact_limit_allowed() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = vec![b'X'; MAX_DECOMPRESSED_SIZE];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).unwrap();
        let compressed = encoder.finish().unwrap();

        let result = decompress(&Bytes::from(compressed), Some("gzip"));
        assert!(result.is_ok(), "Data at exact limit should be allowed");
        assert_eq!(result.unwrap().len(), MAX_DECOMPRESSED_SIZE);
    }

    #[test]
    fn test_decompress_unsupported_encoding() {
        let result = decompress(&Bytes::from_static(b"data"), Some("br"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OtelError::UnsupportedEncoding { encoding } if encoding == "br"
        ));
    }
}
