// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the time inside an operation goes (ADR 111, D2).
//!
//! From a sample of traces that contain the operation, every span under each
//! call of it is charged its exclusive time: its duration minus the part its
//! own children cover. Those times are summed per downstream call (service,
//! span name, kind), so "this endpoint spends 60% of its time in one query"
//! falls out directly. The operation's own exclusive time is reported as its
//! self time. Calls that run in parallel each keep their exclusive time, so
//! the shares can add up to more than 100%.
//!
//! Optionally the calls of the operation are also split by the value of one
//! of their own attributes ("which tenant, which route, which model is the
//! slow one").

use std::collections::{HashMap, HashSet};

use crate::types::{SpanKind, SpanRecord};

/// Downstream groups returned; the rest are summed into one "other" row.
pub const MAX_GROUPS: usize = 25;
/// Attribute values returned; the rest are summed into one "other" row.
pub const MAX_ATTRIBUTE_VALUES: usize = 20;

/// What a downstream call is, from its attributes and kind.
pub fn category(span: &SpanRecord) -> &'static str {
    let has = |key: &str| span.attributes.contains_key(key);
    if has("db.system") || has("db.system.name") {
        "database"
    } else if has("messaging.system") {
        "messaging"
    } else if span.kind == SpanKind::Client
        && (has("http.request.method") || has("http.method") || has("url.full"))
    {
        "http"
    } else if has("rpc.system") {
        "rpc"
    } else {
        match span.kind {
            SpanKind::Client => "client",
            SpanKind::Producer | SpanKind::Consumer => "messaging",
            SpanKind::Server => "server",
            _ => "internal",
        }
    }
}

/// One downstream call, summed over the sampled calls of the operation.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakdownGroup {
    pub service_name: String,
    pub span_name: String,
    /// `database`, `http`, `messaging`, `rpc`, `client`, `server`,
    /// `internal`, or `other` for the summed remainder.
    pub category: String,
    /// Exclusive time, summed.
    pub total_ms: f64,
    pub calls: u64,
    /// Calls of the operation that made at least one such call.
    pub instances: u64,
}

/// One value of the requested attribute on the operation's own spans.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeSlice {
    /// `None` for calls without the attribute.
    pub value: Option<String>,
    pub calls: u64,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Breakdown {
    /// Traces read.
    pub traces: usize,
    /// Calls of the operation found in them.
    pub instances: u64,
    /// Their summed duration.
    pub total_ms: f64,
    /// Their summed exclusive time.
    pub self_ms: f64,
    /// Largest exclusive time first.
    pub groups: Vec<BreakdownGroup>,
    /// Largest total time first; empty unless an attribute was asked for.
    pub by_attribute: Vec<AttributeSlice>,
}

/// Exclusive time of `span`: its duration minus the union of its children's
/// intervals, clipped to it.
fn exclusive_ms(span: &SpanRecord, children: &[&SpanRecord]) -> f64 {
    let start = span.start_time;
    let end = span.end_time;
    let mut intervals: Vec<(i64, i64)> = children
        .iter()
        .filter_map(|child| {
            let s = child.start_time.max(start);
            let e = child.end_time.min(end);
            (e > s).then(|| {
                (
                    s.timestamp_nanos_opt().unwrap_or(0),
                    e.timestamp_nanos_opt().unwrap_or(0),
                )
            })
        })
        .collect();
    intervals.sort_unstable();
    let mut covered_ns: i64 = 0;
    let mut current: Option<(i64, i64)> = None;
    for (s, e) in intervals {
        current = match current {
            Some((cs, ce)) if s <= ce => Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                covered_ns += ce - cs;
                Some((s, e))
            }
            None => Some((s, e)),
        };
    }
    if let Some((cs, ce)) = current {
        covered_ns += ce - cs;
    }
    (span.duration_ms - covered_ns as f64 / 1_000_000.0).max(0.0)
}

/// Accumulates traces into a [`Breakdown`].
pub struct BreakdownBuilder<'a> {
    service_name: &'a str,
    span_name: &'a str,
    attribute: Option<&'a str>,
    traces: usize,
    instances: u64,
    total_ms: f64,
    self_ms: f64,
    groups: HashMap<(String, String, &'static str), BreakdownGroup>,
    slices: HashMap<Option<String>, AttributeSlice>,
}

impl<'a> BreakdownBuilder<'a> {
    pub fn new(service_name: &'a str, span_name: &'a str, attribute: Option<&'a str>) -> Self {
        Self {
            service_name,
            span_name,
            attribute,
            traces: 0,
            instances: 0,
            total_ms: 0.0,
            self_ms: 0.0,
            groups: HashMap::new(),
            slices: HashMap::new(),
        }
    }

