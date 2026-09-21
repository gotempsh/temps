// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
///
/// "duration" is deliberately *not* here: it is one of several spellings
/// [`DURATION_FAMILY`] recognizes, and `promote_canonical_replace_raw` removes
/// whichever spelling matched once it has computed `duration_ms` (ADR-047 §3).
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
];

// --- ADR-047 §3: attribute extraction hard caps -----------------------------

/// Maximum number of keys kept in `LogLine.fields` per line. Canonical
/// (well-known) keys are always considered first, so a noisy line loses its
/// long tail of ad-hoc keys before it loses `trace_id`/`status_code`/etc.
pub const MAX_ATTR_KEYS: usize = 32;

/// Maximum byte length of an attribute key. Keys must also match
/// `[A-Za-z_][A-Za-z0-9_.]*` (see [`is_valid_attr_key`]); numeric-looking keys
/// such as `"123"` or `"0x1f"` are rejected by that same check since they
/// don't start with a letter or underscore.
pub const MAX_ATTR_KEY_BYTES: usize = 64;

/// Maximum byte length of a string attribute value before truncation. Values
/// longer than this are cut at a char boundary and marked with a trailing
/// `…`. `http_route` gets a larger cap ([`HTTP_ROUTE_MAX_BYTES`]) since full
/// route templates can legitimately be longer.
pub const MAX_ATTR_VALUE_BYTES: usize = 256;

/// Cap for the canonical `http_route` attribute specifically (ADR-047 §3).
const HTTP_ROUTE_MAX_BYTES: usize = 512;

// --- ADR-047 §3: well-known attribute spellings -----------------------------

const TRACE_ID_SPELLINGS: &[&str] = &[
    "trace_id",
    "traceid",
    "trace.id",
    "otel.trace_id",
    "dd.trace_id",
];
const SPAN_ID_SPELLINGS: &[&str] = &["span_id", "spanid", "span.id"];
const REQUEST_ID_SPELLINGS: &[&str] = &[
    "request_id",
    "requestid",
    "req_id",
    "rid",
    "x_request_id",
    "http.request_id",
];
const STATUS_CODE_SPELLINGS: &[&str] = &[
    "status",
    "status_code",
    "http.status",
    "http.status_code",
    "code",
];
const HTTP_METHOD_SPELLINGS: &[&str] = &["method", "http.method", "http.request.method"];
const HTTP_ROUTE_SPELLINGS: &[&str] = &["route", "path", "http.route", "http.target", "url.path"];
const DURATION_SPELLINGS: &[&str] = &[
    "duration",
    "duration_ms",
    "dur",
    "latency",
    "elapsed",
    "took",
    "response_time",
];

const HTTP_METHODS: &[&str] = &[
    "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "CONNECT", "TRACE",
];

/// A well-known attribute family: a canonical column name and every spelling
/// that maps to it, matched case-insensitively.
#[derive(Debug, Clone, Copy)]
struct CanonicalFamily {
    canonical: &'static str,
    spellings: &'static [&'static str],
}

const DURATION_FAMILY: CanonicalFamily = CanonicalFamily {
    canonical: "duration_ms",
    spellings: DURATION_SPELLINGS,
};

/// Families other than duration. JSON lines keep their raw key (backward
/// compatible with pre-ADR-047 `fields` shape — see [`add_canonical_keep_raw`])
/// and additionally get the canonical key; logfmt lines have no such legacy
/// shape to preserve, so [`promote_canonical_replace_raw`] is used for them
/// with the full family list below (`ALL_FAMILIES`).
const CANONICAL_FAMILIES: &[CanonicalFamily] = &[
    CanonicalFamily {
        canonical: "trace_id",
        spellings: TRACE_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "span_id",
        spellings: SPAN_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "request_id",
        spellings: REQUEST_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "status_code",
        spellings: STATUS_CODE_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "http_method",
        spellings: HTTP_METHOD_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "http_route",
        spellings: HTTP_ROUTE_SPELLINGS,
    },
];

