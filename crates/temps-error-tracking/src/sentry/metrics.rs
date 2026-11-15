// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sentry trace_metric v1/v2 adapter. Baseline: sentry-javascript 11.8.0,
//! c6492ef78c7bfb97931e43075763ce84ee8fda7e, core/src/metrics/envelope.ts.
//! Each distribution observation is a one-observation delta histogram, with
//! its scalar value retained for native raw-value quantiles. Counters are
//! increments, never cumulative totals. No metric series are synthesized.

use super::{
    envelope::{EnvelopeError, ItemHeader},
    logs,
};
use crate::providers::AuthContext;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use temps_otel::types::{AggregationTemporality, Exemplar, MetricPoint, MetricType, ResourceInfo};

#[derive(Deserialize)]
struct Metric<'a> {
    #[serde(borrow)]
    timestamp: &'a RawValue,
    trace_id: String,
    span_id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(borrow)]
    value: &'a RawValue,
    unit: Option<String>,
    #[serde(default, borrow, deserialize_with = "logs::bounded_attributes")]
    attributes: BTreeMap<String, &'a RawValue>,
}

fn invalid() -> EnvelopeError {
    EnvelopeError::InvalidPayload("invalid or unsupported trace_metric batch".into())
}

pub fn decode(
    header: &ItemHeader,
    payload: &[u8],
    auth: &AuthContext,
) -> Result<Vec<MetricPoint>, EnvelopeError> {
    if header.content_type.as_deref() != Some("application/vnd.sentry.items.trace-metric+json")
        || header.item_count.is_none_or(|n| n > logs::MAX_LOGS)
    {
        return Err(invalid());
    }
    let batch: logs::Batch<'_> = serde_json::from_slice(payload).map_err(|_| invalid())?;
    if header.item_count != Some(batch.items.len())
        || batch.version.is_some_and(|v| v != 1 && v != 2)
    {
        return Err(invalid());
    }
    super::inference::validate(batch.ingest_settings.as_ref())?;
    let mut points = Vec::with_capacity(batch.items.len());
    for raw in batch.items {
        let metric: Metric<'_> = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
        if metric.name.is_empty()
            || metric.name.len() > 256
            || metric.name.contains('\0')
            || metric
                .unit
                .as_ref()
                .is_some_and(|v| v.len() > 64 || v.contains('\0'))
            || !logs::valid_id(&metric.trace_id, 32)
            || metric
                .span_id
                .as_ref()
                .is_some_and(|id| !logs::valid_id(id, 16))
        {
            return Err(invalid());
        }
        let timestamp = if metric.timestamp.get().starts_with('"') {
            let text: String =
                serde_json::from_str(metric.timestamp.get()).map_err(|_| invalid())?;
            chrono::DateTime::parse_from_rfc3339(&text)
                .map_err(|_| invalid())?
                .to_utc()
        } else {
            logs::timestamp(metric.timestamp.get()).ok_or_else(invalid)?
        };
        if !metric.value.get().contains(['.', 'e', 'E'])
            && metric.value.get().parse::<i64>().is_err()
        {
            return Err(invalid());
        }
        let value: f64 = serde_json::from_str(metric.value.get()).map_err(|_| invalid())?;
        if !value.is_finite() || (metric.ty == "counter" && value < 0.0) {
            return Err(invalid());
        }
        let ty = match metric.ty.as_str() {
            "counter" => MetricType::Sum,
            "gauge" => MetricType::Gauge,
            "distribution" => MetricType::Histogram,
            _ => return Err(invalid()),
        };
        let mut attributes = logs::decode_attributes(&metric.attributes)?;
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
            serde_json::to_string(&metric.attributes).map_err(|_| invalid())?,
        );
        // Native numeric columns use f64; retain exact source numeric text and
        // timestamps so large integers and decimal precision are not lost.
        attributes.insert("temps.sentry.value".into(), metric.value.get().into());
        attributes.insert(
            "temps.sentry.timestamp".into(),
            metric.timestamp.get().into(),
        );
        attributes.insert("temps.sentry.metric_type".into(), metric.ty);
        if let Some(id) = auth.environment_id {
            attributes.insert("temps.sentry.environment_id".into(), id.to_string());
        }
        let mut point = MetricPoint::skeleton(
            auth.project_id,
            auth.deployment_id,
            resource,
            metric.name,
            ty,
            metric.unit.unwrap_or_else(|| "none".into()),
            timestamp,
            attributes,
        );
        point.value = Some(value);
        if ty != MetricType::Gauge {
            point.temporality = Some(AggregationTemporality::Delta);
        }
        if ty == MetricType::Sum {
            point.is_monotonic = Some(true);
        }
        if ty == MetricType::Histogram {
            point.histogram_count = Some(1);
            point.histogram_sum = Some(value);
            point.histogram_min = Some(value);
            point.histogram_max = Some(value);
            point.histogram_bounds = Some(vec![]);
            point.histogram_bucket_counts = Some(vec![1]);
        }
        point.exemplars.push(Exemplar {
            timestamp,
            value,
            trace_id: Some(metric.trace_id.to_ascii_lowercase()),
            span_id: metric.span_id.map(|id| id.to_ascii_lowercase()),
            attributes: BTreeMap::new(),
        });
        points.push(point);
    }
    Ok(points)
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
    fn header(n: usize) -> ItemHeader {
        serde_json::from_value(serde_json::json!({"type":"trace_metric","item_count":n,"content_type":"application/vnd.sentry.items.trace-metric+json"})).unwrap()
    }
    fn item(ty: &str, value: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"timestamp":1790000000.125,"name":"synthetic.metric","trace_id":"11111111111111111111111111111111","span_id":"2222222222222222","type":ty,"value":value,"unit":"millisecond","attributes":{"sentry.release":{"type":"string","value":"synthetic-release"},"flag":{"type":"boolean","value":true}}})
    }
    #[test]
    fn counters_are_delta_and_distributions_retain_observations_and_exemplars() {
        let payload=serde_json::to_vec(&serde_json::json!({"version":2,"items":[item("counter",3.into()),item("gauge",7.into()),item("distribution",12.5.into())]})).unwrap();
        let points = decode(&header(3), &payload, &auth()).unwrap();
        assert_eq!(points[0].project_id, 41);
        assert_eq!(points[0].deployment_id, Some(43));
        assert!(matches!(
            points[0].temporality,
            Some(AggregationTemporality::Delta)
        ));
        assert_eq!(points[0].is_monotonic, Some(true));
        assert!(points[1].temporality.is_none());
        assert_eq!(points[2].histogram_count, Some(1));
        assert_eq!(points[2].histogram_sum, Some(12.5));
        assert_eq!(points[2].histogram_bucket_counts, Some(vec![1]));
        assert_eq!(
            points[2].exemplars[0].span_id.as_deref(),
            Some("2222222222222222")
        );
        assert_eq!(
            points[2].resource.service_version.as_deref(),
            Some("synthetic-release")
        );
    }
    #[test]
    fn rejects_count_mismatch_future_versions_inference_negative_counters_and_untyped_attributes() {
        for batch in [
            serde_json::json!({"items":[item("counter",(-1).into())]}),
            serde_json::json!({"items":[item("other",1.into())]}),
            serde_json::json!({"items":[],"version":3}),
            serde_json::json!({"items":[],"version":2,"ingest_settings":{"infer_ip":"auto"}}),
        ] {
            assert!(decode(&header(1), &serde_json::to_vec(&batch).unwrap(), &auth()).is_err());
        }
        let payload=br#"{"items":[{"timestamp":"2026-10-19T00:00:00.123456789Z","trace_id":"11111111111111111111111111111111","name":"synthetic.precision","type":"gauge","value":9007199254740993}]}"#;
        let point = decode(&header(1), payload, &auth()).unwrap().remove(0);
        assert_eq!(point.attributes["temps.sentry.value"], "9007199254740993");
        assert_eq!(point.timestamp.timestamp_subsec_nanos(), 123456789);
    }
}
