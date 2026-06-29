//! Shared, project-scoped trace tools for the debugging chat.
//!
//! Unlike per-context provider tools (e.g. the deployment debugger's
//! `read_repo_file`), these apply to EVERY chat context (deployment, alert, …):
//! distributed traces are a project-wide signal, useful whether you're debugging
//! a failed deploy or a firing alert. `ConversationService` merges them into the
//! tool loop alongside the active provider's tools.
//!
//! Backed by the storage-agnostic [`temps_core::TraceReader`] trait, so this
//! crate never depends on the heavy `temps-otel` storage crate. When no reader
//! is registered (OTel disabled), no trace tools are offered.
//!
//! ## Tenancy
//! `project_id` is always the conversation's project, injected by the service —
//! the model only ever supplies a `trace_id`, service name, time window, etc.,
//! never a project id. `get_trace_spans` filters by project, so a model cannot
//! read another tenant's traces even if it guesses a foreign `trace_id`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{Duration, Utc};

use temps_ai::ChatTool;
use temps_core::{TraceQueryFilter, TraceReader, TraceSpanDto, TraceSummaryDto};

/// Default look-back window for `list_traces` when the model omits one.
const DEFAULT_LOOKBACK_MINUTES: i64 = 360; // 6h
/// Hard ceiling on the look-back window (7 days).
const MAX_LOOKBACK_MINUTES: i64 = 7 * 24 * 60;
/// Default / max number of trace summaries returned by `list_traces`.
const DEFAULT_LIST_LIMIT: u64 = 20;
const MAX_LIST_LIMIT: u64 = 50;
/// Max spans rendered by `get_trace` (bounds the model's context budget).
const MAX_SPANS_RENDERED: usize = 100;
/// Approx byte ceiling on a single tool's rendered output.
const MAX_OUTPUT_BYTES: usize = 12_000;
/// Max attributes shown per span in `get_trace`.
const MAX_ATTRS_PER_SPAN: usize = 6;
/// Max exception events shown per span in `get_trace`.
const MAX_EVENTS_PER_SPAN: usize = 3;
/// Upper bound on a model-supplied `trace_id`. A real OTel trace id is 32 hex
/// chars; this rejects absurd input before it reaches the storage backend.
const MAX_TRACE_ID_LEN: usize = 128;

/// Project-scoped trace tools backed by a [`TraceReader`].
pub struct TraceTools {
    reader: Arc<dyn TraceReader>,
}

impl TraceTools {
    pub fn new(reader: Arc<dyn TraceReader>) -> Self {
        Self { reader }
    }

    /// Tool names this helper handles (so the service can route tool calls).
    pub fn handles(&self, name: &str) -> bool {
        matches!(name, "list_traces" | "get_trace")
    }

