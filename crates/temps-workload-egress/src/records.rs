// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One egress connection record → one OpenTelemetry log record.
//!
//! Attributes follow the OpenTelemetry semantic conventions where one
//! exists (`server.address`, `server.port`, `network.peer.address`,
//! `http.request.method`, `url.path`); the rest are under `temps.egress.*`.
//! Every record carries the instrumentation scope `otel.scope.name =
//! temps.egress` and `temps.platform.source = egress-proxy`, a key only
//! the platform can write (see `temps_otel::platform_logs`), so a reader can
//! tell these records from anything the workload sends about itself.
//!
//! The same record for every workload kind. What names the workload is
//! `temps.workload.kind` and the kind's own id attribute (`temps.sandbox.id`,
//! `temps.environment.id`), plus the workload columns the record is stored
//! and read by.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use temps_otel::platform_logs::PLATFORM_SOURCE_ATTRIBUTE;
use temps_otel::types::{AttributeValue, LogAttributeType, LogRecord, LogSeverity, ResourceInfo};
use temps_workload::WorkloadRef;
use temps_workload_netpol::{
    egress::record::{MAX_RECORDED_HOST, MAX_RECORDED_METHOD, MAX_RECORDED_PATH},
    ConnectionKind, ConnectionOutcome, ConnectionRecord,
};

/// The attribute every record carries with its workload's kind.
pub const WORKLOAD_KIND_ATTRIBUTE: &str = "temps.workload.kind";

/// `service.name` of every record.
pub const SERVICE_NAME: &str = "temps-egress-proxy";

/// `temps.platform.source` of every record.
pub const PLATFORM_SOURCE: &str = "egress-proxy";

/// Who the record is about, as the control plane knows it (never as the
/// node or the workload says).
#[derive(Debug, Clone)]
pub struct Attribution {
    /// The project that owns the workload, when one does.
    pub project_id: Option<i32>,
    /// Stored in the record's workload columns, so the workload's own
    /// records are read by index.
    pub workload: WorkloadRef,
    /// The id the workload's own API names it by.
    pub public_id: String,
    /// The attribute `public_id` is written under (`temps.sandbox.id`).
    pub id_attribute: &'static str,
    pub node_id: i32,
    pub node_name: String,
}

/// Why a record from a node is not acceptable. The node is trusted only to
/// say what it saw, within the bounds its own proxy keeps.
pub fn validate(record: &ConnectionRecord) -> Result<(), String> {
    if let Some(host) = &record.host {
        if host.is_empty() || host.len() > MAX_RECORDED_HOST {
            return Err(format!(
                "host of {} bytes is outside 1..={MAX_RECORDED_HOST}",
                host.len()
            ));
        }
        // A hostname or an address literal, nothing else: a query, a
        // fragment or credentials that reached the host would otherwise be
        // stored as the destination.
        if !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        {
            return Err(
                "host carries characters no hostname or address has (a query, a fragment, \
                 credentials, whitespace or control characters)"
                    .to_string(),
            );
        }
    }
    if let Some(method) = &record.method {
        if method.is_empty()
            || method.len() > MAX_RECORDED_METHOD
            || !method
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'-' || b == b'_')
        {
            return Err(format!("method '{method}' is not a recorded method"));
        }
    }
    if let Some(path) = &record.path {
        // The proxy cuts at MAX_RECORDED_PATH bytes and appends '…'.
        if path.len() > MAX_RECORDED_PATH + '…'.len_utf8() {
            return Err(format!(
                "path of {} bytes is longer than {MAX_RECORDED_PATH}",
                path.len()
            ));
        }
        if path.contains(['?', '#']) || path.chars().any(char::is_control) {
            return Err("path carries a query string, fragment or control characters".to_string());
        }
    }
    if record.count == 0 {
        return Err("count is 0".to_string());
    }
    Ok(())
}

fn millis(ms: u64) -> DateTime<Utc> {
    i64::try_from(ms)
        .ok()
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .unwrap_or_else(Utc::now)
}