/// All seven canonical families, duration included — used for logfmt lines.
const ALL_FAMILIES: &[CanonicalFamily] = &[
    CanonicalFamily {
        canonical: "trace_id",
        spellings: TRACE_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "span_id",
        spellings: SPAN_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "request_id",
        spellings: REQUEST_ID_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "status_code",
        spellings: STATUS_CODE_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "http_method",
        spellings: HTTP_METHOD_SPELLINGS,
    },
    CanonicalFamily {
        canonical: "http_route",
        spellings: HTTP_ROUTE_SPELLINGS,
    },
    DURATION_FAMILY,
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

        // Remove keys already promoted to level/msg/ts columns
        for key in EXTRACTED_KEYS {
            map.remove(*key);
        }

        // Flatten one level of nesting (`{"http":{"status":500}}` ->
        // `"http.status": 500`); deeper values are stringified and capped.
        let mut entries = flatten_one_level(map);

        // Duration keeps its pre-ADR-047 behavior: the matched spelling is
        // replaced by the computed `duration_ms`.
        promote_canonical_replace_raw(&mut entries, std::slice::from_ref(&DURATION_FAMILY));

        // The other well-known attributes are *added* alongside the raw key
        // (not a replacement) so pre-existing `fields` shapes — e.g. a raw
        // `"method"` / `"path"` key from a JSON log line — keep working
        // unchanged while also gaining the fixed `http_method`/`http_route`
        // columns the ClickHouse line index reads (ADR-047 §3).
        add_canonical_keep_raw(&mut entries);

        let fields = build_fields(entries);

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

    // logfmt / key=value extraction (ADR-047 §3). `msg` stays the whole raw
    // line — only `fields` is populated from the recognized pairs.
    let mut entries = extract_logfmt_pairs(trimmed);
    promote_canonical_replace_raw(&mut entries, ALL_FAMILIES);
    let fields = build_fields(entries);

    LogLine {
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

/// Parse a duration string like "45ms", "1.2s", "200us", "500000ns" into
/// milliseconds.
///
/// Multi-character suffixes must be checked before single-character suffixes
/// to avoid false matches (e.g., "us"/"ns" before "s", "ms" before "s").
pub fn parse_duration_to_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        rest.trim().parse::<f64>().ok()
    } else if let Some(rest) = s.strip_suffix("µs") {
        rest.trim().parse::<f64>().ok().map(|v| v / 1000.0)
    } else if let Some(rest) = s.strip_suffix("us") {
        rest.trim().parse::<f64>().ok().map(|v| v / 1000.0)
    } else if let Some(rest) = s.strip_suffix("ns") {
        rest.trim().parse::<f64>().ok().map(|v| v / 1_000_000.0)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.trim().parse::<f64>().ok().map(|v| v * 1000.0)
    } else {
        // Try plain number (assume ms)
        s.parse::<f64>().ok()
    }
}

// --- ADR-047 §3: attribute helpers ------------------------------------------

/// Flatten one level of JSON nesting: `{"http":{"status":500}}` becomes
/// `"http.status": 500`. Values nested two or more levels deep (an object or
/// array found *inside* the first nested object, or a top-level array) are
/// stringified as compact JSON text capped at [`MAX_ATTR_VALUE_BYTES`]; if
/// the stringified form doesn't fit, the key is dropped entirely rather than
/// truncating invalid JSON.
fn flatten_one_level(map: serde_json::Map<String, Value>) -> Vec<(String, Value)> {
    let mut out = Vec::with_capacity(map.len());
    for (key, value) in map {
        match value {
            Value::Object(inner) => {
                for (inner_key, inner_value) in inner {
                    let flat_key = format!("{key}.{inner_key}");
                    match inner_value {
                        Value::Object(_) | Value::Array(_) => {
                            if let Some(s) = stringify_capped(&inner_value) {
                                out.push((flat_key, Value::String(s)));
                            }
                        }
                        other => out.push((flat_key, other)),
                    }
                }
            }
            Value::Array(_) => {
                if let Some(s) = stringify_capped(&value) {
                    out.push((key, Value::String(s)));
                }
            }
            other => out.push((key, other)),
        }
    }
    out
}

