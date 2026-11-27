// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log records the platform observes, told apart from the ones a workload
//! sends about itself.
//!
//! A workload can send any attribute it likes over OTLP, including one that
//! claims "Temps saw this". So the marker of a platform-observed record is a
//! namespace only the platform can write: every key under
//! [`PLATFORM_ATTRIBUTE_PREFIX`] is removed from records (and their
//! resources) on the ordinary ingest path ([`OtelService::ingest_logs`]),
//! and only [`OtelService::ingest_platform_logs`] keeps it. A reader who
//! filters on [`PLATFORM_SOURCE_ATTRIBUTE`] therefore sees only records the
//! platform wrote.
//!
//! [`OtelService::ingest_logs`]: crate::OtelService::ingest_logs
//! [`OtelService::ingest_platform_logs`]: crate::OtelService::ingest_platform_logs

use crate::types::LogRecord;

/// Attribute keys only the platform may write.
pub const PLATFORM_ATTRIBUTE_PREFIX: &str = "temps.platform.";

/// Which platform component observed a record (`egress-proxy`).
pub const PLATFORM_SOURCE_ATTRIBUTE: &str = "temps.platform.source";

/// Removes every platform-only attribute from records that came from a
/// workload. Returns how many attributes were removed.
pub fn strip_platform_attributes(records: &mut [LogRecord]) -> usize {
    let mut removed = 0;
    for record in records {
        let before = record.attributes.len() + record.resource.attributes.len();
        record
            .attributes
            .retain(|key, _| !key.starts_with(PLATFORM_ATTRIBUTE_PREFIX));
        record
            .attribute_types
            .retain(|key, _| !key.starts_with(PLATFORM_ATTRIBUTE_PREFIX));
        record
            .resource
            .attributes
            .retain(|key, _| !key.starts_with(PLATFORM_ATTRIBUTE_PREFIX));
        removed += before - record.attributes.len() - record.resource.attributes.len();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AttributeValue, LogAttributeType, LogSeverity, ResourceInfo};
    use std::collections::BTreeMap;

    fn record(attributes: &[(&str, &str)]) -> LogRecord {
        let mut resource = ResourceInfo::default();
        resource.attributes.insert(
            "temps.platform.source".into(),
            AttributeValue::String("egress-proxy".into()),
        );
        resource
            .attributes
            .insert("service.name".into(), AttributeValue::String("app".into()));
        LogRecord {
            workload: None,
            ingest_identity: None,
            project_id: Some(1),
            deployment_id: None,
            resource,
            timestamp: chrono::Utc::now(),
            observed_timestamp: chrono::Utc::now(),
            severity: LogSeverity::Info,
            severity_text: "INFO".into(),
            body: "hello".into(),
            trace_id: None,
            span_id: None,
            attributes: attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            attribute_types: BTreeMap::from([(
                "temps.platform.source".to_string(),
                LogAttributeType::String,
            )]),
        }
    }

    #[test]
    fn a_workload_cannot_mark_its_records_as_platform_observed() {
        let mut records = vec![record(&[
            ("temps.platform.source", "egress-proxy"),
            ("temps.platform.anything", "x"),
            ("temps.sandbox.id", "sbx_1"),
            ("http.request.method", "GET"),
        ])];
        assert_eq!(strip_platform_attributes(&mut records), 3);
        let kept: Vec<&str> = records[0].attributes.keys().map(String::as_str).collect();
        assert_eq!(kept, ["http.request.method", "temps.sandbox.id"]);
        assert!(records[0].attribute_types.is_empty());
        assert!(records[0].resource.attributes.contains_key("service.name"));
        assert!(!records[0]
            .resource
            .attributes
            .contains_key("temps.platform.source"));
    }
}