    /// The trace tool definitions advertised to the model.
    pub fn tools(&self) -> Vec<ChatTool> {
        vec![
            ChatTool {
                name: "list_traces".to_string(),
                description: "List recent distributed traces (OpenTelemetry) for this project, \
newest first. Use it to find failing or slow requests: set only_errors=true to see only traces \
that contain an error, and/or min_duration_ms to find slow ones. Each result includes a trace_id \
— pass it to get_trace to inspect the individual spans."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "only_errors": {
                            "type": "boolean",
                            "description": "Only traces containing an error span. Default false."
                        },
                        "service_name": {
                            "type": "string",
                            "description": "Only traces that include at least one span emitted by this service (exact name)."
                        },
                        "name_pattern": {
                            "type": "string",
                            "description": "Only traces containing a span whose operation name includes this text."
                        },
                        "min_duration_ms": {
                            "type": "number",
                            "description": "Only traces containing at least one span this many milliseconds long (catches slow operations anywhere in the request)."
                        },
                        "lookback_minutes": {
                            "type": "integer",
                            "description": "How far back to search, in minutes. Default 360 (6h), max 10080 (7 days)."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Max traces to return. Default 20, max 50."
                        }
                    },
                    "additionalProperties": false
                }),
            },
            ChatTool {
                name: "get_trace".to_string(),
                description: "Fetch every span of a single trace by its trace_id, rendered as a \
parent/child tree with timings, status, key attributes, and exception events. Use it after \
list_traces to drill into a specific (usually failing or slow) trace and pinpoint the exact span \
that errored or was slow, and why."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "trace_id": {
                            "type": "string",
                            "description": "The trace id to inspect (from list_traces)."
                        }
                    },
                    "required": ["trace_id"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    /// Execute a trace tool. Returns human-readable text either way — failures
    /// come back as text the model can reason about, never as a hard error.
    pub async fn execute(&self, project_id: i32, name: &str, arguments: &str) -> String {
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
        match name {
            "list_traces" => self.list_traces(project_id, &args).await,
            "get_trace" => self.get_trace(project_id, &args).await,
            other => format!("Unknown trace tool '{other}'."),
        }
    }

    async fn list_traces(&self, project_id: i32, args: &serde_json::Value) -> String {
        let only_errors = args
            .get("only_errors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let service_name = arg_string(args, "service_name");
        let name_pattern = arg_string(args, "name_pattern");
        // A negative threshold would match every span; treat it as "no filter".
        let min_duration_ms = args
            .get("min_duration_ms")
            .and_then(arg_as_f64)
            .filter(|v| *v >= 0.0);
        let lookback = args
            .get("lookback_minutes")
            .and_then(arg_as_i64)
            .unwrap_or(DEFAULT_LOOKBACK_MINUTES)
            .clamp(1, MAX_LOOKBACK_MINUTES);
        let limit = args
            .get("limit")
            .and_then(arg_as_u64)
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);

        // Describe the active filters now (before they're moved into the query),
        // so the empty-result hint can name ALL of them — including service_name
        // / name_pattern, which are usually the reason a search comes back empty.
        let mut active_filters: Vec<String> = Vec::new();
        if only_errors {
            active_filters.push("with errors".to_string());
        }
        if let Some(ms) = min_duration_ms {
            active_filters.push(format!("with a span ≥{ms:.0}ms"));
        }
        if let Some(s) = &service_name {
            active_filters.push(format!("service={s}"));
        }
        if let Some(p) = &name_pattern {
            active_filters.push(format!("name~\"{p}\""));
        }

        // Single clock read: the same `now` bounds the query window AND renders
        // the "… ago" ages, so the two are internally consistent.
        let now = Utc::now();
        let start = now - Duration::minutes(lookback);
        let filter = TraceQueryFilter {
            project_id,
            only_errors,
            service_name,
            name_pattern,
            min_duration_ms,
            start_time: Some(start),
            end_time: Some(now),
            limit: Some(limit),
        };

        let traces = match self.reader.list_traces(filter).await {
            Ok(t) => t,
            Err(e) => return format!("Could not query traces: {e}"),
        };

        if traces.is_empty() {
            let suffix = if active_filters.is_empty() {
                String::new()
            } else {
                format!(" ({})", active_filters.join(", "))
            };
            return format!(
                "No traces found in the last {lookback} minutes for this project{suffix}. \
Try a larger lookback_minutes, or remove filters \
(only_errors / min_duration_ms / service_name / name_pattern)."
            );
        }

        let mut out = format!(
            "Found {} trace(s) in the last {} minutes (newest first). \
Call get_trace with a trace_id to inspect its spans:\n\n",
            traces.len(),
            lookback
        );
        for t in &traces {
            out.push_str(&render_summary_line(t, now));
            if out.len() >= MAX_OUTPUT_BYTES {
                out.push_str("… (output truncated)\n");
                break;
            }
        }
        out
    }

    async fn get_trace(&self, project_id: i32, args: &serde_json::Value) -> String {
        let trace_id = match arg_string(args, "trace_id") {
            Some(t) => t,
            None => return "Invalid arguments: provide a non-empty \"trace_id\".".to_string(),
        };
        if trace_id.len() > MAX_TRACE_ID_LEN {
            return format!("Invalid trace_id: must be at most {MAX_TRACE_ID_LEN} characters.");
        }

        let spans = match self.reader.get_trace_spans(project_id, &trace_id).await {
            Ok(s) => s,
            Err(e) => return format!("Could not read trace '{trace_id}': {e}"),
        };
        if spans.is_empty() {
            return format!(
                "No spans found for trace '{trace_id}' in this project \
(it may have expired, never existed, or belongs to another project)."
            );
        }
        render_trace(&trace_id, spans)
    }
}

