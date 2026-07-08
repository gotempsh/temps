//! Structured log line parser
//!
//! Attempts JSON parse on every incoming log line body. If valid JSON, extracts
//! known fields into top-level columns. If not, attempts to detect log level
//! from common text prefixes. Falls back to INFO for unparsable lines.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::types::{ContainerContext, LogLevel, LogLine, LogStream};

/// Known JSON field names that map to the `level` column
const LEVEL_KEYS: &[&str] = &["level", "severity", "log_level", "loglevel", "lvl"];

/// Known JSON field names that map to the `msg` column
const MESSAGE_KEYS: &[&str] = &["msg", "message", "text", "body", "log"];

/// JSON field names that are promoted to top-level LogLine columns and should be
/// removed from the `fields` JSONB to avoid duplication. Only includes keys whose
/// values are represented in `LogLine.level`, `LogLine.msg`, or `LogLine.ts`.
/// Metadata fields (status, duration_ms, request_id, etc.) are intentionally kept
/// in `fields` so they remain searchable and filterable.
const EXTRACTED_KEYS: &[&str] = &[
    // Level keys -> LogLine.level
    "level",
    "severity",
    "log_level",
    "loglevel",
    "lvl",
    // Message keys -> LogLine.msg
    "msg",
    "message",
    "text",
    "body",
    "log",
    // Timestamp keys -> LogLine.ts (timestamp comes from Docker, these are redundant)
    "time",
    "timestamp",
    "ts",
    "t",
    // Original "duration" is replaced by computed "duration_ms" in extract_duration()
    "duration",
];

/// Parse a raw log line from a Docker container into a structured `LogLine`.
///
/// Strategy:
/// 1. Try JSON parse; extract known fields, store remaining in `fields` JSONB
/// 2. If not JSON, detect log level from text prefixes
/// 3. Fall back to INFO if no level detected
/// 4. Attempt to parse duration values into numeric `duration_ms`
pub fn parse_log_line(
    raw: &str,
    timestamp: DateTime<Utc>,
    stream: LogStream,
    ctx: &ContainerContext,
) -> LogLine {
    let trimmed = raw.trim();

    // Try JSON parse first
    if let Ok(Value::Object(mut map)) = serde_json::from_str(trimmed) {
        let level = extract_level_from_json(&map).unwrap_or(LogLevel::Info);
        let msg = extract_message_from_json(&map).unwrap_or_else(|| trimmed.to_string());

        // Extract duration if present
        extract_duration(&mut map);

        // Remove extracted keys from fields
        for key in EXTRACTED_KEYS {
            map.remove(*key);
        }

        let fields = if map.is_empty() {
            None
        } else {
            Some(Value::Object(map))
        };

        return LogLine {
            ts: timestamp,
            stream,
            level,
            msg,
            fields,
            container_id: ctx.container_id.clone(),
            service: ctx.service.clone(),
            env: ctx.env.clone(),
            project_id: ctx.project_id,
            external_service_id: ctx.external_service_id,
            deploy_id: ctx.deploy_id,
            // node fields are stamped by the collector after parsing — the
            // parser only knows Docker-label-derived context, not platform
            // node placement. Local containers stay None.
            node_id: None,
            node_name: None,
        };
    }

    // Not JSON — detect level from text
    let (level, msg) = detect_level_from_text(trimmed);

    LogLine {
        ts: timestamp,
        stream,
        level,
        msg,
        fields: None,
        container_id: ctx.container_id.clone(),
        service: ctx.service.clone(),
        env: ctx.env.clone(),
        project_id: ctx.project_id,
        external_service_id: ctx.external_service_id,
        deploy_id: ctx.deploy_id,
        node_id: None,
        node_name: None,
    }
}

