// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Structured JSONL logging for the guest agent.
//!
//! The agent runs as PID 1 on a serial console with no `tracing` stack, so it
//! writes one JSON object per line straight to stdout. Every record carries a
//! timestamp, a level, a stable machine-greppable `event` name and whatever
//! typed fields the call site attaches (ports, pids, paths, errors) —
//! never a pre-formatted sentence to scrape.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Component name stamped on every record.
const COMPONENT: &str = "temps-vm-agent";

/// Severity of a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// Render one record as a single JSON line (no trailing newline).
///
/// Call-site `fields` are written first and the reserved keys (`ts_unix_ms`,
/// `level`, `component`, `event`) last, so a field can never shadow them.
pub fn render(ts_unix_ms: u128, level: Level, event: &str, fields: &[(&str, Value)]) -> String {
    let mut record = Map::with_capacity(fields.len() + 4);
    for (key, value) in fields {
        record.insert((*key).to_string(), value.clone());
    }
    // `u128` has no JSON number form in serde_json without a feature flag;
    // epoch milliseconds fit `u64` for the next several hundred million years.
    record.insert(
        "ts_unix_ms".to_string(),
        Value::from(u64::try_from(ts_unix_ms).unwrap_or(u64::MAX)),
    );
    record.insert("level".to_string(), Value::from(level.as_str()));
    record.insert("component".to_string(), Value::from(COMPONENT));
    record.insert("event".to_string(), Value::from(event));
    Value::Object(record).to_string()
}

/// Write one structured record to the serial console. A failed write is
/// ignorable: the console is the only sink there is.
pub fn emit(level: Level, event: &str, fields: &[(&str, Value)]) {
    // A guest clock that was never set reads as the epoch; `unwrap_or_default`
    // keeps logging working rather than panicking PID 1.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let line = render(now, level, event, fields);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn record_is_one_json_object_with_typed_fields() {
        let line = render(
            1_700_000_000_123,
            Level::Warn,
            "pty_agent_exited",
            &[("pid", json!(42)), ("elapsed_ms", json!(1500))],
        );
        assert!(!line.contains('\n'), "a record must be a single line");
        let parsed: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["level"], "warn");
        assert_eq!(parsed["component"], "temps-vm-agent");
        assert_eq!(parsed["event"], "pty_agent_exited");
        assert_eq!(parsed["ts_unix_ms"], 1_700_000_000_123u64);
        assert_eq!(parsed["pid"], 42);
        assert_eq!(parsed["elapsed_ms"], 1500);
    }

    #[test]
    fn call_site_fields_cannot_shadow_reserved_keys() {
        let line = render(
            7,
            Level::Info,
            "ready",
            &[("event", json!("spoofed")), ("level", json!("fatal"))],
        );
        let parsed: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["event"], "ready");
        assert_eq!(parsed["level"], "info");
    }

    #[test]
    fn newlines_in_field_values_stay_inside_the_json_string() {
        let line = render(
            1,
            Level::Error,
            "mount_failed",
            &[("error", json!("first\nsecond"))],
        );
        assert!(!line.contains('\n'));
        let parsed: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["error"], "first\nsecond");
    }

    #[test]
    fn out_of_range_timestamp_saturates_instead_of_panicking() {
        let line = render(u128::MAX, Level::Info, "ready", &[]);
        let parsed: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["ts_unix_ms"], u64::MAX);
    }
}