// ── argument parsing (lenient: models sometimes send numbers as strings) ─────

fn arg_string(args: &serde_json::Value, key: &str) -> Option<String> {
    let s = args.get(key)?.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn arg_as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn arg_as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn arg_as_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

// ── rendering ────────────────────────────────────────────────────────────────

fn render_summary_line(t: &TraceSummaryDto, now: chrono::DateTime<Utc>) -> String {
    let age = humanize_age((now - t.start_time).num_seconds());
    let env = t
        .deployment_environment
        .as_deref()
        .filter(|e| !e.is_empty())
        .map(|e| format!(" env={e}"))
        .unwrap_or_default();
    format!(
        "• {tid}  [{status}]  {err} err / {spans} spans  {dur}  {svc}{env}  \"{name}\"  {age} ago\n",
        tid = t.trace_id,
        status = t.status.to_uppercase(),
        err = t.error_count,
        spans = t.span_count,
        dur = fmt_duration_ms(t.duration_ms),
        svc = t.service_name,
        name = truncate_str(&t.root_span_name, 80),
        age = age,
    )
}

fn render_trace(trace_id: &str, spans: Vec<TraceSpanDto>) -> String {
    let n = spans.len();
    let id_set: HashSet<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();

    // Build parent -> children index. A span is a root when it has no parent, or
    // its parent isn't in this trace's span set (orphan).
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        match s.parent_span_id.as_deref() {
            Some(p) if id_set.contains(p) => children.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }
    roots.sort_by_key(|&i| spans[i].start_time);
    for kids in children.values_mut() {
        kids.sort_by_key(|&i| spans[i].start_time);
    }

    let error_count = spans.iter().filter(|s| s.status == "error").count();
    let total_dur = roots
        .iter()
        .map(|&i| spans[i].duration_ms)
        .fold(0.0_f64, f64::max);
    let services: std::collections::BTreeSet<&str> =
        spans.iter().map(|s| s.service_name.as_str()).collect();

    let mut out = format!(
        "Trace {trace_id}: {n} span(s), {error_count} error(s), ~{dur} total. Services: {svcs}.\n\n",
        dur = fmt_duration_ms(total_dur),
        svcs = services.into_iter().collect::<Vec<_>>().join(", "),
    );

    // Iterative DFS (avoids deep recursion); visited set guards against cycles
    // from malformed parent pointers. Children pushed reversed so they pop in
    // start-time order.
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|&i| (i, 0usize)).collect();
    let mut rendered = 0usize;
    let mut truncated = false;
    while let Some((idx, depth)) = stack.pop() {
        let span = &spans[idx];
        if !visited.insert(span.span_id.as_str()) {
            continue;
        }
        if rendered >= MAX_SPANS_RENDERED || out.len() >= MAX_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        render_span(&mut out, span, depth);
        rendered += 1;
        if let Some(kids) = children.get(span.span_id.as_str()) {
            for &k in kids.iter().rev() {
                stack.push((k, depth + 1));
            }
        }
    }
    if truncated || rendered < n {
        out.push_str(&format!(
            "\n… {} more span(s) not shown (output bounded).\n",
            n.saturating_sub(rendered)
        ));
    }
    out
}

