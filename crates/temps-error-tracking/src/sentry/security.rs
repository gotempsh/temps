// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Browser CSP reports become native issues; NEL becomes native structured
//! logs, matching the current Relay ca7e20d protocol destination. These adapters
//! use the same scoped ingestion services, admission and retention as SDK data.
use super::envelope::{ItemHeader, ItemType};
use crate::providers::{AuthContext, ParsedErrorEvent};
use relay_event_schema::protocol::{Csp, Event, SecurityReportType};
use relay_protocol::{Annotated, FromValue};
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use std::collections::BTreeMap;
use temps_otel::types::{LogAttributeType, LogRecord, LogSeverity, ResourceInfo};

/// Adapt the two official browser report HTTP endpoints to the shared envelope
/// dispatch, preserving auth/admission/native-service behavior for mixed data.
pub fn frame_browser_reports(
    body: &[u8],
    nel: bool,
    params: &std::collections::HashMap<String, String>,
) -> Result<Vec<u8>, &'static str> {
    let entries = if body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        serde_json::from_slice::<Vec<&RawValue>>(body)
            .map_err(|_| "invalid browser report array")?
    } else {
        if nel {
            return Err("NEL expects a Reporting API array");
        }
        vec![serde_json::from_slice::<&RawValue>(body).map_err(|_| "invalid browser report JSON")?]
    };
    if entries.len() > 100 {
        return Err("at most 100 browser reports per request");
    }
    let mut bytes = b"{}\n".to_vec();
    for entry in entries {
        let mut header = serde_json::json!({"type":if nel {"nel"}else{"raw_security"},"length":entry.get().len(),"content_type":"application/json"});
        for key in ["sentry_release", "sentry_environment"] {
            if let Some(value) = params.get(key) {
                if value.len() > 1024 || value.contains('\0') {
                    return Err("invalid browser report metadata");
                }
                header[key] = Value::String(value.clone());
            }
        }
        bytes.extend(serde_json::to_vec(&header).map_err(|_| "invalid report headers")?);
        bytes.push(b'\n');
        bytes.extend_from_slice(entry.get().as_bytes());
        bytes.push(b'\n');
    }
    if bytes.len() > 10 * 1024 * 1024 {
        return Err("browser report batch exceeds 10 MiB");
    }
    Ok(bytes)
}

fn item_id(
    event: &Value,
    envelope_id: Option<String>,
    index: usize,
) -> Result<String, &'static str> {
    if let Some(value) = event.get("event_id") {
        let text = value.as_str().ok_or("invalid security event ID")?;
        let id = uuid::Uuid::parse_str(text).map_err(|_| "invalid security event ID")?;
        if id.is_nil() {
            return Err("zero security event ID");
        }
        return Ok(id.simple().to_string());
    }
    if let Some(id) = envelope_id {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(&id);
        hash.update((index as u64).to_be_bytes());
        let bytes = hash.finalize();
        let mut uuid = [0_u8; 16];
        uuid.copy_from_slice(&bytes[..16]);
        return Ok(uuid::Uuid::from_bytes(uuid).simple().to_string());
    }
    Ok(uuid::Uuid::new_v4().simple().to_string())
}

