// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Legacy SDK StatsD metrics, pinned to sentry-javascript 8.47.0,
//! eac1a8821988618eb12821011be901a12958978f, core/src/metrics/{utils,instance}.ts.
//! Counters are delta up/down sums; distributions retain observations. A gauge
//! reports its last sample, with interval min/max/sum/count retained in native
//! summary fields. Sets need a cardinality service and remain visibly unsupported.
use super::{envelope::EnvelopeError, logs};
use crate::providers::AuthContext;
use std::collections::BTreeMap;
use temps_otel::types::{AggregationTemporality, MetricPoint, MetricType, ResourceInfo};

pub struct StatsdBatch {
    pub points: Vec<MetricPoint>,
    pub unsupported_sets: usize,
    pub retained_bytes: usize,
}
fn invalid() -> EnvelopeError {
    EnvelopeError::InvalidPayload("invalid or oversized legacy StatsD batch".into())
}
fn number(text: &str) -> Result<f64, EnvelopeError> {
    if text.is_empty() || text.len() > 128 {
        return Err(invalid());
    }
    text.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(invalid)
}
fn tag(text: &str) -> Result<String, EnvelopeError> {
    // SDK escaping is independent of JSON escaping. Decode only known escapes;
    // never split on escaped delimiters or unescape an already decoded value.
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err(invalid());
                }
                let code = (chars.next(), chars.next());
                if chars.next() != Some('}') {
                    return Err(invalid());
                }
                match code {
                    (Some('2'), Some('c' | 'C')) => output.push(','),
                    (Some('7'), Some('c' | 'C')) => output.push('|'),
                    _ => return Err(invalid()),
                }
            }
            _ => return Err(invalid()),
        }
    }
    if output.contains('\0') {
        return Err(invalid());
    }
    Ok(output)
}