/// Extract log level from a JSON object by checking known level keys.
fn extract_level_from_json(map: &serde_json::Map<String, Value>) -> Option<LogLevel> {
    for key in LEVEL_KEYS {
        if let Some(val) = map.get(*key) {
            let level_str = match val {
                Value::String(s) => s.clone(),
                Value::Number(n) => {
                    // Numeric levels (syslog-style): 0-2 = error, 3-4 = warn, 5-6 = info, 7 = debug
                    if let Some(n) = n.as_u64() {
                        return match n {
                            0..=2 => Some(LogLevel::Error),
                            3..=4 => Some(LogLevel::Warn),
                            5..=6 => Some(LogLevel::Info),
                            7 => Some(LogLevel::Debug),
                            _ => Some(LogLevel::Trace),
                        };
                    }
                    continue;
                }
                _ => continue,
            };
            if let Some(level) = LogLevel::parse(&level_str) {
                return Some(level);
            }
        }
    }
    None
}

/// Extract message from a JSON object by checking known message keys.
fn extract_message_from_json(map: &serde_json::Map<String, Value>) -> Option<String> {
    for key in MESSAGE_KEYS {
        if let Some(Value::String(s)) = map.get(*key) {
            return Some(s.clone());
        }
    }
    None
}

/// Extract and normalize duration values into `duration_ms` field.
fn extract_duration(map: &mut serde_json::Map<String, Value>) {
    // Check for explicit duration_ms
    if map.contains_key("duration_ms") {
        return;
    }

    // Check for "duration" field and try to parse it
    if let Some(val) = map.get("duration").cloned() {
        match val {
            Value::String(s) => {
                if let Some(ms) = parse_duration_to_ms(&s) {
                    map.insert(
                        "duration_ms".to_string(),
                        Value::Number(
                            serde_json::Number::from_f64(ms)
                                .unwrap_or_else(|| serde_json::Number::from(ms as u64)),
                        ),
                    );
                }
            }
            Value::Number(n) => {
                // Assume ms if just a number
                map.insert("duration_ms".to_string(), Value::Number(n));
            }
            _ => {}
        }
    }
}

/// Parse a duration string like "45ms", "1.2s", "200us" into milliseconds.
///
/// Multi-character suffixes must be checked before single-character suffixes
/// to avoid false matches (e.g., "us" before "s", "ms" before "s").
pub fn parse_duration_to_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        rest.trim().parse::<f64>().ok()
    } else if let Some(rest) = s.strip_suffix("µs") {
        rest.trim().parse::<f64>().ok().map(|v| v / 1000.0)
    } else if let Some(rest) = s.strip_suffix("us") {
        rest.trim().parse::<f64>().ok().map(|v| v / 1000.0)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.trim().parse::<f64>().ok().map(|v| v * 1000.0)
    } else {
        // Try plain number (assume ms)
        s.parse::<f64>().ok()
    }
}

/// Detect log level from common text prefixes in non-JSON log lines.
///
/// Patterns matched:
/// - `ERROR ...`, `WARN ...`, `INFO ...`, `DEBUG ...`, `TRACE ...`
/// - `[ERROR] ...`, `[WARN] ...`, `[error] ...`
/// - `level=error ...`, `level=warn ...`
/// - Timestamps followed by level: `2026-02-25T14:00:00Z ERROR ...`
fn detect_level_from_text(line: &str) -> (LogLevel, String) {
    let trimmed = line.trim();

    // Try bracket style: [ERROR], [WARN], etc.
    if let Some(rest) = try_bracket_level(trimmed) {
        return rest;
    }

    // Try key=value style: level=error
    if let Some(rest) = try_kv_level(trimmed) {
        return rest;
    }

    // Try prefix style: ERROR ..., WARN ...
    // Also handles timestamp prefix: skip ISO8601 timestamp then check level
    if let Some(rest) = try_prefix_level(trimmed) {
        return rest;
    }

    // No level detected — fall back to INFO
    (LogLevel::Info, trimmed.to_string())
}