/// Serialize `v` to compact JSON text, dropping it (returning `None`) if the
/// result exceeds [`MAX_ATTR_VALUE_BYTES`].
fn stringify_capped(v: &Value) -> Option<String> {
    let s = serde_json::to_string(v).ok()?;
    if s.len() <= MAX_ATTR_VALUE_BYTES {
        Some(s)
    } else {
        None
    }
}

/// Scan a non-JSON log line for `key=value` / logfmt pairs in a single pass.
///
/// - Keys must match `[A-Za-z0-9_.]+` immediately followed by `=` (the
///   stricter `[A-Za-z_][A-Za-z0-9_.]*` shape required by ADR-047 §3 is
///   enforced later, in [`is_valid_attr_key`], so obviously-bad keys such as
///   numeric ones are simply dropped rather than mis-parsed here).
/// - Values are either a double-quoted string (supporting `\"` / `\\`
///   escapes) or a bare run of non-whitespace characters.
/// - A token that doesn't start with a key character (e.g. a bare URL path
///   like `/api/v1/users/42`) is never considered a candidate and is skipped
///   as a whole, which is also what naturally excludes glued-on query
///   strings like `/api/x?a=b` from being mis-read as a `a=b` pair — no
///   separate special case is needed since scanning never resumes mid-token.
/// - A bare value containing `://` (a full URL used as a value, e.g.
///   `link=http://example.com`) is rejected: it isn't a meaningful attribute
///   value and would blow the size/shape assumptions callers make.
///
/// Hand-written and single-pass (no `regex` crate) to hit the ADR-047 §3
/// perf target of ~2 µs for a 120-byte line.
fn extract_logfmt_pairs(line: &str) -> Vec<(String, Value)> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let key_start = i;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
        {
            i += 1;
        }
        let key_end = i;

        if key_end > key_start && i < len && bytes[i] == b'=' {
            let key = line[key_start..key_end].to_string();
            i += 1; // skip '='

            if i < len && bytes[i] == b'"' {
                i += 1; // skip opening quote
                let mut val = String::new();
                let mut seg_start = i;
                loop {
                    if i >= len {
                        val.push_str(&line[seg_start..i]);
                        break;
                    }
                    match bytes[i] {
                        b'\\' if i + 1 < len => {
                            val.push_str(&line[seg_start..i]);
                            i += 1;
                            if let Some(ch) = line[i..].chars().next() {
                                val.push(ch);
                                i += ch.len_utf8();
                            }
                            seg_start = i;
                        }
                        b'"' => {
                            val.push_str(&line[seg_start..i]);
                            i += 1; // skip closing quote
                            break;
                        }
                        _ => i += 1,
                    }
                }
                out.push((key, Value::String(val)));
            } else {
                let val_start = i;
                while i < len && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let val = &line[val_start..i];
                if !val.is_empty() && !val.contains("://") {
                    out.push((key, Value::String(val.to_string())));
                }
            }
        } else {
            // Not a `key=value` token at this position — skip the rest of it.
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }

    out
}

/// Validate and normalize a raw value into the canonical shape for `canonical`.
/// Returns `None` when the value doesn't satisfy that attribute's constraints
/// (e.g. a `status` outside 100..=599, or a `method` that isn't a real HTTP
/// verb) — in which case the raw key/value is left untouched by the caller.
fn validate_canonical(canonical: &str, val: &Value) -> Option<Value> {
    match canonical {
        "trace_id" | "span_id" | "request_id" => match val {
            Value::String(s) if !s.is_empty() => {
                Some(truncate_string_value(s, MAX_ATTR_VALUE_BYTES))
            }
            Value::Number(n) => Some(Value::String(n.to_string())),
            _ => None,
        },
        "status_code" => {
            let code = match val {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            }?;
            if (100..=599).contains(&code) {
                Some(Value::Number(serde_json::Number::from(code)))
            } else {
                None
            }
        }
        "http_method" => match val {
            Value::String(s) => {
                let upper = s.to_uppercase();
                if HTTP_METHODS.contains(&upper.as_str()) {
                    Some(Value::String(upper))
                } else {
                    None
                }
            }
            _ => None,
        },
        "http_route" => match val {
            Value::String(s) if !s.is_empty() => {
                Some(truncate_string_value(s, HTTP_ROUTE_MAX_BYTES))
            }
            _ => None,
        },
        "duration_ms" => {
            let ms = match val {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => parse_duration_to_ms(s),
                _ => None,
            }?;
            Some(Value::Number(
                serde_json::Number::from_f64(ms)
                    .unwrap_or_else(|| serde_json::Number::from(ms as i64)),
            ))
        }
        _ => None,
    }
}