pub fn csp(
    header: &ItemHeader,
    payload: &[u8],
    auth: &AuthContext,
    envelope_id: Option<String>,
    index: usize,
) -> Result<Option<ParsedErrorEvent>, &'static str> {
    if payload.len() > 64 * 1024 {
        return Err("CSP report exceeds 64 KiB");
    }
    let source: Value = serde_json::from_slice(payload).map_err(|_| "invalid security JSON")?;
    let mut raw = if header.ty == ItemType::RawSecurity {
        match SecurityReportType::from_json(payload)
            .map_err(|_| "invalid security report schema")?
        {
            Some(SecurityReportType::Csp) => {}
            _ => return Ok(None),
        }
        let mut event = Event::default();
        Csp::apply_to_event(payload, &mut event).map_err(|_| "invalid CSP report schema")?;
        let normalized = Annotated::new(event)
            .to_json()
            .map_err(|_| "CSP report normalization failed")?;
        let mut value: Value =
            serde_json::from_str(&normalized).map_err(|_| "CSP report normalization failed")?;
        value["type"] = Value::String("csp".into());
        value["temps.security_report"] = source.clone();
        value
    } else {
        if source.get("type").and_then(Value::as_str) != Some("csp")
            || !source.get("csp").is_some_and(Value::is_object)
        {
            return Ok(None);
        }
        source
    };
    let id = item_id(&raw, envelope_id, index)?;
    raw["event_id"] = Value::String(id.clone());
    for (key, field) in [
        ("sentry_release", "release"),
        ("sentry_environment", "environment"),
    ] {
        if let Some(value) = header.other.get(key) {
            let text = value
                .as_str()
                .filter(|v| v.len() <= 1024 && !v.contains('\0'))
                .ok_or("invalid security item metadata")?;
            raw[field] = Value::String(text.to_owned());
        }
    }
    super::mapper::validate_event_metadata(&raw).map_err(|_| "invalid security event metadata")?;
    let event = Event::from_value(raw.clone().into());
    let mut data = super::mapper::convert_sentry_event_to_error_data(
        &event,
        raw.clone(),
        auth.project_id,
        auth.environment_id,
        auth.deployment_id,
    )
    .map_err(|_| "CSP event mapping failed")?;
    data.source = Some("sentry".into());
    data.exception_type = Some("CSPViolation".into());
    if let Some(exception) = data.exceptions.last_mut() {
        exception.exception_type = "CSPViolation".into();
    }
    Ok(Some(ParsedErrorEvent {
        event_id: id,
        raw_event: raw,
        error_data: data,
    }))
}

#[derive(Deserialize)]
struct Nel<'a> {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    age: u64,
    url: String,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(borrow)]
    body: BTreeMap<&'a str, &'a RawValue>,
}

