// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sentry check-in wire adapter; scheduling and run state belong to Temps.
use serde::Deserialize;
use temps_monitoring::cron_monitors::{CronMonitorError, MonitorCheckIn, MonitorConfig};
#[derive(Deserialize)]
struct CheckIn {
    check_in_id: String,
    monitor_slug: String,
    status: String,
    duration: Option<f64>,
    monitor_config: Option<MonitorConfig>,
    release: Option<String>,
    environment: Option<String>,
    contexts: Option<Contexts>,
}
#[derive(Deserialize)]
struct Contexts {
    trace: Option<Trace>,
}
#[derive(Deserialize)]
struct Trace {
    trace_id: Option<String>,
    span_id: Option<String>,
}
pub fn decode(payload: &[u8]) -> Result<MonitorCheckIn, CronMonitorError> {
    if payload.len() > 16384 {
        return Err(CronMonitorError::Invalid {
            reason: "check-in exceeds 16 KiB".into(),
        });
    }
    let wire: CheckIn = serde_json::from_slice(payload).map_err(|_| CronMonitorError::Invalid {
        reason: "invalid check-in JSON".into(),
    })?;
    if wire.check_in_id.len() != 32
        || !wire.check_in_id.bytes().all(|c| c.is_ascii_hexdigit())
        || wire.check_in_id.bytes().all(|c| c == b'0')
    {
        return Err(CronMonitorError::Invalid {
            reason: "invalid Sentry check-in ID".into(),
        });
    }
    let trace = wire.contexts.and_then(|c| c.trace);
    let item = MonitorCheckIn {
        id: wire.check_in_id.to_ascii_lowercase(),
        slug: wire.monitor_slug,
        status: wire.status,
        duration: wire.duration,
        config: wire.monitor_config,
        release: wire.release,
        environment: wire.environment,
        trace_id: trace.as_ref().and_then(|t| t.trace_id.clone()),
        span_id: trace.and_then(|t| t.span_id),
    };
    item.validate(chrono::Utc::now())?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_official_check_in_shape_and_bounded_fields() {
        let good = serde_json::json!({"check_in_id":"a".repeat(32),"monitor_slug":"synthetic-job","status":"in_progress","monitor_config":{"schedule":{"type":"interval","value":1,"unit":"hour"}},"contexts":{"trace":{"trace_id":"b".repeat(32),"span_id":"c".repeat(16)}}});
        let record = decode(&serde_json::to_vec(&good).unwrap()).unwrap();
        assert_eq!(
            record.trace_id.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        for (field, value) in [
            ("check_in_id", serde_json::json!("0".repeat(32))),
            ("monitor_slug", serde_json::json!("x".repeat(129))),
            ("status", serde_json::json!("timeout")),
            ("duration", serde_json::json!(-1)),
            ("release", serde_json::json!("nul\u{0000}")),
        ] {
            let mut invalid = good.clone();
            invalid[field] = value;
            assert!(
                decode(&serde_json::to_vec(&invalid).unwrap()).is_err(),
                "{field}"
            );
        }
        let mut future = good.clone();
        future["monitor_config"]["future"] = true.into();
        assert!(decode(&serde_json::to_vec(&future).unwrap()).is_err());
        assert!(decode(&vec![b' '; 16385]).is_err());
        assert!(
            decode(br#"{"items":[]}"#).is_err(),
            "check-ins are individual items, not telemetry batches"
        );
    }
}