/// For each family, if a raw entry matches one of its spellings (case
/// insensitive) and validates, *add* the canonical key alongside the raw
/// one (never removing or renaming it). Used for JSON lines so that a
/// pre-existing `fields` shape — e.g. `{"method":"GET"}` — keeps its raw
/// `method` key exactly as before while also gaining `http_method`.
///
/// No-op if the canonical key is already present verbatim.
fn add_canonical_keep_raw(entries: &mut Vec<(String, Value)>) {
    let mut additions = Vec::new();
    for family in CANONICAL_FAMILIES {
        if entries.iter().any(|(k, _)| k == family.canonical) {
            continue;
        }
        let matched = entries
            .iter()
            .find(|(k, _)| family.spellings.iter().any(|s| k.eq_ignore_ascii_case(s)));
        if let Some((_, val)) = matched {
            if let Some(canon_val) = validate_canonical(family.canonical, val) {
                additions.push((family.canonical.to_string(), canon_val));
            }
        }
    }
    for (offset, item) in additions.into_iter().enumerate() {
        entries.insert(offset, item);
    }
}

/// For each family, if a raw entry matches one of its spellings and
/// validates, remove every entry matching any spelling in that family and
/// insert a single canonical entry in its place. Used for logfmt lines
/// (which have no pre-existing shape to preserve) and, within the JSON path,
/// for `duration` specifically (matching the pre-ADR-047 behavior of
/// replacing `duration` with the computed `duration_ms`).
fn promote_canonical_replace_raw(entries: &mut Vec<(String, Value)>, families: &[CanonicalFamily]) {
    let mut canonical_entries = Vec::new();
    for family in families {
        let match_idx = entries
            .iter()
            .position(|(k, _)| family.spellings.iter().any(|s| k.eq_ignore_ascii_case(s)));
        let Some(idx) = match_idx else {
            continue;
        };
        let (_, val) = &entries[idx];
        if let Some(canon_val) = validate_canonical(family.canonical, val) {
            entries.retain(|(k, _)| !family.spellings.iter().any(|s| k.eq_ignore_ascii_case(s)));
            canonical_entries.push((family.canonical.to_string(), canon_val));
        }
    }
    for (offset, item) in canonical_entries.into_iter().enumerate() {
        entries.insert(offset, item);
    }
}