pub fn nel(
    header: &ItemHeader,
    payload: &[u8],
    auth: &AuthContext,
) -> Result<Option<LogRecord>, &'static str> {
    if payload.len() > 16 * 1024 {
        return Err("NEL report exceeds 16 KiB");
    }
    let report: Nel<'_> =
        serde_json::from_slice(payload).map_err(|_| "invalid NEL report schema")?;
    if report.ty != "network-error" {
        return Ok(None);
    }
    if report.body.len() > 32
        || report.age > 31 * 24 * 60 * 60 * 1000
        || report.url.len() > 4096
        || report.url.contains('\0')
        || report
            .user_agent
            .as_ref()
            .is_some_and(|v| v.len() > 4096 || v.contains('\0'))
    {
        return Err("NEL metadata limits exceeded");
    }
    let url = url::Url::parse(&report.url).map_err(|_| "invalid NEL URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("unsupported NEL URL scheme");
    }
    let mut attributes = BTreeMap::new();
    let mut attribute_types = BTreeMap::new();
    let mut insert = |key: &str, value: String, ty: LogAttributeType| {
        attributes.insert(key.to_owned(), value);
        attribute_types.insert(key.to_owned(), ty);
    };
    insert("url.full", report.url.clone(), LogAttributeType::String);
    if let Some(domain) = url.host_str() {
        insert("url.domain", domain.to_owned(), LogAttributeType::String);
    }
    insert(
        "temps.sentry.nel",
        std::str::from_utf8(payload)
            .map_err(|_| "invalid NEL UTF-8")?
            .to_owned(),
        LogAttributeType::Object,
    );
    insert(
        "temps.sentry.source",
        "nel".into(),
        LogAttributeType::String,
    );
    insert("nel.age", report.age.to_string(), LogAttributeType::Integer);
    if let Some(agent) = report.user_agent {
        insert("user_agent.original", agent, LogAttributeType::String);
    }
    if let Some(environment) = auth.environment_id {
        insert(
            "temps.sentry.environment_id",
            environment.to_string(),
            LogAttributeType::Integer,
        );
    }
    let mut error_type = None;
    for (source, target) in [
        ("type", "nel.type"),
        ("phase", "nel.phase"),
        ("method", "http.request.method"),
        ("referrer", "nel.referrer"),
        ("server_ip", "server.address"),
        ("protocol", "network.protocol.name"),
    ] {
        if let Some(value) = report.body.get(source) {
            let value: String =
                serde_json::from_str(value.get()).map_err(|_| "invalid NEL string field")?;
            if value.len() > 4096 || value.contains('\0') {
                return Err("NEL attribute exceeds limits");
            }
            if source == "type" {
                if value.is_empty() || value.len() > 128 {
                    return Err("invalid NEL error type");
                }
                error_type = Some(value.clone());
            }
            if source == "server_ip"
                && !value.is_empty()
                && value.parse::<std::net::IpAddr>().is_err()
            {
                return Err("invalid NEL server IP");
            }
            if source == "protocol" {
                let (name, version) = value
                    .split_once('/')
                    .map_or((value.as_str(), None), |(n, v)| (n, Some(v)));
                insert(target, name.to_owned(), LogAttributeType::String);
                if let Some(version) = version {
                    insert(
                        "network.protocol.version",
                        version.to_owned(),
                        LogAttributeType::String,
                    );
                }
            } else {
                insert(target, value, LogAttributeType::String);
            }
        }
    }
    for (source, target) in [
        ("status_code", "http.response.status_code"),
        ("elapsed_time", "http.request.duration"),
    ] {
        if let Some(value) = report.body.get(source) {
            let number: u64 =
                serde_json::from_str(value.get()).map_err(|_| "invalid NEL integer field")?;
            if number > i64::MAX as u64 || (source == "status_code" && number > 599) {
                return Err("NEL number exceeds limits");
            }
            insert(target, number.to_string(), LogAttributeType::Integer);
        }
    }
    if let Some(value) = report.body.get("sampling_fraction") {
        let fraction: f64 =
            serde_json::from_str(value.get()).map_err(|_| "invalid NEL sampling fraction")?;
        if !(0.0..=1.0).contains(&fraction) {
            return Err("invalid NEL sampling fraction");
        }
        insert(
            "nel.sampling_fraction",
            value.get().to_owned(),
            LogAttributeType::Double,
        );
    }
    let error_type = error_type.ok_or("NEL body is missing error type")?;
    let now = chrono::Utc::now();
    let severity = if error_type == "ok" {
        LogSeverity::Info
    } else {
        LogSeverity::Warn
    };
    let mut resource = ResourceInfo {
        service_name: "sentry.nel".into(),
        ..Default::default()
    };
    for (key, target) in [
        ("sentry_release", &mut resource.service_version),
        ("sentry_environment", &mut resource.deployment_environment),
    ] {
        if let Some(value) = header.other.get(key) {
            *target = Some(
                value
                    .as_str()
                    .filter(|s| s.len() <= 1024 && !s.contains('\0'))
                    .ok_or("invalid NEL item metadata")?
                    .to_owned(),
            );
        }
    }
    Ok(Some(LogRecord {
        ingest_identity: None,
        project_id: auth.project_id,
        deployment_id: auth.deployment_id,
        resource,
        timestamp: now - chrono::Duration::milliseconds(report.age as i64),
        observed_timestamp: now,
        severity,
        severity_text: severity.to_string(),
        body: format!("Network report: {error_type}"),
        trace_id: None,
        span_id: None,
        attributes,
        attribute_types,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn auth() -> AuthContext {
        AuthContext {
            project_id: 7,
            environment_id: Some(8),
            deployment_id: Some(9),
            rate_limit_per_minute: None,
        }
    }
    fn header(ty: &str) -> ItemHeader {
        serde_json::from_value(json!({"type":ty,"sentry_release":"synthetic-release","sentry_environment":"synthetic-test"})).unwrap()
    }
    #[test]
    fn csp_legacy_reporting_api_and_normalized_security_become_native_issues() {
        let legacy = json!({"csp-report":{"document-uri":"https://synthetic.example/page","effective-directive":"script-src-elem","violated-directive":"script-src-elem","blocked-uri":"inline","original-policy":"script-src 'none'","disposition":"enforce"}});
        let modern = json!({"type":"csp-violation","url":"https://synthetic.example/page","body":{"documentURL":"https://synthetic.example/page","effectiveDirective":"script-src-elem","blockedURL":"inline","originalPolicy":"script-src 'none'","disposition":"report"}});
        for report in [legacy, modern] {
            let event = csp(
                &header("raw_security"),
                &serde_json::to_vec(&report).unwrap(),
                &auth(),
                None,
                0,
            )
            .unwrap()
            .unwrap();
            assert_eq!(event.error_data.project_id, 7);
            assert_eq!(event.error_data.environment_id, Some(8));
            assert_eq!(event.error_data.deployment_id, Some(9));
            assert_eq!(
                event.error_data.exception_type.as_deref(),
                Some("CSPViolation")
            );
            assert_eq!(event.raw_event["csp"]["blocked_uri"], "inline");
            assert_eq!(event.raw_event["release"], "synthetic-release");
            let normalized = csp(
                &header("security"),
                &serde_json::to_vec(&event.raw_event).unwrap(),
                &auth(),
                None,
                0,
            )
            .unwrap()
            .unwrap();
            assert_eq!(event.event_id, normalized.event_id);
        }
        assert!(csp(
            &header("raw_security"),
            br#"{"type":"deprecation","body":{}}"#,
            &auth(),
            None,
            0
        )
        .unwrap()
        .is_none());
        assert!(csp(&header("raw_security"), b"invalid", &auth(), None, 0).is_err());
    }
    #[test]
    fn nel_preserves_typed_attributes_and_uses_logs_without_invented_trace_ids() {
        let report=br#"{"type":"network-error","age":1000,"url":"https://synthetic.example/api","user_agent":"SyntheticAgent/1","body":{"type":"dns.name_not_resolved","phase":"dns","status_code":0,"elapsed_time":12,"sampling_fraction":0.1234567890123456789,"server_ip":"192.0.2.1","protocol":"http/2","method":"GET"}}"#;
        let log = nel(&header("nel"), report, &auth()).unwrap().unwrap();
        assert_eq!(log.project_id, 7);
        assert_eq!(log.deployment_id, Some(9));
        assert_eq!(log.attributes["temps.sentry.environment_id"], "8");
        assert_eq!(
            log.attributes["nel.sampling_fraction"],
            "0.1234567890123456789"
        );
        assert_eq!(
            log.attribute_types["http.response.status_code"],
            LogAttributeType::Integer
        );
        assert_eq!(
            log.resource.service_version.as_deref(),
            Some("synthetic-release")
        );
        assert_eq!(log.attributes["network.protocol.version"], "2");
        assert_eq!(log.severity, LogSeverity::Warn);
        assert!(log.trace_id.is_none());
        assert_eq!(
            (log.observed_timestamp - log.timestamp).num_milliseconds(),
            1000
        );
        assert_eq!(
            log.attributes["temps.sentry.nel"],
            std::str::from_utf8(report).unwrap()
        );
        for body in [
            json!({"type":"ok","sampling_fraction":2}),
            json!({"type":"ok","status_code":600}),
            json!({"type":"ok","elapsed_time":-1}),
            json!({"type":"ok","server_ip":"invalid"}),
        ] {
            assert!(nel(
                &header("nel"),
                &serde_json::to_vec(
                    &json!({"type":"network-error","url":"https://synthetic.example","body":body})
                )
                .unwrap(),
                &auth()
            )
            .is_err());
        }
    }
    #[test]
    fn browser_report_framing_accepts_multiline_and_bounds_batches() {
        let params = std::collections::HashMap::new();
        let framed = frame_browser_reports(
            b"[\n {\"type\":\"deprecation\",\"body\":{}}\n]",
            false,
            &params,
        )
        .unwrap();
        let parsed = super::super::envelope::Envelope::parse_for_ingestion(&framed).unwrap();
        assert_eq!(parsed.items_with_payloads().count(), 1);
        assert!(frame_browser_reports(b"{}", true, &params).is_err());
        assert!(frame_browser_reports(
            &serde_json::to_vec(&vec![json!({}); 101]).unwrap(),
            false,
            &params
        )
        .is_err());
    }
}