    /// Add one trace's spans. A call of the operation nested inside another
    /// call of it is counted once, through the outer call.
    pub fn add_trace(&mut self, spans: &[SpanRecord]) {
        self.traces += 1;
        let mut children: HashMap<&str, Vec<&SpanRecord>> = HashMap::new();
        let by_id: HashMap<&str, &SpanRecord> =
            spans.iter().map(|span| (span.span_id.as_str(), span)).collect();
        for span in spans {
            if let Some(parent) = span.parent_span_id.as_deref().filter(|p| !p.is_empty()) {
                children.entry(parent).or_default().push(span);
            }
        }
        let is_target = |span: &SpanRecord| {
            span.name == self.span_name && span.resource.service_name == self.service_name
        };
        for root in spans.iter().filter(|span| is_target(span)) {
            // Skip a call nested in another call of the operation.
            let mut ancestor = root.parent_span_id.as_deref();
            let mut nested = false;
            let mut hops = 0;
            while let Some(id) = ancestor.filter(|id| !id.is_empty()) {
                match by_id.get(id) {
                    Some(parent) if is_target(parent) => {
                        nested = true;
                        break;
                    }
                    Some(parent) => ancestor = parent.parent_span_id.as_deref(),
                    None => break,
                }
                hops += 1;
                if hops > spans.len() {
                    break;
                }
            }
            if nested {
                continue;
            }
            self.instances += 1;
            self.total_ms += root.duration_ms;
            let empty = Vec::new();
            let root_children = children.get(root.span_id.as_str()).unwrap_or(&empty);
            self.self_ms += exclusive_ms(root, root_children);
            if let Some(key) = self.attribute {
                let value = root.attributes.get(key).cloned();
                let slice = self.slices.entry(value.clone()).or_insert(AttributeSlice {
                    value,
                    calls: 0,
                    total_ms: 0.0,
                    avg_ms: 0.0,
                    max_ms: 0.0,
                });
                slice.calls += 1;
                slice.total_ms += root.duration_ms;
                slice.max_ms = slice.max_ms.max(root.duration_ms);
            }
            // Walk the descendants; `seen` guards against a malformed cycle.
            let mut stack: Vec<&SpanRecord> = root_children.clone();
            let mut seen: HashSet<&str> = HashSet::new();
            let mut touched: HashSet<(String, String, &'static str)> =
                HashSet::new();
            while let Some(span) = stack.pop() {
                if !seen.insert(span.span_id.as_str()) {
                    continue;
                }
                let own = children.get(span.span_id.as_str()).unwrap_or(&empty);
                let key = (
                    span.resource.service_name.clone(),
                    span.name.clone(),
                    category(span),
                );
                let group = self.groups.entry(key.clone()).or_insert(BreakdownGroup {
                    service_name: key.0.clone(),
                    span_name: key.1.clone(),
                    category: key.2.to_string(),
                    total_ms: 0.0,
                    calls: 0,
                    instances: 0,
                });
                group.total_ms += exclusive_ms(span, own);
                group.calls += 1;
                if touched.insert(key) {
                    group.instances += 1;
                }
                stack.extend(own.iter().copied());
            }
        }
    }

    pub fn finish(self) -> Breakdown {
        let mut groups: Vec<BreakdownGroup> = self.groups.into_values().collect();
        groups.sort_by(|a, b| {
            b.total_ms
                .partial_cmp(&a.total_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.span_name.cmp(&b.span_name))
        });
        if groups.len() > MAX_GROUPS {
            let rest = groups.split_off(MAX_GROUPS - 1);
            groups.push(BreakdownGroup {
                service_name: String::new(),
                span_name: format!("{} other calls", rest.len()),
                category: "other".to_string(),
                total_ms: rest.iter().map(|g| g.total_ms).sum(),
                calls: rest.iter().map(|g| g.calls).sum(),
                instances: rest.iter().map(|g| g.instances).max().unwrap_or(0),
            });
        }
        let mut by_attribute: Vec<AttributeSlice> = self
            .slices
            .into_values()
            .map(|mut slice| {
                slice.avg_ms = slice.total_ms / slice.calls.max(1) as f64;
                slice
            })
            .collect();
        by_attribute.sort_by(|a, b| {
            b.total_ms
                .partial_cmp(&a.total_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.value.cmp(&b.value))
        });
        if by_attribute.len() > MAX_ATTRIBUTE_VALUES {
            let rest = by_attribute.split_off(MAX_ATTRIBUTE_VALUES - 1);
            let calls: u64 = rest.iter().map(|s| s.calls).sum();
            let total_ms: f64 = rest.iter().map(|s| s.total_ms).sum();
            by_attribute.push(AttributeSlice {
                value: Some(format!("{} other values", rest.len())),
                calls,
                total_ms,
                avg_ms: total_ms / calls.max(1) as f64,
                max_ms: rest.iter().map(|s| s.max_ms).fold(0.0, f64::max),
            });
        }
        Breakdown {
            traces: self.traces,
            instances: self.instances,
            total_ms: self.total_ms,
            self_ms: self.self_ms,
            groups,
            by_attribute,
        }
    }
}

#[cfg(test)]
mod tests;