/// Validate an attribute key against ADR-047 §3: `[A-Za-z_][A-Za-z0-9_.]*`,
/// at most [`MAX_ATTR_KEY_BYTES`] bytes. Numeric-looking keys like `"123"` or
/// `"0x1f"` fail because they don't start with a letter or underscore.
fn is_valid_attr_key(key: &str) -> bool {
    if key.is_empty() || key.len() > MAX_ATTR_KEY_BYTES {
        return false;
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Truncate `s` to at most `cap` bytes on a char boundary, appending `…` when
/// truncated.
fn truncate_str(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

fn truncate_string_value(s: &str, cap: usize) -> Value {
    Value::String(truncate_str(s, cap))
}

fn truncate_json_value(value: Value, cap: usize) -> Value {
    match value {
        Value::String(s) => Value::String(truncate_str(&s, cap)),
        other => other,
    }
}

/// Build the final `fields` object from an ordered list of candidate
/// entries, enforcing every ADR-047 §3 hard cap: at most [`MAX_ATTR_KEYS`]
/// keys (first N in encounter order — canonical entries are placed first by
/// the caller so they always win), key shape/length, and per-value byte
/// truncation.
fn build_fields(entries: Vec<(String, Value)>) -> Option<Value> {
    let mut map = serde_json::Map::new();
    let mut count = 0usize;
    for (key, value) in entries {
        if count >= MAX_ATTR_KEYS {
            break;
        }
        if !is_valid_attr_key(&key) || map.contains_key(&key) {
            continue;
        }
        let cap = if key == "http_route" {
            HTTP_ROUTE_MAX_BYTES
        } else {
            MAX_ATTR_VALUE_BYTES
        };
        map.insert(key, truncate_json_value(value, cap));
        count += 1;
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// Well-known attributes read out of an already-built `fields` object,
/// borrowed with no allocation — used by the ClickHouse line index
/// (ADR-047 §5) to populate `log_lines_index`'s fixed columns.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WellKnownAttrs<'a> {
    pub trace_id: Option<&'a str>,
    pub span_id: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub status_code: Option<u16>,
    pub http_method: Option<&'a str>,
    pub http_route: Option<&'a str>,
    pub duration_ms: Option<f32>,
}

/// The canonical keys [`well_known`] reads — the ones the line index stores
/// in fixed columns rather than in `attrs`.
pub const CANONICAL_KEYS: &[&str] = &[
    "trace_id",
    "span_id",
    "request_id",
    "status_code",
    "http_method",
    "http_route",
    "duration_ms",
];

/// True when `key` is one of [`CANONICAL_KEYS`].
pub fn is_canonical_key(key: &str) -> bool {
    CANONICAL_KEYS.contains(&key)
}

/// Read the canonical well-known attributes out of a `LogLine.fields` value.
/// Cheap: every field is a direct map lookup with no allocation.
pub fn well_known(fields: &Value) -> WellKnownAttrs<'_> {
    let obj = fields.as_object();
    WellKnownAttrs {
        trace_id: obj.and_then(|m| m.get("trace_id")).and_then(Value::as_str),
        span_id: obj.and_then(|m| m.get("span_id")).and_then(Value::as_str),
        request_id: obj
            .and_then(|m| m.get("request_id"))
            .and_then(Value::as_str),
        status_code: obj
            .and_then(|m| m.get("status_code"))
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok()),
        http_method: obj
            .and_then(|m| m.get("http_method"))
            .and_then(Value::as_str),
        http_route: obj
            .and_then(|m| m.get("http_route"))
            .and_then(Value::as_str),
        duration_ms: obj
            .and_then(|m| m.get("duration_ms"))
            .and_then(Value::as_f64)
            .map(|v| v as f32),
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

    // --- ADR-047 §3: logfmt extraction, canonicalization, caps -------------

    #[test]
    fn test_logfmt_basics() {
        let ctx = test_ctx();
        let raw = r#"a=1 b=hello c="hello world""#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["a"], "1");
        assert_eq!(fields["b"], "hello");
        assert_eq!(fields["c"], "hello world");
    }

    #[test]
    fn test_logfmt_quoted_value_with_escapes() {
        let ctx = test_ctx();
        let raw = r#"x="a\"b" y=z"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["x"], "a\"b");
        assert_eq!(fields["y"], "z");
    }

    #[test]
    fn test_logfmt_bare_url_token_not_a_pair() {
        let ctx = test_ctx();
        // The whole log-example from ADR-047 §3: bare tokens like "GET",
        // "/api/v1/users/42", "200", "137ms" never look like key=value pairs.
        let raw = "INFO GET /api/v1/users/42 200 137ms rid=7-31337 worker=3 cache=hit";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert!(!obj.contains_key("GET"));
        assert!(!obj.contains_key("INFO"));
        assert!(!obj.contains_key("200"));
        // "rid" is a request_id spelling and gets promoted to the canonical
        // key; the other two stay as-is.
        assert_eq!(fields["request_id"], "7-31337");
        assert!(!obj.contains_key("rid"));
        assert_eq!(fields["worker"], "3");
        assert_eq!(fields["cache"], "hit");
    }

    #[test]
    fn test_logfmt_url_value_rejected() {
        let ctx = test_ctx();
        let raw = "link=http://example.com/a?x=y other=1";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert!(!obj.contains_key("link"));
        assert_eq!(fields["other"], "1");
    }

    #[test]
    fn test_logfmt_path_value_promotes_to_http_route() {
        let ctx = test_ctx();
        // Unlike a bare token, "path=/api/v1/users" *is* a valid key=value
        // pair whose value happens to start with '/' — this must still be
        // captured and promoted to the canonical `http_route`.
        let raw = "path=/api/v1/users other=1";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["http_route"], "/api/v1/users");
    }

    #[test]
    fn test_logfmt_canonical_trace_and_span_id_spellings() {
        let ctx = test_ctx();
        for raw in [
            "traceId=abc123 spanId=def456",
            "trace_id=abc123 span_id=def456",
            "trace.id=abc123 span.id=def456",
            "otel.trace_id=abc123 span_id=def456",
            "dd.trace_id=abc123 span_id=def456",
        ] {
            let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
            let fields = line.fields.expect("fields should be populated");
            assert_eq!(fields["trace_id"], "abc123", "raw line: {raw}");
            assert_eq!(fields["span_id"], "def456", "raw line: {raw}");
        }
    }

    #[test]
    fn test_logfmt_canonical_request_id_spellings() {
        let ctx = test_ctx();
        for raw in [
            "request_id=42",
            "requestId=42",
            "req_id=42",
            "rid=42",
            "x_request_id=42",
            "http.request_id=42",
        ] {
            let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
            let fields = line.fields.expect("fields should be populated");
            assert_eq!(fields["request_id"], "42", "raw line: {raw}");
        }
    }

    #[test]
    fn test_logfmt_canonical_status_code_spellings() {
        let ctx = test_ctx();
        for raw in [
            "status=500",
            "status_code=500",
            "http.status=500",
            "http.status_code=500",
            "code=500",
        ] {
            let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
            let fields = line.fields.expect("fields should be populated");
            assert_eq!(fields["status_code"], 500, "raw line: {raw}");
        }
    }

    #[test]
    fn test_logfmt_status_code_out_of_range_not_promoted() {
        let ctx = test_ctx();
        let raw = "status=999";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert!(!obj.contains_key("status_code"));
        assert_eq!(fields["status"], "999");
    }

    #[test]
    fn test_logfmt_canonical_http_method_spellings() {
        let ctx = test_ctx();
        for raw in [
            "method=get",
            "http.method=post",
            "http.request.method=DELETE",
        ] {
            let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
            let fields = line.fields.expect("fields should be populated");
            assert!(
                ["GET", "POST", "DELETE"].contains(&fields["http_method"].as_str().unwrap()),
                "raw line: {raw}, fields: {fields:?}"
            );
        }
    }

    #[test]
    fn test_logfmt_invalid_http_method_not_promoted() {
        let ctx = test_ctx();
        let raw = "method=foo";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert!(!obj.contains_key("http_method"));
        assert_eq!(fields["method"], "foo");
    }

    #[test]
    fn test_logfmt_duration_units() {
        let ctx = test_ctx();
        let cases: &[(&str, f64)] = &[
            ("took=250ms", 250.0),
            ("elapsed=2s", 2000.0),
            ("dur=500", 500.0),
            ("latency=500000ns", 0.5),
            ("response_time=200us", 0.2),
        ];
        for (raw, expected) in cases {
            let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
            let fields = line.fields.expect("fields should be populated");
            assert_eq!(
                fields["duration_ms"].as_f64().unwrap(),
                *expected,
                "raw line: {raw}"
            );
        }
    }

    #[test]
    fn test_logfmt_caps_at_32_keys() {
        let ctx = test_ctx();
        let raw = (0..33)
            .map(|i| format!("k{i}={i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let line = parse_log_line(&raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert_eq!(obj.len(), MAX_ATTR_KEYS);
        assert!(obj.contains_key("k0"));
        assert!(obj.contains_key("k31"));
        assert!(!obj.contains_key("k32"));
    }

    #[test]
    fn test_logfmt_long_value_truncated() {
        let ctx = test_ctx();
        let long_value = "a".repeat(300);
        let raw = format!("bigfield={long_value}");
        let line = parse_log_line(&raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let stored = fields["bigfield"].as_str().expect("string value");
        assert!(stored.ends_with('…'));
        assert!(stored.len() <= MAX_ATTR_VALUE_BYTES + '…'.len_utf8());
    }

    #[test]
    fn test_logfmt_bad_keys_dropped() {
        let ctx = test_ctx();
        let raw = "0x1f=value good=1";
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        let obj = fields.as_object().expect("object");
        assert!(!obj.contains_key("0x1f"));
        assert_eq!(fields["good"], "1");
    }

    #[test]
    fn test_json_nested_flattening() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"x","http":{"status":500,"deep":{"a":1}}}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        // one level of nesting flattened with '.'
        assert_eq!(fields["http.status"], 500);
        // "http.status" is itself a status_code spelling -> also promoted
        assert_eq!(fields["status_code"], 500);
        // two levels deep -> stringified JSON text
        assert_eq!(fields["http.deep"], r#"{"a":1}"#);
    }

    #[test]
    fn test_json_deeply_nested_array_stringified() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"x","tags":["a","b","c"]}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["tags"], r#"["a","b","c"]"#);
    }

    #[test]
    fn test_json_raw_fields_preserved_alongside_canonical_additions() {
        // Regression: JSON lines must keep their existing `fields` shape
        // (raw "method"/"path" keys) even though the parser now also adds
        // the canonical `http_method`/`http_route` keys.
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"request","method":"GET","path":"/api"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["method"], "GET");
        assert_eq!(fields["path"], "/api");
        assert_eq!(fields["http_method"], "GET");
        assert_eq!(fields["http_route"], "/api");
    }

    #[test]
    fn test_json_existing_duration_ms_shape_unchanged() {
        let ctx = test_ctx();
        let raw = r#"{"level":"info","msg":"x","duration_ms":12.5,"other":"y"}"#;
        let line = parse_log_line(raw, Utc::now(), LogStream::Stdout, &ctx);
        let fields = line.fields.expect("fields should be populated");
        assert_eq!(fields["duration_ms"], 12.5);
        assert_eq!(fields["other"], "y");
    }

    #[test]
    fn test_well_known_reads_canonical_columns() {
        let fields = serde_json::json!({
            "trace_id": "t-1",
            "span_id": "s-1",
            "request_id": "r-1",
            "status_code": 200,
            "http_method": "GET",
            "http_route": "/api/x",
            "duration_ms": 12.5,
        });
        let wk = well_known(&fields);
        assert_eq!(wk.trace_id, Some("t-1"));
        assert_eq!(wk.span_id, Some("s-1"));
        assert_eq!(wk.request_id, Some("r-1"));
        assert_eq!(wk.status_code, Some(200));
        assert_eq!(wk.http_method, Some("GET"));
        assert_eq!(wk.http_route, Some("/api/x"));
        assert_eq!(wk.duration_ms, Some(12.5));
    }

    #[test]
    fn test_well_known_defaults_when_absent() {
        let wk = well_known(&Value::Null);
        assert_eq!(wk, WellKnownAttrs::default());
    }

    #[test]
    fn test_parse_duration_to_ms_nanoseconds() {
        assert_eq!(parse_duration_to_ms("500000ns"), Some(0.5));
    }

    #[test]
    #[ignore = "micro-benchmark, run with --ignored to see ns/line"]
    fn bench_logfmt_extraction_ns_per_line() {
        let raw = "INFO GET /api/v1/users/42 200 137ms rid=7-31337 worker=3 cache=hit extra=1";
        let iterations = 100_000u32;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = std::hint::black_box(extract_logfmt_pairs(std::hint::black_box(raw)));
        }
        let elapsed = start.elapsed();
        let ns_per_line = elapsed.as_nanos() as f64 / f64::from(iterations);
        eprintln!("extract_logfmt_pairs: {ns_per_line:.1} ns/line over {iterations} iterations");
    }
}
