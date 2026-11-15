// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sentry Error Provider Implementation

use async_trait::async_trait;
use relay_protocol::FromValue;
use serde_json::Value;
use std::sync::Arc;

use super::{AuthContext, ErrorProvider, ParsedErrorEvent, ProviderError};
use crate::sentry::{dsn_service::DSNService, envelope::Envelope, envelope::EnvelopeItem, mapper};

/// Sentry error provider implementation
pub struct SentryProvider {
    dsn_service: Arc<DSNService>,
}

impl SentryProvider {
    pub fn new(dsn_service: Arc<DSNService>) -> Self {
        Self { dsn_service }
    }
}

/// Fully validated dispatch result. No writes happen until every supported item
/// has been decoded. Unsupported names are collapsed to the fixed ItemType set.
pub struct ParsedSentryEnvelope {
    pub events: Vec<ParsedErrorEvent>,
    pub logs: Vec<temps_otel::types::LogRecord>,
    pub feedback: Vec<crate::services::feedback_service::FeedbackSubmission>,
    pub spans: Vec<temps_otel::types::SpanRecord>,
    pub metrics: Vec<temps_otel::types::MetricPoint>,
    pub replay: Option<crate::sentry::replay::ReplaySegment>,
    pub client_outcomes: Vec<crate::sentry::client_reports::ClientReport>,
    pub check_ins: Vec<temps_monitoring::cron_monitors::MonitorCheckIn>,
    pub unsupported: std::collections::BTreeMap<&'static str, usize>,
}

impl SentryProvider {
    pub fn parse_envelope(
        &self,
        payload: &[u8],
        auth: &AuthContext,
    ) -> Result<ParsedSentryEnvelope, ProviderError> {
        self.parse_envelope_with_request(payload, auth, None)
    }