/// `1.2 KiB`-style size for the body.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The destination as a person reads it: `CONNECT api.example.com:443`,
/// `GET http://api.example.com/v1/items`, `relay model`.
fn destination(record: &ConnectionRecord) -> String {
    let host = record.host.as_deref().unwrap_or("?");
    match record.kind {
        ConnectionKind::Tunnel => match record.port {
            Some(port) => format!("CONNECT {host}:{port}"),
            None => format!("CONNECT {host}"),
        },
        ConnectionKind::Forward => {
            let authority = match record.port {
                Some(80) | None => host.to_string(),
                Some(port) => format!("{host}:{port}"),
            };
            format!(
                "{} http://{authority}{}",
                record.method.as_deref().unwrap_or("_OTHER"),
                record.path.as_deref().unwrap_or("/")
            )
        }
        ConnectionKind::Relay => format!(
            "relay {}",
            record.relay.map(|r| r.as_str()).unwrap_or("unknown")
        ),
        ConnectionKind::Unknown => "request".to_string(),
    }
}

fn body(record: &ConnectionRecord) -> String {
    let destination = destination(record);
    let repeated = if record.count > 1 {
        let window = record
            .last_started_at_ms
            .saturating_sub(record.started_at_ms)
            .div_ceil(1000);
        format!(" (×{} in {window} s)", record.count)
    } else {
        String::new()
    };
    match record.outcome {
        ConnectionOutcome::Allowed => format!(
            "{destination} allowed ({}, {} sent, {} received, {} ms)",
            record.kind.as_str(),
            size(record.bytes_sent),
            size(record.bytes_received),
            record.duration_ms
        ),
        ConnectionOutcome::Refused | ConnectionOutcome::Failed => format!(
            "{destination} {}: {}{repeated}",
            record.outcome.as_str(),
            record.reason.map(|r| r.as_str()).unwrap_or("unknown"),
        ),
    }
}

