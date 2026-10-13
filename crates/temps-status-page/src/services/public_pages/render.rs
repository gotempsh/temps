// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-rendered HTML for a public status page.
//!
//! Rendered once per snapshot refresh, never per request. The page is static
//! HTML with inline CSS and no JavaScript, so the public handler can send a CSP
//! of `default-src 'none'`. Every piece of text that came from a user (title,
//! description, display names, incident update messages) goes through
//! [`escape_html`]; everything else is a fixed vocabulary.

use std::fmt::Write as _;

use temps_core::UtcDateTime;

use super::types::{
    PublicComponentStatus, PublicDayStatus, PublicIncident, PublicIncidentStatus,
    PublicStatusPageResponse,
};

/// Seconds between browser auto-refreshes (`<meta http-equiv="refresh">`),
/// matching the snapshot refresh interval.
const AUTO_REFRESH_SECS: u64 = 60;

/// Escape text for an HTML text node or a double/single-quoted attribute.
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn format_time(time: &UtcDateTime) -> String {
    time.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn overall_label(status: PublicComponentStatus) -> &'static str {
    match status {
        PublicComponentStatus::Operational => "All systems operational",
        PublicComponentStatus::Degraded => "Some systems are degraded",
        PublicComponentStatus::Outage => "Some systems are down",
        PublicComponentStatus::Unknown => "Status not available yet",
    }
}

fn day_class(status: PublicDayStatus) -> &'static str {
    match status {
        PublicDayStatus::NoData => "d-none",
        PublicDayStatus::Operational => "d-ok",
        PublicDayStatus::Degraded => "d-warn",
        PublicDayStatus::Outage => "d-down",
    }
}