/// Try to detect level from `[LEVEL]` or `[level]` prefix.
fn try_bracket_level(line: &str) -> Option<(LogLevel, String)> {
    if !line.starts_with('[') {
        return None;
    }
    let close = line.find(']')?;
    let inside = &line[1..close];
    let level = LogLevel::parse(inside.trim())?;
    let msg = line[close + 1..].trim().to_string();
    Some((level, msg))
}

/// Try to detect level from `level=error` style.
fn try_kv_level(line: &str) -> Option<(LogLevel, String)> {
    for pattern in &["level=", "LEVEL=", "lvl=", "LVL="] {
        if let Some(pos) = line.find(pattern) {
            let after = &line[pos + pattern.len()..];
            let level_str = after.split_whitespace().next()?;
            let level = LogLevel::parse(level_str)?;
            return Some((level, line.to_string()));
        }
    }
    None
}

/// Try to detect level from word prefix, possibly after a timestamp.
fn try_prefix_level(line: &str) -> Option<(LogLevel, String)> {
    // Split by whitespace and check each token for a level
    for (i, word) in line.split_whitespace().enumerate() {
        // Only check the first 5 tokens (to avoid false matches deep in message)
        if i > 4 {
            break;
        }
        // Strip common delimiters
        let clean = word.trim_matches(|c: char| c == '-' || c == '|' || c == ':');
        if let Some(level) = LogLevel::parse(clean) {
            return Some((level, line.to_string()));
        }
    }
    None
}