fn render_span(out: &mut String, s: &TraceSpanDto, depth: usize) {
    let indent = "  ".repeat(depth.min(12));
    let marker = if s.status == "error" { "✗" } else { "·" };
    out.push_str(&format!(
        "{indent}{marker} {name} [{kind}] {dur} {status} ({svc})\n",
        name = truncate_str(&s.name, 100),
        kind = s.kind,
        dur = fmt_duration_ms(s.duration_ms),
        status = s.status.to_uppercase(),
        svc = s.service_name,
    ));
    if s.status == "error" && !s.status_message.is_empty() {
        out.push_str(&format!(
            "{indent}    status: {}\n",
            truncate_str(&s.status_message, 240)
        ));
    }
    for (k, v) in selected_attrs(&s.attributes, s.status == "error") {
        out.push_str(&format!("{indent}    {k} = {}\n", truncate_str(v, 200)));
    }
    let mut shown_events = 0;
    for ev in &s.events {
        if shown_events >= MAX_EVENTS_PER_SPAN {
            break;
        }
        if ev.name.eq_ignore_ascii_case("exception") {
            let kind = ev.attributes.get("exception.type").map(String::as_str);
            let msg = ev.attributes.get("exception.message").map(String::as_str);
            let detail = match (kind, msg) {
                (Some(k), Some(m)) => format!("{k}: {m}"),
                (Some(k), None) => k.to_string(),
                (None, Some(m)) => m.to_string(),
                (None, None) => "(no detail)".to_string(),
            };
            out.push_str(&format!(
                "{indent}    ⚠ exception {}\n",
                truncate_str(&detail, 220)
            ));
            shown_events += 1;
        }
    }
}

/// Pick the most useful attributes for a span: error/exception/http/db/rpc keys
/// first (especially for error spans), capped so a chatty span can't blow the
/// output budget.
fn selected_attrs(attrs: &BTreeMap<String, String>, _is_error: bool) -> Vec<(&String, &String)> {
    let mut v: Vec<(&String, &String)> = attrs.iter().collect();
    // Stable sort: interesting keys first, then alphabetical.
    v.sort_by(|(a, _), (b, _)| {
        is_interesting(b)
            .cmp(&is_interesting(a))
            .then_with(|| a.cmp(b))
    });
    v.into_iter().take(MAX_ATTRS_PER_SPAN).collect()
}

fn is_interesting(key: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "error",
        "exception",
        "http.",
        "db.",
        "rpc.",
        "url.",
        "server.",
        "messaging.",
        "otel.status",
        "net.",
    ];
    let k = key.to_ascii_lowercase();
    PREFIXES.iter().any(|p| k.starts_with(p))
        || k.contains("status_code")
        || k.contains("method")
        || k.contains("route")
        || k.contains("target")
}

fn humanize_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{}h", s / 86_400, (s % 86_400) / 3600)
    }
}

fn fmt_duration_ms(ms: f64) -> String {
    if ms < 1.0 {
        format!("{ms:.2}ms")
    } else if ms < 1000.0 {
        format!("{ms:.0}ms")
    } else {
        format!("{:.2}s", ms / 1000.0)
    }
}