    pub(crate) fn parse_envelope_with_request(
        &self,
        payload: &[u8],
        auth: &AuthContext,
        request: Option<&crate::sentry::inference::RequestInference>,
    ) -> Result<ParsedSentryEnvelope, ProviderError> {
        // Parse Sentry envelope
        let envelope = Envelope::parse_for_ingestion(payload)
            .map_err(|e| ProviderError::Parsing(e.to_string()))?;

        let mut parsed_events = Vec::new();
        let mut logs = Vec::new();
        let mut feedback = Vec::new();
        let mut feedback_bytes = 0_usize;
        let mut check_ins = Vec::new();
        let mut client_outcomes = Vec::new();
        let mut client_outcome_entries = 0usize;
        let mut unsupported = std::collections::BTreeMap::new();
        let mut spans = Vec::new();
        let mut metrics = Vec::new();
        let mut statsd_bytes = 0usize;
        let mut replay_event = None;
        let mut replay_recording = None;

        let envelope_id = envelope.event_id();
        // Process each item in the envelope
        for (index, (header, item, payload)) in envelope.items_with_payloads().enumerate() {
            match item {
                EnvelopeItem::Event(event) => {
                    // Extract event ID first
                    let event_id = event
                        .value()
                        .and_then(|e| e.id.value().map(|id| id.to_string()))
                        .ok_or_else(|| ProviderError::Validation("Event missing ID".to_string()))?;
                    if event_id.bytes().all(|byte| byte == b'0') {
                        return Err(ProviderError::Validation(
                            "Event ID must be nonzero hexadecimal".into(),
                        ));
                    }

                    // Keep the submitted JSON, including nulls and unknown contexts.
                    // Relay's normalized representation is used for validation, not
                    // as a lossy replacement for the native source payload.
                    let mut raw_event: Value = serde_json::from_slice(payload).map_err(|_| {
                        ProviderError::Validation("Invalid Sentry event JSON".into())
                    })?;
                    let fields = raw_event.as_object_mut().ok_or_else(|| {
                        ProviderError::Validation("Sentry event must be an object".into())
                    })?;
                    fields.insert("event_id".into(), Value::String(event_id.clone()));
                    mapper::validate_event_metadata(&raw_event)
                        .map_err(|e| ProviderError::Validation(e.to_string()))?;

                    // Convert to internal format
                    let error_data = mapper::convert_sentry_event_to_error_data(
                        event,
                        raw_event.clone(),
                        auth.project_id,
                        auth.environment_id,
                        auth.deployment_id,
                    )
                    .map_err(|e| ProviderError::Parsing(e.to_string()))?;

                    parsed_events.push(ParsedErrorEvent {
                        event_id,
                        raw_event,
                        error_data,
                    });
                }
                EnvelopeItem::Unsupported(payload)
                    if matches!(
                        header.ty,
                        crate::sentry::envelope::ItemType::Security
                            | crate::sentry::envelope::ItemType::RawSecurity
                            | crate::sentry::envelope::ItemType::Nel
                    ) =>
                {
                    if header.item_count.is_some_and(|v| v != 1)
                        || header.content_type.as_deref().is_some_and(|v| {
                            !matches!(
                                v,
                                "application/json"
                                    | "application/csp-report"
                                    | "application/reports+json"
                            )
                        })
                    {
                        return Err(ProviderError::Validation(
                            "Invalid browser report item headers".into(),
                        ));
                    }
                    if header.ty == crate::sentry::envelope::ItemType::Nel {
                        match crate::sentry::security::nel(header, payload, auth)
                            .map_err(|reason| ProviderError::Validation(reason.into()))?
                        {
                            Some(log) => {
                                if logs.len() >= 1000 {
                                    return Err(ProviderError::Validation(
                                        "At most 1000 logs per envelope".into(),
                                    ));
                                }
                                logs.push(log);
                            }
                            None => {
                                *unsupported.entry("nel").or_insert(0) += 1;
                            }
                        }
                    } else {
                        match crate::sentry::security::csp(
                            header,
                            payload,
                            auth,
                            envelope.event_id(),
                            index,
                        )
                        .map_err(|reason| ProviderError::Validation(reason.into()))?
                        {
                            Some(event) => {
                                if parsed_events.len() >= 100 {
                                    return Err(ProviderError::Validation(
                                        "At most 100 browser reports per envelope".into(),
                                    ));
                                }
                                parsed_events.push(event);
                            }
                            None => {
                                *unsupported.entry(header.ty.name()).or_insert(0) += 1;
                            }
                        }
                    }
                }
                EnvelopeItem::Unsupported(payload)
                    if header.ty == crate::sentry::envelope::ItemType::ClientReport =>
                {
                    if header
                        .content_type
                        .as_deref()
                        .is_some_and(|v| v != "application/json")
                        || header.item_count.is_some_and(|v| v != 1)
                    {
                        return Err(ProviderError::Validation(
                            "Invalid client report headers".into(),
                        ));
                    }
                    let report = crate::sentry::client_reports::decode_report(
                        payload,
                        envelope.event_id().as_deref(),
                        index,
                    )
                    .map_err(|reason| ProviderError::Validation(reason.into()))?;
                    client_outcome_entries += report.entries.len();
                    if client_outcome_entries > 1000 {
                        return Err(ProviderError::Validation(
                            "At most 1000 client outcomes per envelope".into(),
                        ));
                    }
                    if !report.entries.is_empty() {
                        client_outcomes.push(report);
                    }
                }
                EnvelopeItem::Unsupported(payload)
                    if matches!(
                        header.ty,
                        crate::sentry::envelope::ItemType::UserReport
                            | crate::sentry::envelope::ItemType::UserReportV2
                    ) =>
                {
                    feedback_bytes += payload.len();
                    if feedback.len() >= crate::services::feedback_service::MAX_FEEDBACK_BATCH
                        || feedback_bytes > 1024 * 1024
                        || header
                            .content_type
                            .as_deref()
                            .is_some_and(|v| v != "application/json")
                        || header.item_count.is_some_and(|v| v != 1)
                    {
                        return Err(ProviderError::Validation(
                            "Invalid feedback headers or count".into(),
                        ));
                    }
                    feedback.push(
                        crate::sentry::feedback::decode(
                            payload,
                            header.ty == crate::sentry::envelope::ItemType::UserReport,
                            envelope.event_id(),
                        )
                        .map_err(|_| ProviderError::Validation("Invalid feedback".into()))?,
                    );
                }
                EnvelopeItem::Log(payload) => {
                    let mut records = crate::sentry::logs::decode_logs(header, payload, auth)
                        .map_err(ProviderError::LogBatch)?;
                    if let Some(request) = request {
                        request
                            .enrich(payload, records.iter_mut().map(|r| &mut r.attributes))
                            .map_err(ProviderError::LogBatch)?;
                    }
                    for (record_index, record) in records.iter_mut().enumerate() {
                        record.ingest_identity = crate::sentry::identities::item(
                            auth,
                            envelope_id.as_deref(),
                            index,
                            payload,
                            record_index,
                        );
                        for key in [
                            "user.ip_address",
                            "browser.name",
                            "browser.version",
                            "os.name",
                            "os.version",
                        ] {
                            if record.attributes.contains_key(key) {
                                record
                                    .attribute_types
                                    .entry(key.into())
                                    .or_insert(temps_otel::types::LogAttributeType::String);
                            }
                        }
                    }
                    if logs.len() + records.len() > crate::sentry::logs::MAX_LOGS {
                        return Err(ProviderError::Validation(
                            "At most 1000 logs per envelope".into(),
                        ));
                    }
                    logs.extend(records);
                }
                EnvelopeItem::Unsupported(payload)
                    if matches!(
                        header.ty,
                        crate::sentry::envelope::ItemType::Transaction
                            | crate::sentry::envelope::ItemType::Span
                    ) =>
                {
                    let streamed = header.ty == crate::sentry::envelope::ItemType::Span
                        && header.content_type.as_deref()
                            == Some("application/vnd.sentry.items.span.v2+json");
                    if !streamed
                        && (header
                            .content_type
                            .as_deref()
                            .is_some_and(|v| v != "application/json")
                            || header.item_count.is_some_and(|n| n != 1))
                    {
                        return Err(ProviderError::Validation(
                            "Unsupported trace content type or count".into(),
                        ));
                    }
                    let mut records = if streamed {
                        crate::sentry::traces::streamed(header, payload, auth)
                    } else if header.ty == crate::sentry::envelope::ItemType::Transaction {
                        crate::sentry::traces::transaction(payload, auth)
                    } else {
                        crate::sentry::traces::standalone(payload, auth).map(|s| vec![s])
                    }
                    .map_err(|_| ProviderError::Validation("Invalid Sentry trace".into()))?;
                    if streamed {
                        if let Some(request) = request {
                            request
                                .enrich(payload, records.iter_mut().map(|r| &mut r.attributes))
                                .map_err(ProviderError::LogBatch)?;
                        }
                    }
                    if spans.len() + records.len() > 1000 {
                        return Err(ProviderError::Validation(
                            "At most 1000 spans per envelope".into(),
                        ));
                    }
                    for record in &mut records {
                        record.ingest_identity = Some(crate::sentry::identities::span(
                            auth,
                            &record.trace_id,
                            &record.span_id,
                        ));
                    }
                    spans.extend(records);
                }
                EnvelopeItem::Unsupported(payload)
                    if header.ty == crate::sentry::envelope::ItemType::Statsd =>
                {
                    let mut batch = crate::sentry::statsd::decode(payload, auth).map_err(|_| {
                        ProviderError::Validation("Invalid legacy Sentry metrics".into())
                    })?;
                    statsd_bytes = statsd_bytes.saturating_add(batch.retained_bytes);
                    if statsd_bytes > 4 * 1024 * 1024 {
                        return Err(ProviderError::Validation(
                            "Legacy Sentry metrics exceed the 4 MiB expansion limit".into(),
                        ));
                    }
                    if metrics.len() + batch.points.len() > crate::sentry::logs::MAX_LOGS {
                        return Err(ProviderError::Validation(
                            "At most 1000 metrics per envelope".into(),
                        ));
                    }
                    if batch.unsupported_sets > 0 {
                        *unsupported.entry("statsd_set").or_insert(0) += batch.unsupported_sets;
                    }
                    for (record_index, record) in batch.points.iter_mut().enumerate() {
                        record.ingest_identity = crate::sentry::identities::item(
                            auth,
                            envelope_id.as_deref(),
                            index,
                            payload,
                            record_index,
                        );
                    }
                    metrics.extend(batch.points);
                }
                EnvelopeItem::Unsupported(payload)
                    if header.ty == crate::sentry::envelope::ItemType::TraceMetric =>
                {
                    let mut records = crate::sentry::metrics::decode(header, payload, auth)
                        .map_err(|_| ProviderError::Validation("Invalid Sentry metrics".into()))?;
                    if let Some(request) = request {
                        request
                            .enrich(payload, records.iter_mut().map(|r| &mut r.attributes))
                            .map_err(ProviderError::LogBatch)?;
                    }
                    if metrics.len() + records.len() > crate::sentry::logs::MAX_LOGS {
                        return Err(ProviderError::Validation(
                            "At most 1000 metrics per envelope".into(),
                        ));
                    }
                    for (record_index, record) in records.iter_mut().enumerate() {
                        record.ingest_identity = crate::sentry::identities::item(
                            auth,
                            envelope_id.as_deref(),
                            index,
                            payload,
                            record_index,
                        );
                    }
                    metrics.extend(records);
                }
                EnvelopeItem::Unsupported(payload)
                    if header.ty == crate::sentry::envelope::ItemType::ReplayEvent =>
                {
                    if replay_event.replace(*payload).is_some() {
                        return Err(ProviderError::Validation(
                            "Only one replay segment per envelope".into(),
                        ));
                    }
                }
                EnvelopeItem::Unsupported(payload)
                    if header.ty == crate::sentry::envelope::ItemType::ReplayRecording =>
                {
                    if replay_recording.replace(*payload).is_some() {
                        return Err(ProviderError::Validation(
                            "Only one replay recording per envelope".into(),
                        ));
                    }
                }
                EnvelopeItem::Unsupported(payload)
                    if matches!(header.ty, crate::sentry::envelope::ItemType::CheckIn) =>
                {
                    if header
                        .content_type
                        .as_deref()
                        .is_some_and(|v| v != "application/json")
                        || header.item_count.is_some_and(|count| count != 1)
                    {
                        return Err(ProviderError::Validation(
                            "Unsupported check-in content type or count".into(),
                        ));
                    }
                    if check_ins.len() >= 100 {
                        return Err(ProviderError::Validation(
                            "At most 100 check-ins per envelope".into(),
                        ));
                    }
                    check_ins.push(
                        crate::sentry::check_ins::decode(payload)
                            .map_err(|e| ProviderError::Validation(e.to_string()))?,
                    );
                }
                _ => {
                    *unsupported.entry(header.ty.name()).or_insert(0) += 1;
                }
            }
        }

        let replay = match (replay_event, replay_recording) {
            (None, None) => None,
            // Native/video replay is not rrweb. Keep the paired metadata
            // unsupported too, without rejecting other valid envelope items.
            (Some(_), None) if unsupported.contains_key("replay_video") => {
                *unsupported.entry("replay_event").or_insert(0) += 1;
                None
            }
            (Some(event), Some(recording)) => Some(
                crate::sentry::replay::decode(event, recording, auth).map_err(|_| {
                    ProviderError::Validation("Invalid browser replay segment".into())
                })?,
            ),
            _ => {
                return Err(ProviderError::Validation(
                    "Replay metadata and recording must be paired".into(),
                ))
            }
        };
        Ok(ParsedSentryEnvelope {
            spans,
            metrics,
            replay,
            events: parsed_events,
            logs,
            feedback,
            check_ins,
            client_outcomes,
            unsupported,
        })
    }
}