/// Parse a Docker log timestamp prefix.
///
/// Docker daemon logs come as: `2026-02-25T14:00:00.123456789Z message`
pub fn parse_docker_timestamp(line: &str) -> (DateTime<Utc>, &str) {
    // Docker timestamps are RFC 3339 with nanoseconds, always ASCII.
    // Format: `2026-02-25T14:00:00.123456789Z <message>`
    //
    // We search for the first space in the line. The timestamp is pure ASCII
    // so the space delimiter is always a single byte. We avoid fixed-offset
    // byte slicing because the message portion may contain multi-byte UTF-8
    // characters (e.g. `▲ Next.js ...`).
    if line.len() > 30 {
        if let Some(space_pos) = line.find(' ') {
            if space_pos <= 40 {
                if let Ok(ts) = DateTime::parse_from_rfc3339(line[..space_pos].trim()) {
                    return (ts.with_timezone(&Utc), &line[space_pos + 1..]);
                }
            }
        }
    }
    // Fallback: use current time
    (Utc::now(), line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ContainerContext {
        ContainerContext {
            project_id: 1,
            external_service_id: None,
            env: "1".to_string(),
            service: "web".to_string(),
            container_id: "abc123".to_string(),
            deploy_id: None,
        }
    }

    #[test]
    fn test_parse_json_log_with_level_and_message() {
        let ctx = test_ctx();
        let raw =
            r#"{"level":"error","msg":"database timeout","status":500,"request_id":"req-123"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Error);
        assert_eq!(line.msg, "database timeout");
        assert!(
            line.fields.is_none() || !line.fields.as_ref().unwrap().to_string().contains("level")
        );
    }

    #[test]
    fn test_parse_json_log_with_remaining_fields() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"request","method":"GET","path":"/api"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Info);
        assert_eq!(line.msg, "request");
        let fields = line.fields.unwrap();
        assert_eq!(fields["method"], "GET");
        assert_eq!(fields["path"], "/api");
    }

    #[test]
    fn test_parse_json_log_with_severity_key() {
        let ctx = test_ctx();
        let raw = r#"{"severity":"WARNING","message":"rate limit approaching"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Warn);
        assert_eq!(line.msg, "rate limit approaching");
    }

    #[test]
    fn test_parse_json_log_no_level_defaults_to_info() {
        let ctx = test_ctx();
        let raw = r#"{"msg":"just a message","key":"value"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Info);
        assert_eq!(line.msg, "just a message");
    }

    #[test]
    fn test_parse_json_duration_string() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"request completed","duration":"45ms"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        let fields = line.fields.unwrap();
        assert_eq!(fields["duration_ms"], 45.0);
    }

    #[test]
    fn test_parse_json_duration_seconds() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"slow query","duration":"1.5s"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        let fields = line.fields.unwrap();
        assert_eq!(fields["duration_ms"], 1500.0);
    }

    #[test]
    fn test_parse_plain_text_error() {
        let ctx = test_ctx();
        let raw = "ERROR connection refused to database";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stderr, &ctx);

        assert_eq!(line.level, LogLevel::Error);
        assert_eq!(line.stream, LogStream::Stderr);
    }

    #[test]
    fn test_parse_plain_text_bracket_level() {
        let ctx = test_ctx();
        let raw = "[WARN] disk usage at 85%";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Warn);
        assert_eq!(line.msg, "disk usage at 85%");
    }

    #[test]
    fn test_parse_plain_text_kv_level() {
        let ctx = test_ctx();
        let raw = "time=2026-02-25 level=error msg=timeout";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Error);
    }

    #[test]
    fn test_parse_plain_text_no_level_defaults_info() {
        let ctx = test_ctx();
        let raw = "Starting server on port 3000";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Info);
    }

    #[test]
    fn test_parse_plain_text_with_timestamp_prefix() {
        let ctx = test_ctx();
        let raw = "2026-02-25T14:00:00Z ERROR database connection lost";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);

        assert_eq!(line.level, LogLevel::Error);
    }

    #[test]
    fn test_parse_duration_to_ms() {
        assert_eq!(parse_duration_to_ms("45ms"), Some(45.0));
        assert_eq!(parse_duration_to_ms("1.5s"), Some(1500.0));
        assert_eq!(parse_duration_to_ms("200us"), Some(0.2));
        assert_eq!(parse_duration_to_ms("200µs"), Some(0.2));
        assert_eq!(parse_duration_to_ms("123"), Some(123.0));
        assert_eq!(parse_duration_to_ms("not-a-duration"), None);
    }

    #[test]
    fn test_parse_docker_timestamp() {
        let line = "2026-02-25T14:30:00.123456789Z Hello world";
        let (ts, msg) = parse_docker_timestamp(line);
        assert_eq!(msg, "Hello world");
        assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-02-25");
    }

    #[test]
    fn test_parse_docker_timestamp_fallback() {
        let line = "not a timestamp line";
        let (_ts, msg) = parse_docker_timestamp(line);
        assert_eq!(msg, "not a timestamp line");
    }

    #[test]
    fn test_parse_docker_timestamp_with_multibyte_utf8() {
        // Regression: the `▲` character is 3 bytes (U+25B2). A naive byte slice
        // at a fixed offset (e.g. 35) can land inside this character and panic.
        let line = "2026-02-26T21:44:10.430590805Z    ▲ Next.js 15.2.8\n";
        let (ts, msg) = parse_docker_timestamp(line);
        assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-02-26");
        assert!(msg.contains("Next.js"));
        assert!(msg.contains("▲"));
    }

    #[test]
    fn test_parse_docker_timestamp_with_emoji() {
        let line = "2026-02-26T12:00:00.000000000Z 🚀 Server started";
        let (ts, msg) = parse_docker_timestamp(line);
        assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-02-26");
        assert!(msg.contains("🚀"));
    }

    #[test]
    fn test_context_fields_applied() {
        let ctx = ContainerContext {
            project_id: 7,
            external_service_id: None,
            env: "2".to_string(),
            service: "api".to_string(),
            container_id: "container-abc".to_string(),
            deploy_id: Some(171),
        };
        let line = parse_log_line("test", Utc::now(), LogStream::Stdout, &ctx);
        assert_eq!(line.service, "api");
        assert_eq!(line.env, "2");
        assert_eq!(line.container_id, "container-abc");
        assert_eq!(line.project_id, 7);
        assert!(line.deploy_id.is_some());
    }
}