/// Char-boundary-safe truncation with an ellipsis — never slices mid-codepoint.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::{TraceReaderError, TraceSpanEventDto};

    fn span(
        span_id: &str,
        parent: Option<&str>,
        status: &str,
        attrs: &[(&str, &str)],
    ) -> TraceSpanDto {
        let now = Utc::now();
        TraceSpanDto {
            span_id: span_id.to_string(),
            parent_span_id: parent.map(str::to_string),
            name: format!("op-{span_id}"),
            kind: "server".to_string(),
            service_name: "api".to_string(),
            start_time: now,
            duration_ms: 5.0,
            status: status.to_string(),
            status_message: if status == "error" {
                "kaboom".to_string()
            } else {
                String::new()
            },
            attributes: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            events: vec![],
        }
    }

    #[test]
    fn test_render_trace_builds_tree_and_marks_errors() {
        let spans = vec![
            span("root", None, "ok", &[("http.method", "GET")]),
            span(
                "child",
                Some("root"),
                "error",
                &[("http.status_code", "500")],
            ),
        ];
        let out = render_trace("abc", spans);
        assert!(out.contains("Trace abc: 2 span(s), 1 error(s)"));
        assert!(out.contains("op-root"));
        // Child is indented under root.
        assert!(out.contains("  ✗ op-child"));
        assert!(out.contains("http.status_code = 500"));
        assert!(out.contains("status: kaboom"));
    }

    #[test]
    fn test_render_trace_orphan_parent_treated_as_root() {
        // Parent id not present in the set → the span is a root, not dropped.
        let spans = vec![span("only", Some("missing-parent"), "ok", &[])];
        let out = render_trace("t", spans);
        assert!(out.contains("op-only"));
        assert!(!out.contains("more span(s) not shown"));
    }

    #[test]
    fn test_render_trace_cycle_does_not_hang() {
        // Two spans that point at each other — must terminate, not loop forever.
        let spans = vec![
            span("a", Some("b"), "ok", &[]),
            span("b", Some("a"), "ok", &[]),
        ];
        let out = render_trace("cyc", spans);
        assert!(out.contains("Trace cyc"));
    }

    #[test]
    fn test_truncate_str_is_char_safe() {
        let s = "a".to_string() + &"😀".repeat(10);
        let out = truncate_str(&s, 4); // cut lands mid-emoji
        assert!(out.ends_with('…'));
        // Did not panic and produced valid UTF-8 (guaranteed by String).
    }

    #[test]
    fn test_selected_attrs_prioritises_interesting() {
        let mut attrs = BTreeMap::new();
        attrs.insert("zzz.custom".to_string(), "1".to_string());
        attrs.insert("http.method".to_string(), "POST".to_string());
        let picked = selected_attrs(&attrs, false);
        assert_eq!(picked[0].0, "http.method");
    }

    // A stub reader to exercise the empty/error branches of execute().
    struct EmptyReader;
    #[async_trait::async_trait]
    impl TraceReader for EmptyReader {
        async fn list_traces(
            &self,
            _f: TraceQueryFilter,
        ) -> Result<Vec<TraceSummaryDto>, TraceReaderError> {
            Ok(vec![])
        }
        async fn get_trace_spans(
            &self,
            _p: i32,
            _t: &str,
        ) -> Result<Vec<TraceSpanDto>, TraceReaderError> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn test_execute_handles_and_empty_messages() {
        let tt = TraceTools::new(Arc::new(EmptyReader));
        assert!(tt.handles("list_traces"));
        assert!(tt.handles("get_trace"));
        assert!(!tt.handles("read_repo_file"));

        let listed = tt
            .execute(7, "list_traces", r#"{"only_errors":true}"#)
            .await;
        assert!(listed.contains("No traces"));

        let got = tt.execute(7, "get_trace", r#"{"trace_id":"nope"}"#).await;
        assert!(got.contains("No spans found"));

        // Missing required trace_id → readable validation text, not a panic.
        let bad = tt.execute(7, "get_trace", "{}").await;
        assert!(bad.contains("Invalid arguments"));

        // Over-long trace_id is rejected before hitting the store.
        let long = "x".repeat(MAX_TRACE_ID_LEN + 1);
        let over = tt
            .execute(7, "get_trace", &format!(r#"{{"trace_id":"{long}"}}"#))
            .await;
        assert!(over.contains("at most"));

        let unknown = tt.execute(7, "frobnicate", "{}").await;
        assert!(unknown.contains("Unknown trace tool"));
    }

    fn summary(trace_id: &str, status: &str, errs: i64, spans: i64) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: trace_id.to_string(),
            root_span_name: "GET /x".to_string(),
            service_name: "api".to_string(),
            deployment_environment: Some("production".to_string()),
            status: status.to_string(),
            start_time: Utc::now(),
            duration_ms: 123.0,
            span_count: spans,
            error_count: errs,
        }
    }

    /// A reader that records the filter it was handed and returns canned rows,
    /// so we can assert arg→filter mapping and clamps without a DB.
    struct CapturingReader {
        captured: std::sync::Mutex<Option<TraceQueryFilter>>,
        summaries: Vec<TraceSummaryDto>,
    }
    #[async_trait::async_trait]
    impl TraceReader for CapturingReader {
        async fn list_traces(
            &self,
            f: TraceQueryFilter,
        ) -> Result<Vec<TraceSummaryDto>, TraceReaderError> {
            *self.captured.lock().expect("capture lock") = Some(f);
            Ok(self.summaries.clone())
        }
        async fn get_trace_spans(
            &self,
            _p: i32,
            _t: &str,
        ) -> Result<Vec<TraceSpanDto>, TraceReaderError> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn test_list_traces_maps_args_clamps_and_renders() {
        let reader = Arc::new(CapturingReader {
            captured: std::sync::Mutex::new(None),
            summaries: vec![summary("trace-abc", "error", 3, 7)],
        });
        let tt = TraceTools::new(reader.clone());
        let out = tt
            .execute(
                7,
                "list_traces",
                r#"{"only_errors":true,"min_duration_ms":500,"service_name":"api","limit":999,"lookback_minutes":99999}"#,
            )
            .await;

        // Success rendering.
        assert!(out.contains("Found 1 trace(s)"));
        assert!(out.contains("trace-abc"));
        assert!(out.contains("[ERROR]"));
        assert!(out.contains("3 err / 7 spans"));
        assert!(out.contains("env=production"));

        // Arg → filter mapping + clamps.
        let f = reader
            .captured
            .lock()
            .expect("lock")
            .clone()
            .expect("filter captured");
        assert_eq!(f.project_id, 7);
        assert!(f.only_errors);
        assert_eq!(f.min_duration_ms, Some(500.0));
        assert_eq!(f.service_name.as_deref(), Some("api"));
        assert_eq!(f.limit, Some(MAX_LIST_LIMIT), "limit clamped to ceiling");
        let start = f.start_time.expect("start");
        let end = f.end_time.expect("end");
        assert_eq!(
            (end - start).num_minutes(),
            MAX_LOOKBACK_MINUTES,
            "lookback clamped to ceiling"
        );
    }

    #[tokio::test]
    async fn test_list_traces_empty_hint_names_all_active_filters() {
        let reader = Arc::new(CapturingReader {
            captured: std::sync::Mutex::new(None),
            summaries: vec![],
        });
        let tt = TraceTools::new(reader);
        let out = tt
            .execute(
                7,
                "list_traces",
                r#"{"service_name":"payments","name_pattern":"checkout"}"#,
            )
            .await;
        assert!(out.contains("No traces"));
        // The hint must surface the service/name filters, not just errors/duration.
        assert!(out.contains("service=payments"));
        assert!(out.contains("checkout"));
    }

    #[test]
    fn test_render_trace_caps_large_tree() {
        let mut spans = vec![span("root", None, "ok", &[])];
        for i in 0..(MAX_SPANS_RENDERED * 2) {
            spans.push(span(&format!("c{i}"), Some("root"), "ok", &[]));
        }
        let out = render_trace("big", spans);
        assert!(out.contains("more span(s) not shown"));
        let rendered = out.matches('·').count() + out.matches('✗').count();
        assert!(
            rendered <= MAX_SPANS_RENDERED && rendered > 0,
            "rendered {rendered} spans, expected <= {MAX_SPANS_RENDERED}"
        );
    }

    #[test]
    fn test_render_trace_renders_exception_events() {
        let mut s = span("root", None, "error", &[]);
        s.events = vec![TraceSpanEventDto {
            timestamp: Utc::now(),
            name: "exception".to_string(),
            attributes: [
                ("exception.type".to_string(), "TypeError".to_string()),
                (
                    "exception.message".to_string(),
                    "x is undefined".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        }];
        let out = render_trace("t", vec![s]);
        assert!(out.contains("⚠ exception TypeError: x is undefined"));
    }

    #[test]
    fn test_humanize_age_and_fmt_duration_branches() {
        assert_eq!(humanize_age(45), "45s");
        assert_eq!(humanize_age(125), "2m");
        assert_eq!(humanize_age(3700), "1h1m");
        assert_eq!(humanize_age(90_000), "1d1h");
        assert_eq!(humanize_age(-5), "0s"); // negative clamped
        assert_eq!(fmt_duration_ms(0.5), "0.50ms");
        assert_eq!(fmt_duration_ms(250.4), "250ms");
        assert_eq!(fmt_duration_ms(1500.0), "1.50s");
    }
}