pub fn decode(payload: &[u8], auth: &AuthContext) -> Result<StatsdBatch, EnvelopeError> {
    // The envelope body is bounded too; impose a smaller metric-specific cap
    // before UTF-8 decoding, splitting or allocating per-point metadata.
    if payload.len() > 1024 * 1024 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(payload).map_err(|_| invalid())?;
    let mut batch = StatsdBatch {
        points: vec![],
        unsupported_sets: 0,
        retained_bytes: 0,
    };
    let mut retained_bytes = 0usize;
    for (index, line) in text.lines().enumerate() {
        if index >= 1000 || line.is_empty() || line.len() > 64 * 1024 {
            return Err(invalid());
        }
        let mut parts = line.split('|');
        let (identity, values) = parts
            .next()
            .and_then(|s| s.split_once(':'))
            .ok_or_else(invalid)?;
        let ty = parts.next().ok_or_else(invalid)?;
        if !matches!(ty, "c" | "d" | "g" | "s") {
            return Err(invalid());
        }
        let (name, unit) = identity.split_once('@').unwrap_or((identity, "none"));
        if name.is_empty()
            || name.len() > 256
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-/".contains(&c))
            || unit.is_empty()
            || unit.len() > 64
            || !unit
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_%./-".contains(&c))
        {
            return Err(invalid());
        }
        let mut attributes = BTreeMap::new();
        let mut timestamp = None;
        let mut source_timestamp = None;
        let mut saw_tags = false;
        for part in parts {
            if let Some(tags) = part.strip_prefix('#') {
                if saw_tags {
                    return Err(invalid());
                }
                saw_tags = true;
                for (n, pair) in tags.split(',').enumerate() {
                    if n >= 128 {
                        return Err(invalid());
                    }
                    let (key, value) = pair.split_once(':').ok_or_else(invalid)?;
                    let key = tag(key)?;
                    let value = tag(value)?;
                    if key.is_empty()
                        || key.len() > 128
                        || key.starts_with("temps.sentry.")
                        || value.len() > 16 * 1024
                        || attributes.insert(key, value).is_some()
                    {
                        return Err(invalid());
                    }
                }
            } else if let Some(value) = part.strip_prefix('T') {
                if timestamp.is_some() {
                    return Err(invalid());
                }
                source_timestamp = Some(value);
                timestamp = logs::timestamp(value);
                if timestamp.is_none() {
                    return Err(invalid());
                }
            } else {
                return Err(invalid());
            }
        }
        // Official SDKs supply bucket timestamps; do not substitute receive time
        // for missing/invalid timestamps and thereby create misleading trends.
        let timestamp = timestamp.ok_or_else(invalid)?;
        if ty == "s" {
            if values.is_empty() || values.split(':').count() > 1000 {
                return Err(invalid());
            }
            batch.unsupported_sets += 1;
            continue;
        }
        let numbers = values
            .split(':')
            .take(1001)
            .map(number)
            .collect::<Result<Vec<_>, _>>()?;
        if numbers.is_empty() || numbers.len() > 1000 {
            return Err(invalid());
        }
        let resource = ResourceInfo {
            service_name: attributes
                .get("service.name")
                .cloned()
                .unwrap_or_else(|| "sentry".into()),
            service_version: attributes.get("release").cloned(),
            deployment_environment: attributes.get("environment").cloned(),
            ..Default::default()
        };
        if ty != "d" {
            attributes.insert("temps.sentry.statsd.values".into(), values.into());
        }
        attributes.insert("temps.sentry.metric_type".into(), format!("statsd.{ty}"));
        if let Some(source) = source_timestamp {
            attributes.insert("temps.sentry.timestamp".into(), source.into());
        }
        if let Some(environment) = auth.environment_id {
            attributes.insert(
                "temps.sentry.environment_id".into(),
                environment.to_string(),
            );
        }
        let expansion = if ty == "d" { numbers.len() } else { 1 };
        // Prevent a compact distribution with many shared tags from expanding
        // into hundreds of MiB of cloned labels on a small machine.
        retained_bytes = retained_bytes.saturating_add(
            expansion.saturating_mul(
                512 + name.len()
                    + unit.len()
                    + attributes
                        .iter()
                        .map(|(k, v)| k.len() + v.len())
                        .sum::<usize>(),
            ),
        );
        if retained_bytes > 4 * 1024 * 1024 || batch.points.len() + expansion > 1000 {
            return Err(invalid());
        }
        let template = MetricPoint::skeleton(
            auth.project_id,
            auth.deployment_id,
            resource,
            name.into(),
            match ty {
                "c" => MetricType::Sum,
                "d" => MetricType::Histogram,
                _ => MetricType::Gauge,
            },
            unit.into(),
            timestamp,
            attributes,
        );
        match ty {
            "c" => {
                let mut point = template;
                let sum: f64 = numbers.iter().sum();
                if !sum.is_finite() {
                    return Err(invalid());
                }
                point.value = Some(sum);
                point.temporality = Some(AggregationTemporality::Delta);
                point.is_monotonic = Some(false);
                batch.points.push(point);
            }
            "g" => {
                if numbers.len() != 1 && numbers.len() != 5 {
                    return Err(invalid());
                }
                let mut point = template;
                point.value = Some(numbers[0]);
                if numbers.len() == 5 {
                    let count = values
                        .rsplit(':')
                        .next()
                        .ok_or_else(invalid)?
                        .parse::<u64>()
                        .map_err(|_| invalid())?;
                    if count == 0
                        || count > i64::MAX as u64
                        || numbers[1] > numbers[0]
                        || numbers[0] > numbers[2]
                    {
                        return Err(invalid());
                    }
                    point.histogram_min = Some(numbers[1]);
                    point.histogram_max = Some(numbers[2]);
                    point.histogram_sum = Some(numbers[3]);
                    point.histogram_count = Some(count);
                }
                batch.points.push(point);
            }
            "d" => {
                for (value, raw) in numbers.into_iter().zip(values.split(':')) {
                    let mut point = template.clone();
                    point
                        .attributes
                        .insert("temps.sentry.value".into(), raw.into());
                    point.value = Some(value);
                    point.temporality = Some(AggregationTemporality::Delta);
                    point.histogram_count = Some(1);
                    point.histogram_sum = Some(value);
                    point.histogram_min = Some(value);
                    point.histogram_max = Some(value);
                    point.histogram_bounds = Some(vec![]);
                    point.histogram_bucket_counts = Some(vec![1]);
                    batch.points.push(point);
                }
            }
            _ => return Err(invalid()),
        }
        if batch.points.len() > 1000 {
            return Err(invalid());
        }
    }
    batch.retained_bytes = retained_bytes;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn auth() -> AuthContext {
        AuthContext {
            project_id: 7,
            environment_id: Some(8),
            deployment_id: Some(9),
            rate_limit_per_minute: None,
        }
    }
    #[test]
    fn legacy_delta_distribution_gauge_rollups_and_unsupported_sets_are_explicit() {
        let batch=decode(b"synthetic.counter@none:2:-1|c|#release:synthetic,environment:test,tag:a\\u{2c}b\\u{7c}c\\nend|T1790000000\nsynthetic.gauge@byte:7:2:7:9:2|g|T1790000000\nsynthetic.distribution@millisecond:1:9|d|T1790000000\nsynthetic.set@none:1:2|s|T1790000000\n",&auth()).unwrap();
        assert_eq!(batch.points.len(), 4);
        assert_eq!(batch.unsupported_sets, 1);
        assert_eq!(batch.points[0].value, Some(1.0));
        assert_eq!(batch.points[0].is_monotonic, Some(false));
        assert_eq!(batch.points[0].attributes["tag"], "a,b|c\nend");
        assert_eq!(
            batch.points[0].resource.service_version.as_deref(),
            Some("synthetic")
        );
        assert_eq!(batch.points[1].value, Some(7.0));
        assert_eq!(batch.points[1].histogram_count, Some(2));
        assert_eq!(batch.points[1].histogram_sum, Some(9.0));
        assert_eq!(batch.points[2].histogram_count, Some(1));
        assert_eq!(batch.points[3].value, Some(9.0));
        assert!(batch
            .points
            .iter()
            .all(|p| p.project_id == 7 && p.deployment_id == Some(9)));
    }
    #[test]
    fn malformed_and_expansion_limits_fail_before_writes() {
        for payload in [
            "x@none:NaN|c|T1790000000",
            "x@none:1|c",
            "x@none:1|c|T1790000000|T1790000001",
            "x@none:1:2|g|T1790000000",
            "x@none:1|c|#k:a,k:b|T1790000000",
            "x@none:1|c|#temps.sentry.x:v|T1790000000",
            "x@none:1|c|#key:a\\u{2c|T1790000000",
        ] {
            assert!(decode(payload.as_bytes(), &auth()).is_err());
        }
        let payload = format!("x@none:{}|d|T1790000000", vec!["1"; 1001].join(":"));
        assert!(decode(payload.as_bytes(), &auth()).is_err());
        let payload = "x@none:1|c|T1790000000\n".repeat(1001);
        assert!(decode(payload.as_bytes(), &auth()).is_err());
        assert!(decode(&[0xff], &auth()).is_err());
    }
}