/// The log record for `record`, attributed to `who`.
pub fn to_log_record(record: &ConnectionRecord, who: &Attribution) -> LogRecord {
    let mut attributes = BTreeMap::new();
    let mut types = BTreeMap::new();
    let mut text = |key: &str, value: String| {
        attributes.insert(key.to_string(), value);
    };
    text(
        "otel.scope.name",
        temps_workload_netpol::egress::record::RECORD_SCOPE.to_string(),
    );
    text(PLATFORM_SOURCE_ATTRIBUTE, PLATFORM_SOURCE.to_string());
    text(
        WORKLOAD_KIND_ATTRIBUTE,
        who.workload.kind.as_str().to_string(),
    );
    text(who.id_attribute, who.public_id.clone());
    text("temps.node.id", who.node_id.to_string());
    text("temps.node.name", who.node_name.clone());
    text("temps.egress.kind", record.kind.as_str().to_string());
    text("temps.egress.outcome", record.outcome.as_str().to_string());
    if let Some(reason) = record.reason {
        text("temps.egress.reason", reason.as_str().to_string());
    }
    if let Some(host) = &record.host {
        text("server.address", host.clone());
    }
    if let Some(peer) = record.peer {
        text("network.peer.address", peer.to_string());
    }
    if let Some(method) = &record.method {
        text("http.request.method", method.clone());
    }
    if let Some(path) = &record.path {
        text("url.path", path.clone());
    }
    if let Some(relay) = record.relay {
        text("temps.egress.relay", relay.as_str().to_string());
    }
    if record.count > 1 {
        text(
            "temps.egress.last_started_at",
            millis(record.last_started_at_ms).to_rfc3339(),
        );
    }
    let mut integer = |key: &str, value: u64| {
        attributes.insert(key.to_string(), value.to_string());
        types.insert(key.to_string(), LogAttributeType::Integer);
    };
    if let Some(port) = record.port {
        integer("server.port", u64::from(port));
    }
    integer("temps.egress.bytes_sent", record.bytes_sent);
    integer("temps.egress.bytes_received", record.bytes_received);
    integer("temps.egress.duration_ms", record.duration_ms);
    integer("temps.egress.count", u64::from(record.count));
    for key in attributes.keys() {
        types.entry(key.clone()).or_insert(LogAttributeType::String);
    }

    let mut resource = ResourceInfo {
        service_name: SERVICE_NAME.to_string(),
        ..ResourceInfo::default()
    };
    for (key, value) in [
        ("service.name", SERVICE_NAME.to_string()),
        (PLATFORM_SOURCE_ATTRIBUTE, PLATFORM_SOURCE.to_string()),
        ("temps.node.id", who.node_id.to_string()),
    ] {
        resource
            .attributes
            .insert(key.to_string(), AttributeValue::String(value));
    }

    let severity = match record.outcome {
        ConnectionOutcome::Allowed => LogSeverity::Info,
        ConnectionOutcome::Refused | ConnectionOutcome::Failed => LogSeverity::Warn,
    };
    LogRecord {
        workload: Some(who.workload),
        ingest_identity: None,
        project_id: who.project_id,
        deployment_id: None,
        resource,
        timestamp: millis(record.started_at_ms),
        observed_timestamp: Utc::now(),
        severity,
        severity_text: severity.to_string(),
        body: body(record),
        trace_id: None,
        span_id: None,
        attributes,
        attribute_types: types,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;
    use temps_workload_netpol::{EgressReason, RelayKind};

    fn who() -> Attribution {
        Attribution {
            project_id: Some(7),
            workload: WorkloadRef::new(WorkloadKind::Sandbox, 42),
            public_id: "sbx_0123456789abcdef".into(),
            id_attribute: "temps.sandbox.id",
            node_id: 3,
            node_name: "worker-a".into(),
        }
    }

    fn tunnel() -> ConnectionRecord {
        ConnectionRecord {
            workload: WorkloadRef::new(WorkloadKind::Sandbox, 11),
            started_at_ms: 1_700_000_000_000,
            last_started_at_ms: 1_700_000_000_000,
            duration_ms: 230,
            kind: ConnectionKind::Tunnel,
            outcome: ConnectionOutcome::Allowed,
            reason: None,
            host: Some("api.example.com".into()),
            port: Some(443),
            peer: Some("203.0.113.7".parse().unwrap()),
            method: None,
            path: None,
            relay: None,
            bytes_sent: 1300,
            bytes_received: 4600,
            count: 1,
            source: None,
            recorded_at: None,
        }
    }

    #[test]
    fn an_allowed_tunnel_uses_semantic_convention_attributes() {
        let log = to_log_record(&tunnel(), &who());
        let attr = |k: &str| log.attributes.get(k).map(String::as_str);
        assert_eq!(log.project_id, Some(7));
        assert_eq!(
            log.workload,
            Some(WorkloadRef::new(WorkloadKind::Sandbox, 42))
        );
        assert_eq!(attr("temps.workload.kind"), Some("sandbox"));
        assert_eq!(log.resource.service_name, SERVICE_NAME);
        assert_eq!(log.severity, LogSeverity::Info);
        assert_eq!(attr("otel.scope.name"), Some("temps.egress"));
        assert_eq!(attr("temps.platform.source"), Some("egress-proxy"));
        assert_eq!(attr("temps.sandbox.id"), Some("sbx_0123456789abcdef"));
        assert_eq!(attr("temps.node.id"), Some("3"));
        assert_eq!(attr("server.address"), Some("api.example.com"));
        assert_eq!(attr("server.port"), Some("443"));
        assert_eq!(attr("network.peer.address"), Some("203.0.113.7"));
        assert_eq!(attr("temps.egress.kind"), Some("tunnel"));
        assert_eq!(attr("temps.egress.outcome"), Some("allowed"));
        assert_eq!(attr("temps.egress.bytes_sent"), Some("1300"));
        assert_eq!(attr("temps.egress.duration_ms"), Some("230"));
        assert_eq!(attr("temps.egress.reason"), None);
        assert_eq!(attr("http.request.method"), None);
        assert_eq!(
            log.attribute_types.get("server.port"),
            Some(&LogAttributeType::Integer)
        );
        assert_eq!(
            log.body,
            "CONNECT api.example.com:443 allowed (tunnel, 1.3 KiB sent, 4.5 KiB received, 230 ms)"
        );
        assert_eq!(log.timestamp.timestamp_millis(), 1_700_000_000_000);
    }

    #[test]
    fn a_forward_carries_method_and_path() {
        let record = ConnectionRecord {
            kind: ConnectionKind::Forward,
            port: Some(80),
            method: Some("GET".into()),
            path: Some("/v1/items".into()),
            ..tunnel()
        };
        let log = to_log_record(&record, &who());
        assert_eq!(
            log.attributes
                .get("http.request.method")
                .map(String::as_str),
            Some("GET")
        );
        assert_eq!(
            log.attributes.get("url.path").map(String::as_str),
            Some("/v1/items")
        );
        assert!(log
            .body
            .starts_with("GET http://api.example.com/v1/items allowed"));
    }

    #[test]
    fn a_collapsed_refusal_says_how_many_and_is_a_warning() {
        let record = ConnectionRecord {
            outcome: ConnectionOutcome::Refused,
            reason: Some(EgressReason::NoAllowRule),
            host: Some("blocked.example.com".into()),
            peer: None,
            count: 51,
            last_started_at_ms: 1_700_000_004_200,
            bytes_sent: 0,
            bytes_received: 0,
            ..tunnel()
        };
        let log = to_log_record(&record, &who());
        assert_eq!(log.severity, LogSeverity::Warn);
        assert_eq!(
            log.body,
            "CONNECT blocked.example.com:443 refused: no_allow_rule (×51 in 5 s)"
        );
        assert_eq!(
            log.attributes
                .get("temps.egress.reason")
                .map(String::as_str),
            Some("no_allow_rule")
        );
        assert_eq!(
            log.attributes.get("temps.egress.count").map(String::as_str),
            Some("51")
        );
        assert!(log.attributes.contains_key("temps.egress.last_started_at"));
    }

    /// The same record for another kind, owned by no project: only the
    /// workload columns and the kind's own id attribute differ.
    #[test]
    fn a_record_names_its_workload_whatever_the_kind_and_needs_no_project() {
        let environment = Attribution {
            project_id: None,
            workload: WorkloadRef::new(WorkloadKind::Deployment, 9),
            public_id: "9".into(),
            id_attribute: "temps.environment.id",
            ..who()
        };
        let log = to_log_record(&tunnel(), &environment);
        assert_eq!(log.project_id, None);
        assert_eq!(
            log.workload,
            Some(WorkloadRef::new(WorkloadKind::Deployment, 9))
        );
        let attr = |k: &str| log.attributes.get(k).map(String::as_str);
        assert_eq!(attr("temps.workload.kind"), Some("deployment"));
        assert_eq!(attr("temps.environment.id"), Some("9"));
        assert_eq!(attr("temps.sandbox.id"), None);
        assert_eq!(attr("temps.platform.source"), Some("egress-proxy"));
    }

    #[test]
    fn a_relay_names_the_relay_only() {
        let record = ConnectionRecord {
            kind: ConnectionKind::Relay,
            relay: Some(RelayKind::Model),
            host: None,
            port: None,
            peer: None,
            ..tunnel()
        };
        let log = to_log_record(&record, &who());
        assert!(log.body.starts_with("relay model allowed"));
        assert!(!log.attributes.contains_key("server.address"));
    }

    #[test]
    fn records_outside_the_proxy_bounds_are_invalid() {
        assert!(validate(&tunnel()).is_ok());
        for host in [
            "93.184.216.34",
            "2606:4700::1111",
            "under_score.example.com",
        ] {
            let record = ConnectionRecord {
                host: Some(host.into()),
                ..tunnel()
            };
            assert!(validate(&record).is_ok(), "{host}");
        }
        let bad = [
            ConnectionRecord {
                host: Some("a".repeat(300)),
                ..tunnel()
            },
            ConnectionRecord {
                host: Some("evil host".into()),
                ..tunnel()
            },
            ConnectionRecord {
                host: Some("api.example.com?token=secret".into()),
                ..tunnel()
            },
            ConnectionRecord {
                host: Some("user:pass@api.example.com".into()),
                ..tunnel()
            },
            ConnectionRecord {
                host: Some("api.example.com#frag".into()),
                ..tunnel()
            },
            ConnectionRecord {
                method: Some("get".into()),
                ..tunnel()
            },
            ConnectionRecord {
                path: Some("/a?token=secret".into()),
                ..tunnel()
            },
            ConnectionRecord {
                path: Some(format!("/{}", "a".repeat(400))),
                ..tunnel()
            },
            ConnectionRecord {
                count: 0,
                ..tunnel()
            },
        ];
        for record in bad {
            assert!(validate(&record).is_err(), "{record:?}");
        }
    }

    #[test]
    fn sizes_read_as_people_expect() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MiB");
    }
}