const STYLE: &str = r#"
:root{--bg:#fafafa;--fg:#18181b;--muted:#71717a;--card:#fff;--border:#e4e4e7;--ok:#16a34a;--warn:#d97706;--down:#dc2626;--none:#d4d4d8}
@media (prefers-color-scheme:dark){:root{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--card:#18181b;--border:#27272a;--none:#3f3f46}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
main{max-width:860px;margin:0 auto;padding:40px 20px 64px}
h1{font-size:28px;margin:0 0 4px}
h2{font-size:17px;margin:32px 0 12px}
p.desc{color:var(--muted);margin:0 0 24px;white-space:pre-wrap}
.banner{border-radius:10px;padding:16px 20px;font-weight:600;color:#fff;margin:24px 0}
.s-operational{background:var(--ok)}.s-degraded{background:var(--warn)}.s-outage{background:var(--down)}.s-unknown{background:var(--muted)}
.card{background:var(--card);border:1px solid var(--border);border-radius:10px;padding:16px 20px;margin-bottom:12px}
.row{display:flex;justify-content:space-between;align-items:baseline;gap:12px;flex-wrap:wrap}
.name{font-weight:600}
.pill{font-size:13px;font-weight:600}
.t-operational{color:var(--ok)}.t-degraded{color:var(--warn)}.t-outage{color:var(--down)}.t-unknown{color:var(--muted)}
.bars{display:flex;gap:2px;margin:12px 0 6px;height:32px}
.bars span{flex:1;border-radius:2px;min-width:2px}
.d-ok{background:var(--ok)}.d-warn{background:var(--warn)}.d-down{background:var(--down)}.d-none{background:var(--none)}
.legend{display:flex;justify-content:space-between;color:var(--muted);font-size:12px}
.inc h3{font-size:15px;margin:0}
.upd{border-left:2px solid var(--border);padding:2px 0 2px 12px;margin-top:10px}
.upd p{margin:2px 0 0;white-space:pre-wrap;overflow-wrap:anywhere}
.meta{color:var(--muted);font-size:13px}
footer{color:var(--muted);font-size:13px;margin-top:40px}
.sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}
"#;

/// Render the full HTML document for a public status page.
pub fn render_public_page_html(page: &PublicStatusPageResponse) -> String {
    let mut html = String::with_capacity(16 * 1024);
    let title = escape_html(&page.title);

    let _ = write!(
        html,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"refresh\" content=\"{AUTO_REFRESH_SECS}\">\
         <title>{title}</title><style>{STYLE}</style></head><body><main>"
    );

    let _ = write!(html, "<h1>{title}</h1>");
    if let Some(description) = page.description.as_deref().filter(|d| !d.is_empty()) {
        let _ = write!(html, "<p class=\"desc\">{}</p>", escape_html(description));
    }
    let _ = write!(
        html,
        "<div class=\"banner s-{}\" role=\"status\">{}</div>",
        page.status.as_str(),
        overall_label(page.status)
    );

    let (active, resolved): (Vec<&PublicIncident>, Vec<&PublicIncident>) = page
        .incidents
        .iter()
        .partition(|i| i.status != PublicIncidentStatus::Resolved);

    if !active.is_empty() {
        html.push_str("<h2>Active incidents</h2>");
        for incident in active {
            render_incident(&mut html, incident);
        }
    }

    let _ = write!(
        html,
        "<h2>Uptime over the last {} days</h2>",
        page.history_days
    );
    if page.components.is_empty() {
        html.push_str("<div class=\"card meta\">No services are listed on this page.</div>");
    }
    for component in &page.components {
        let name = escape_html(&component.name);
        let uptime = component
            .uptime_percentage
            .map(|pct| format!("{pct:.2}% uptime"))
            .unwrap_or_else(|| "No data yet".to_string());
        let _ = write!(
            html,
            "<section class=\"card\"><div class=\"row\"><span class=\"name\">{name}</span>\
             <span class=\"pill t-{}\">{}</span></div>\
             <div class=\"bars\" role=\"img\" aria-label=\"{name}: {uptime} over the last {} days\">",
            component.status.as_str(),
            component.status.label(),
            page.history_days
        );
        for day in &component.days {
            let detail = day
                .uptime_percentage
                .map(|pct| format!("{pct:.2}% uptime"))
                .unwrap_or_else(|| "no data".to_string());
            let _ = write!(
                html,
                "<span class=\"{}\" title=\"{}: {}\"></span>",
                day_class(day.status),
                day.date,
                detail
            );
        }
        let _ = write!(
            html,
            "</div><div class=\"legend\"><span>{} days ago</span><span>{}</span><span>Today</span></div></section>",
            page.history_days, uptime
        );
    }

    if !resolved.is_empty() {
        html.push_str("<h2>Recently resolved</h2>");
        for incident in resolved {
            render_incident(&mut html, incident);
        }
    }

    let _ = write!(
        html,
        "<footer>Last updated {}. This page refreshes every minute.</footer></main></body></html>",
        format_time(&page.generated_at)
    );
    html
}

fn render_incident(html: &mut String, incident: &PublicIncident) {
    let affected = if incident.affected_components.is_empty() {
        String::new()
    } else {
        format!(
            " · Affects {}",
            incident
                .affected_components
                .iter()
                .map(|name| escape_html(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let resolved = incident
        .resolved_at
        .as_ref()
        .map(|t| format!(" · Resolved {}", format_time(t)))
        .unwrap_or_default();
    let _ = write!(
        html,
        "<section class=\"card inc\"><h3>{} — {}</h3><div class=\"meta\">Started {}{}{}</div>",
        incident.severity.heading(),
        incident.status.label(),
        format_time(&incident.started_at),
        resolved,
        affected
    );
    for update in &incident.updates {
        let _ = write!(
            html,
            "<div class=\"upd\"><span class=\"meta\"><strong>{}</strong> · {}</span><p>{}</p></div>",
            update.status.label(),
            format_time(&update.created_at),
            escape_html(&update.message)
        );
    }
    html.push_str("</section>");
}

/// The body served for any slug that is not a published page. Identical for
/// unknown, malformed, unpublished and deleted slugs, and shared with the
/// proxy's answers on custom hostnames.
pub use temps_core::public_status_page::NOT_FOUND_HTML;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::types::{
        PublicIncidentSeverity, PublicIncidentUpdate, PublicStatusComponent, PublicUptimeDay,
    };
    use chrono::Utc;

    fn page_with(name: &str, message: &str, title: &str) -> PublicStatusPageResponse {
        let now = Utc::now();
        PublicStatusPageResponse {
            title: title.to_string(),
            description: Some("<i>desc</i>".to_string()),
            status: PublicComponentStatus::Degraded,
            components: vec![PublicStatusComponent {
                name: name.to_string(),
                status: PublicComponentStatus::Operational,
                uptime_percentage: Some(99.5),
                days: vec![PublicUptimeDay {
                    date: "2026-10-11".to_string(),
                    status: PublicDayStatus::Operational,
                    uptime_percentage: Some(99.5),
                }],
            }],
            incidents: vec![PublicIncident {
                severity: PublicIncidentSeverity::Major,
                status: PublicIncidentStatus::Identified,
                affected_components: vec![name.to_string()],
                started_at: now,
                resolved_at: None,
                updates: vec![PublicIncidentUpdate {
                    status: PublicIncidentStatus::Identified,
                    message: message.to_string(),
                    created_at: now,
                }],
            }],
            history_days: 1,
            generated_at: now,
        }
    }

    #[test]
    fn escapes_every_user_supplied_string() {
        let html = render_public_page_html(&page_with(
            "<script>alert(1)</script>",
            "\"><img src=x onerror=alert(2)>",
            "</title><script>alert(3)</script>",
        ));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<i>desc</i>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(2)&gt;"));
    }

    #[test]
    fn page_has_no_script_and_is_not_indexed() {
        let html = render_public_page_html(&page_with("API", "Investigating", "Status"));
        assert!(!html.to_ascii_lowercase().contains("<script"));
        assert!(html.contains("noindex"));
        assert!(html.contains("Some systems are degraded"));
        assert!(html.contains("Active incidents"));
        assert!(html.contains("99.50% uptime"));
    }

    #[test]
    fn escape_html_covers_quotes() {
        assert_eq!(escape_html("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&#39;");
    }
}