#[async_trait]
impl ErrorProvider for SentryProvider {
    fn name(&self) -> &'static str {
        "sentry"
    }

    async fn authenticate(
        &self,
        project_id: i32,
        credentials: &str,
    ) -> Result<AuthContext, ProviderError> {
        let dsn = self
            .dsn_service
            .validate_dsn(project_id, credentials)
            .await
            .map_err(|e| ProviderError::Authentication(e.to_string()))?;

        Ok(AuthContext {
            project_id: dsn.project_id,
            environment_id: dsn.environment_id,
            deployment_id: dsn.deployment_id,
            rate_limit_per_minute: dsn.rate_limit_per_minute,
        })
    }

    async fn parse_events(
        &self,
        payload: &[u8],
        auth: &AuthContext,
    ) -> Result<Vec<ParsedErrorEvent>, ProviderError> {
        Ok(self.parse_envelope(payload, auth)?.events)
    }

    async fn parse_json_event(
        &self,
        mut event_json: Value,
        auth: &AuthContext,
    ) -> Result<ParsedErrorEvent, ProviderError> {
        mapper::validate_event_metadata(&event_json)
            .map_err(|e| ProviderError::Validation(e.to_string()))?;
        // Parse with Relay types
        let event = relay_event_schema::protocol::Event::from_value(event_json.clone().into());

        // Extract event ID
        let event_id = event
            .value()
            .and_then(|e| e.id.value().map(|id| id.to_string()))
            .ok_or_else(|| ProviderError::Validation("Event missing ID".to_string()))?;
        if event_id.bytes().all(|byte| byte == b'0') {
            return Err(ProviderError::Validation(
                "Event ID must be nonzero hexadecimal".into(),
            ));
        }

        if let Some(fields) = event_json.as_object_mut() {
            fields.insert("event_id".into(), Value::String(event_id.clone()));
        }
        // Convert to internal format
        let error_data = mapper::convert_sentry_event_to_error_data(
            &event,
            event_json.clone(),
            auth.project_id,
            auth.environment_id,
            auth.deployment_id,
        )
        .map_err(|e| ProviderError::Parsing(e.to_string()))?;

        Ok(ParsedErrorEvent {
            event_id,
            raw_event: event_json,
            error_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn framed(items: Vec<(&str, serde_json::Value)>) -> Vec<u8> {
        let mut bytes = b"{}\n".to_vec();
        for (kind, payload) in items {
            let payload_bytes = serde_json::to_vec(&payload).unwrap();
            let mime = if kind == "log" {
                "application/vnd.sentry.items.log+json"
            } else {
                "application/vnd.sentry.items.trace-metric+json"
            };
            let header = serde_json::json!({"type":kind,"length":payload_bytes.len(),"item_count":payload["items"].as_array().map(|v|v.len()),"content_type":mime});
            bytes.extend(serde_json::to_vec(&header).unwrap());
            bytes.push(b'\n');
            bytes.extend(payload_bytes);
            bytes.push(b'\n');
        }
        bytes
    }
    #[test]
    fn client_outcomes_and_browser_reports_mix_with_existing_errors() {
        let db = sea_orm::DatabaseConnection::Disconnected;
        let provider = SentryProvider::new(Arc::new(DSNService::new(Arc::new(db))));
        let auth = AuthContext {
            project_id: 7,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        let mut bytes = b"{}\n".to_vec();
        for (kind, payload) in [
            (
                "event",
                serde_json::json!({"event_id":"00112233445546778899aabbccddeeff","message":"Synthetic mixed error"}),
            ),
            (
                "client_report",
                serde_json::json!({"discarded_events":[{"reason":"before_send","category":"error","quantity":3}]}),
            ),
            (
                "raw_security",
                serde_json::json!({"csp-report":{"effective-directive":"script-src","violated-directive":"script-src","blocked-uri":"inline"}}),
            ),
            (
                "nel",
                serde_json::json!({"type":"network-error","url":"https://synthetic.example","body":{"type":"dns.name_not_resolved"}}),
            ),
        ] {
            let payload = serde_json::to_vec(&payload).unwrap();
            bytes.extend(serde_json::to_vec(&serde_json::json!({"type":kind,"length":payload.len(),"content_type":"application/json"})).unwrap());
            bytes.push(b'\n');
            bytes.extend(payload);
            bytes.push(b'\n');
        }
        let parsed = provider.parse_envelope(&bytes, &auth).unwrap();
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.logs.len(), 1);
        assert_eq!(parsed.client_outcomes.len(), 1);
        assert_eq!(parsed.client_outcomes[0].entries[0].quantity, 3);
        assert!(parsed.unsupported.is_empty());
        bytes.extend_from_slice(b"{\"type\":\"client_report\"}\n{\"discarded_events\":[{\"reason\":\"before_send\",\"category\":\"error\",\"quantity\":-1}]}\n");
        assert!(
            provider.parse_envelope(&bytes, &auth).is_err(),
            "every supported item must validate before any writes"
        );
    }

    #[test]
    fn repeated_batches_combine_with_total_limits_and_original_events_survive() {
        use serde_json::json;
        let provider = SentryProvider::new(Arc::new(DSNService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ))));
        let auth = AuthContext {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        let log = json!({"timestamp":1790000000,"level":"info","body":"Synthetic log","trace_id":"11111111111111111111111111111111"});
        let metric = json!({"timestamp":1790000000,"name":"synthetic.metric","type":"gauge","value":7,"trace_id":"11111111111111111111111111111111"});
        let bytes = framed(vec![
            ("log", json!({"items":[log.clone()]})),
            ("trace_metric", json!({"items":[metric.clone()]})),
            ("log", json!({"items":[log.clone()]})),
            ("trace_metric", json!({"items":[metric.clone()]})),
        ]);
        let batch = provider.parse_envelope(&bytes, &auth).unwrap();
        assert_eq!((batch.logs.len(), batch.metrics.len()), (2, 2));
        let bytes = framed(vec![
            ("log", json!({"items":vec![log.clone();500]})),
            ("log", json!({"items":vec![log.clone();500]})),
        ]);
        assert_eq!(
            provider.parse_envelope(&bytes, &auth).unwrap().logs.len(),
            1000
        );
        let bytes = framed(vec![
            ("log", json!({"items":vec![log.clone();500]})),
            ("log", json!({"items":vec![log;501]})),
        ]);
        assert!(provider.parse_envelope(&bytes, &auth).is_err());
        let bytes = framed(vec![
            ("trace_metric", json!({"items":vec![metric.clone();500]})),
            ("trace_metric", json!({"items":vec![metric.clone();500]})),
        ]);
        assert_eq!(
            provider
                .parse_envelope(&bytes, &auth)
                .unwrap()
                .metrics
                .len(),
            1000
        );
        let bytes = framed(vec![
            ("trace_metric", json!({"items":vec![metric.clone();500]})),
            ("trace_metric", json!({"items":vec![metric;501]})),
        ]);
        assert!(provider.parse_envelope(&bytes, &auth).is_err());
        let nil = framed(vec![(
            "event",
            json!({"event_id":"0".repeat(32),"message":"Synthetic nil ID"}),
        )]);
        assert!(provider.parse_envelope(&nil, &auth).is_err());
        let bytes=b"{}\n{\"type\":\"event\"}\n{\"event_id\":\"11111111111111111111111111111111\",\"message\":\"Synthetic message\",\"extra\":{\"nullable\":null,\"values\":[1,null,2]},\"contexts\":{\"custom\":{\"nullable\":null}}}\n";
        let parsed = provider.parse_envelope(bytes, &auth).unwrap();
        assert_eq!(
            parsed.events[0].raw_event["extra"]["values"],
            json!([1, null, 2])
        );
        assert!(parsed.events[0].raw_event["contexts"]["custom"]
            .get("nullable")
            .is_some());
    }
    #[test]
    fn feedback_count_and_total_bytes_are_bounded_before_any_writes() {
        let provider = SentryProvider::new(Arc::new(DSNService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ))));
        let auth = AuthContext {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        let build = |count: usize, size: usize| {
            let mut bytes = b"{}\n".to_vec();
            for index in 0..count {
                let payload=serde_json::to_vec(&serde_json::json!({"event_id":format!("{:032x}",index+1),"contexts":{"feedback":{"message":"x".repeat(size)}}})).unwrap();
                let header = serde_json::json!({"type":"feedback","length":payload.len(),"content_type":"application/json"});
                bytes.extend(serde_json::to_vec(&header).unwrap());
                bytes.push(b'\n');
                bytes.extend(payload);
                bytes.push(b'\n');
            }
            bytes
        };
        assert_eq!(
            provider
                .parse_envelope(&build(100, 1024), &auth)
                .unwrap()
                .feedback
                .len(),
            100
        );
        assert!(provider.parse_envelope(&build(101, 1024), &auth).is_err());
        assert!(provider
            .parse_envelope(&build(100, 12 * 1024), &auth)
            .is_err());
    }

    #[tokio::test]
    async fn unsupported_mobile_video_does_not_block_native_telemetry() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let provider = SentryProvider::new(Arc::new(DSNService::new(db)));
        let auth = AuthContext {
            project_id: 41,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        let mut bytes=b"{}\n{\"type\":\"replay_event\"}\n{\"platform\":\"cocoa\"}\n{\"type\":\"replay_video\",\"length\":3}\n".to_vec();
        bytes.extend_from_slice(b"\0\xff\n\n");
        bytes.extend_from_slice(br#"{"type":"log","item_count":1,"content_type":"application/vnd.sentry.items.log+json"}
{"items":[{"timestamp":1790000000,"level":"info","body":"Synthetic mixed log","trace_id":"11111111111111111111111111111111"}]}
"#);
        let parsed = provider.parse_envelope(&bytes, &auth).unwrap();
        assert_eq!(parsed.logs.len(), 1);
        assert!(parsed.replay.is_none());
        assert_eq!(parsed.unsupported.get("replay_video"), Some(&1));
        assert_eq!(parsed.unsupported.get("replay_event"), Some(&1));
    }
    #[test]
    fn legacy_statsd_mixes_with_modern_metrics_without_storing_sets_and_has_shared_limits() {
        let provider = SentryProvider::new(Arc::new(DSNService::new(Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        ))));
        let auth = AuthContext {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        let modern = serde_json::json!({"timestamp":1790000000,"name":"synthetic.metric","type":"gauge","value":7,"trace_id":"11111111111111111111111111111111"});
        let mut bytes = framed(vec![(
            "trace_metric",
            serde_json::json!({"items":[modern]}),
        )]);
        let payload =
            b"synthetic.counter@none:1|c|T1790000000\nsynthetic.set@none:1|s|T1790000000\n";
        for _ in 0..2 {
            bytes.extend(
                serde_json::to_vec(&serde_json::json!({"type":"statsd","length":payload.len()}))
                    .unwrap(),
            );
            bytes.push(b'\n');
            bytes.extend(payload);
            bytes.push(b'\n');
        }
        let parsed = provider.parse_envelope(&bytes, &auth).unwrap();
        assert_eq!(parsed.metrics.len(), 3);
        assert_eq!(parsed.unsupported.get("statsd_set"), Some(&2));
        // Every individual item fits; their expanded labels exceed the shared
        // envelope budget. Admission rejects the entire envelope before writes.
        let payload = format!(
            "synthetic.distribution@none:{}|d|#padding:{}|T1790000000\n",
            vec!["1"; 200].join(":"),
            "x".repeat(12_000)
        );
        let mut bytes = b"{}\n".to_vec();
        for _ in 0..2 {
            bytes.extend(
                serde_json::to_vec(&serde_json::json!({"type":"statsd","length":payload.len()}))
                    .unwrap(),
            );
            bytes.push(b'\n');
            bytes.extend(payload.as_bytes());
            bytes.push(b'\n');
        }
        assert!(provider.parse_envelope(&bytes, &auth).is_err());
    }
}
