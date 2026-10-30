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

use chrono::NaiveDate;
use temps_core::UtcDateTime;

use super::types::{
    PublicComponentStatus, PublicDayStatus, PublicIncident, PublicIncidentSeverity,
    PublicIncidentStatus, PublicStatusComponent, PublicStatusPageResponse, PublicUptimeDay,
    StatusPageTheme,
};

/// Seconds between browser auto-refreshes (`<meta http-equiv="refresh">`),
/// matching the snapshot refresh interval.
const AUTO_REFRESH_SECS: u64 = 60;
/// Days listed under "Past incidents", newest first.
const PAST_INCIDENT_DAYS: u64 = 14;
/// Bars this close to either edge open their tooltip inwards.
const TOOLTIP_EDGE_BARS: usize = 12;
/// Bars shown on narrow screens (the most recent ones).
const NARROW_SCREEN_DAYS: usize = 30;
/// Accent used when the page sets none.
const DEFAULT_ACCENT: &str = "#4f46e5";

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

fn format_day(date: &str) -> String {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%-d %b %Y").to_string())
        .unwrap_or_else(|_| escape_html(date))
}

/// Only a validated `#rrggbb` reaches the stylesheet; anything else (which the
/// service never stores) falls back to the default, so no stored value can
/// break out of the CSS declaration.
fn accent(color: Option<&str>) -> &str {
    match color {
        Some(c)
            if c.len() == 7
                && c.starts_with('#')
                && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()) =>
        {
            c
        }
        _ => DEFAULT_ACCENT,
    }
}

fn overall_label(status: PublicComponentStatus) -> &'static str {
    match status {
        PublicComponentStatus::Operational => "All systems operational",
        PublicComponentStatus::Degraded => "Some systems are degraded",
        PublicComponentStatus::Outage => "Some systems are down",
        PublicComponentStatus::Unknown => "Status not available yet",
    }
}

fn overall_icon(status: PublicComponentStatus) -> &'static str {
    match status {
        PublicComponentStatus::Operational => "&#10003;",
        PublicComponentStatus::Degraded => "!",
        PublicComponentStatus::Outage => "&#10005;",
        PublicComponentStatus::Unknown => "?",
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

fn severity_class(severity: PublicIncidentSeverity) -> &'static str {
    match severity {
        PublicIncidentSeverity::Minor => "sev-minor",
        PublicIncidentSeverity::Major => "sev-major",
        PublicIncidentSeverity::Critical => "sev-critical",
    }
}

const STYLE: &str = r#"
:root{--bg:#f7f7f8;--fg:#18181b;--muted:#6b6b76;--card:#fff;--border:#e4e4e7;--ok:#16a34a;--warn:#e08a00;--down:#dc2626;--none:#d4d4d8;--tip:#fff;--shadow:0 8px 24px rgba(0,0,0,.12)}
html[data-theme=dark]{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--card:#151518;--border:#27272a;--none:#3f3f46;--tip:#1f1f23;--shadow:0 8px 24px rgba(0,0,0,.6)}
@media (prefers-color-scheme:dark){html[data-theme=auto]{--bg:#09090b;--fg:#fafafa;--muted:#a1a1aa;--card:#151518;--border:#27272a;--none:#3f3f46;--tip:#1f1f23;--shadow:0 8px 24px rgba(0,0,0,.6)}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:var(--accent)}
.top{border-top:4px solid var(--accent);background:var(--card);border-bottom:1px solid var(--border)}
.top .in{max-width:880px;margin:0 auto;padding:22px 20px;display:flex;align-items:center;justify-content:space-between;gap:16px;flex-wrap:wrap}
.brand{font-size:22px;font-weight:700;margin:0;display:flex;align-items:center;gap:12px;min-width:0}
.brand img{max-height:40px;max-width:240px;display:block}
.site{font-size:14px;font-weight:600;text-decoration:none;border:1px solid var(--border);border-radius:8px;padding:6px 12px;white-space:nowrap}
.site:hover{border-color:var(--accent)}
main{max-width:880px;margin:0 auto;padding:28px 20px 64px}
h2{font-size:17px;margin:36px 0 12px}
p.desc{color:var(--muted);margin:0 0 4px;white-space:pre-wrap}
.banner{border-radius:12px;padding:18px 22px;font-weight:650;font-size:18px;color:#fff;margin:20px 0 8px;display:flex;align-items:center;gap:14px}
.banner .ic{width:30px;height:30px;border-radius:50%;background:rgba(255,255,255,.25);display:inline-flex;align-items:center;justify-content:center;font-size:16px;flex:none}
.s-operational{background:var(--ok)}.s-degraded{background:var(--warn)}.s-outage{background:var(--down)}.s-unknown{background:var(--muted)}
.card{background:var(--card);border:1px solid var(--border);border-radius:12px}
.comp{padding:18px 22px}
.card>*+*,.members>*+*{border-top:1px solid var(--border)}
.grp>summary{list-style:none;cursor:pointer;padding:18px 22px}
.grp>summary::-webkit-details-marker{display:none}
.grp>summary:hover{background:var(--bg)}
.chev{display:inline-block;width:1em;color:var(--muted);transition:transform .15s}
.grp[open] .chev{transform:rotate(90deg)}
.count{color:var(--muted);font-weight:400;font-size:13px;margin-left:6px}
.members{border-top:1px solid var(--border)}
.members>.comp{padding-left:42px}
.row{display:flex;justify-content:space-between;align-items:baseline;gap:12px;flex-wrap:wrap}
.name{font-weight:600}
.pill{font-size:13px;font-weight:600}
.t-operational{color:var(--ok)}.t-degraded{color:var(--warn)}.t-outage{color:var(--down)}.t-unknown{color:var(--muted)}
.bars{display:flex;gap:2px;margin:12px 0 8px;height:34px}
.bar{flex:1;min-width:2px;border-radius:2px;position:relative;cursor:default}
.bar:hover{box-shadow:inset 0 0 0 40px rgba(255,255,255,.3)}
.d-ok{background:var(--ok)}.d-warn{background:var(--warn)}.d-down{background:var(--down)}.d-none{background:var(--none)}
.tip{display:none;position:absolute;bottom:calc(100% + 10px);left:50%;transform:translateX(-50%);width:270px;background:var(--tip);color:var(--fg);border:1px solid var(--border);border-radius:10px;box-shadow:var(--shadow);padding:14px 16px;z-index:20;font-size:13px;line-height:1.45;pointer-events:none}
.bar:hover .tip{display:block}
.bar.l .tip{left:-4px;transform:none}.bar.r .tip{left:auto;right:-4px;transform:none}
.tip b{display:block;font-size:14px;margin-bottom:6px}
.tip p{margin:0}
.tip .kv{display:flex;justify-content:space-between;gap:8px;color:var(--muted);margin-top:4px}
.tip .kv span:last-child{color:var(--fg);font-variant-numeric:tabular-nums}
.tip ul{margin:8px 0 0;padding:8px 0 0;border-top:1px solid var(--border);list-style:none}
.tip li{font-weight:600}
.legend{display:flex;align-items:center;gap:10px;color:var(--muted);font-size:12px}
.legend .ln{flex:1;height:1px;background:var(--border)}
.n30{display:none}
.rt{margin-top:14px}
.rt .row{font-size:12px;color:var(--muted)}
.rt svg{display:block;width:100%;height:44px;margin-top:4px}
.rt polyline{fill:none;stroke:var(--accent);stroke-width:1.5;stroke-linejoin:round;stroke-linecap:round}
.inc{padding:16px 20px;border-left:4px solid var(--warn);margin-bottom:12px}
.inc.sev-critical{border-left-color:var(--down)}.inc.sev-minor{border-left-color:var(--warn)}.inc.sev-major{border-left-color:#ea580c}
.inc.resolved{border-left-color:var(--ok)}
.inc h3{font-size:16px;margin:0}
.upd{padding:8px 0 0}
.upd+.upd{border-top:1px dashed var(--border);margin-top:8px}
.upd p{margin:2px 0 0;white-space:pre-wrap;overflow-wrap:anywhere}
.meta{color:var(--muted);font-size:13px}
.day{padding:14px 0;border-bottom:1px solid var(--border)}
.day h4{margin:0 0 6px;font-size:14px}
.day .none{color:var(--muted);font-size:14px;margin:0}
.day .item{margin-top:6px}
.day .item a,.day .item strong{font-weight:600}
footer{color:var(--muted);font-size:13px;margin-top:40px;display:flex;justify-content:space-between;gap:12px;flex-wrap:wrap}
@media (max-width:640px){.bar.o{display:none}.bar.nl .tip{left:-4px;transform:none}.n90{display:none}.n30{display:inline}.tip{width:220px}.banner{font-size:16px}}
"#;

/// Render the full HTML document for a public status page.
pub fn render_public_page_html(page: &PublicStatusPageResponse) -> String {
    let mut html = String::with_capacity(32 * 1024);
    let title = escape_html(&page.title);
    let appearance = &page.appearance;
    let theme = match appearance.theme {
        StatusPageTheme::Auto => "auto",
        StatusPageTheme::Light => "light",
        StatusPageTheme::Dark => "dark",
    };

    let _ = write!(
        html,
        "<!doctype html><html lang=\"en\" data-theme=\"{theme}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"refresh\" content=\"{AUTO_REFRESH_SECS}\">\
         <title>{title}</title><style>:root{{--accent:{}}}{STYLE}</style></head><body>",
        accent(appearance.accent_color.as_deref())
    );

    // Header: logo (or the title as text) and a link back to the owner's site.
    html.push_str("<header class=\"top\"><div class=\"in\"><h1 class=\"brand\">");
    match appearance
        .logo_url
        .as_deref()
        .filter(|u| u.starts_with("https://"))
    {
        Some(logo) => {
            let _ = write!(html, "<img src=\"{}\" alt=\"{title}\">", escape_html(logo));
        }
        None => html.push_str(&title),
    }
    html.push_str("</h1>");
    if let Some(site) = appearance
        .website_url
        .as_deref()
        .filter(|u| u.starts_with("https://"))
    {
        let _ = write!(
            html,
            "<a class=\"site\" href=\"{}\" rel=\"noopener noreferrer nofollow\">Visit website &#8599;</a>",
            escape_html(site)
        );
    }
    html.push_str("</div></header><main>");

    if let Some(description) = page.description.as_deref().filter(|d| !d.is_empty()) {
        let _ = write!(html, "<p class=\"desc\">{}</p>", escape_html(description));
    }
    let _ = write!(
        html,
        "<div class=\"banner s-{}\" role=\"status\"><span class=\"ic\" aria-hidden=\"true\">{}</span>{}</div>",
        page.status.as_str(),
        overall_icon(page.status),
        overall_label(page.status)
    );

    let active: Vec<&PublicIncident> = page
        .incidents
        .iter()
        .filter(|i| i.status != PublicIncidentStatus::Resolved)
        .collect();
    if !active.is_empty() {
        html.push_str("<h2>Active incidents</h2>");
        for incident in active {
            render_incident(&mut html, incident);
        }
    }

    let _ = write!(
        html,
        "<h2>Uptime <span class=\"n90\">over the last {} days</span><span class=\"n30\">over the last {} days</span></h2>",
        page.history_days,
        NARROW_SCREEN_DAYS.min(page.history_days as usize)
    );
    html.push_str("<div class=\"card\">");
    if page.components.is_empty() {
        html.push_str("<div class=\"comp meta\">No services are listed on this page.</div>");
    }
    for entry in layout(&page.components) {
        match entry {
            Entry::Single(component) => render_component(&mut html, component, page.history_days),
            Entry::Group(name, members) => {
                render_group(&mut html, name, &members, page.history_days)
            }
        }
    }
    html.push_str("</div>");

    render_past_incidents(&mut html, page);

    let _ = write!(
        html,
        "<footer><span>Last updated {}. This page refreshes every minute.</span><span>Times are in UTC.</span></footer></main></body></html>",
        format_time(&page.generated_at)
    );
    html
}

/// A top-level row of the component list: a component on its own, or a named
/// group of them.
enum Entry<'a> {
    Single(&'a PublicStatusComponent),
    Group(&'a str, Vec<&'a PublicStatusComponent>),
}

/// Arrange components in page order, gathering each group at the position of
/// its first component.
fn layout(components: &[PublicStatusComponent]) -> Vec<Entry<'_>> {
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for component in components {
        match component.group.as_deref() {
            None => entries.push(Entry::Single(component)),
            Some(group) => {
                let existing = entries.iter_mut().find_map(|entry| match entry {
                    Entry::Group(name, members) if *name == group => Some(members),
                    _ => None,
                });
                match existing {
                    Some(members) => members.push(component),
                    None => entries.push(Entry::Group(group, vec![component])),
                }
            }
        }
    }
    entries
}

/// Worst known status among `members`, `Unknown` when none is known.
fn group_status(members: &[&PublicStatusComponent]) -> PublicComponentStatus {
    members
        .iter()
        .map(|c| c.status)
        .filter(|s| *s != PublicComponentStatus::Unknown)
        .max()
        .unwrap_or(PublicComponentStatus::Unknown)
}

/// A collapsible group (no JavaScript: `<details>`). Open by default when
/// something in it is not operational, so problems are never hidden.
fn render_group(
    html: &mut String,
    name: &str,
    members: &[&PublicStatusComponent],
    history_days: u32,
) {
    let status = group_status(members);
    let open = if matches!(
        status,
        PublicComponentStatus::Operational | PublicComponentStatus::Unknown
    ) {
        ""
    } else {
        " open"
    };
    let count = members.len();
    let _ = write!(
        html,
        "<details class=\"grp\"{open}><summary class=\"row\"><span class=\"name\">\
         <span class=\"chev\" aria-hidden=\"true\">&#8250;</span>{}\
         <span class=\"count\">{count} component{}</span></span>\
         <span class=\"pill t-{}\">{}</span></summary><div class=\"members\">",
        escape_html(name),
        if count == 1 { "" } else { "s" },
        status.as_str(),
        status.label()
    );
    for component in members {
        render_component(html, component, history_days);
    }
    html.push_str("</div></details>");
}

fn render_component(html: &mut String, component: &PublicStatusComponent, history_days: u32) {
    let name = escape_html(&component.name);
    let uptime = component
        .uptime_percentage
        .map(|pct| format!("{pct:.2}% uptime"));
    let _ = write!(
        html,
        "<section class=\"comp\"><div class=\"row\"><span class=\"name\">{name}</span>\
         <span class=\"pill t-{}\">{}</span></div>\
         <div class=\"bars\" role=\"img\" aria-label=\"{name}: {} over the last {history_days} days\">",
        component.status.as_str(),
        component.status.label(),
        uptime.as_deref().unwrap_or("daily status")
    );
    let count = component.days.len();
    for (index, day) in component.days.iter().enumerate() {
        let edge = if index < TOOLTIP_EDGE_BARS {
            " l"
        } else if index + TOOLTIP_EDGE_BARS >= count {
            " r"
        } else {
            ""
        };
        // Narrow screens show only the last NARROW_SCREEN_DAYS bars, so the
        // first of those becomes the left edge there.
        let narrow_start = count.saturating_sub(NARROW_SCREEN_DAYS);
        let narrow = if index < narrow_start {
            " o"
        } else if narrow_start > 0 && index < narrow_start + TOOLTIP_EDGE_BARS {
            " nl"
        } else {
            ""
        };
        let _ = write!(
            html,
            "<div class=\"bar {}{edge}{narrow}\"><div class=\"tip\" aria-hidden=\"true\">",
            day_class(day.status)
        );
        render_day_tooltip(html, day);
        html.push_str("</div></div>");
    }
    html.push_str("</div><div class=\"legend\">");
    let _ = write!(
        html,
        "<span class=\"n90\">{history_days} days ago</span><span class=\"n30\">{} days ago</span><span class=\"ln\"></span>",
        NARROW_SCREEN_DAYS.min(history_days as usize)
    );
    if let Some(uptime) = &uptime {
        let _ = write!(html, "<span>{uptime}</span><span class=\"ln\"></span>");
    }
    html.push_str("<span>Today</span></div>");
    render_response_times(html, component);
    html.push_str("</section>");
}

fn render_day_tooltip(html: &mut String, day: &PublicUptimeDay) {
    let _ = write!(html, "<b>{}</b>", format_day(&day.date));
    let summary = match day.status {
        PublicDayStatus::NoData => "No data recorded on this day.".to_string(),
        PublicDayStatus::Operational if day.incidents.is_empty() => {
            "No downtime recorded on this day.".to_string()
        }
        PublicDayStatus::Operational => "No downtime detected by checks.".to_string(),
        PublicDayStatus::Degraded | PublicDayStatus::Outage => {
            match (day.total_checks, day.failed_checks, day.degraded_checks) {
                (Some(total), Some(failed), Some(degraded)) => {
                    let mut parts = Vec::with_capacity(2);
                    if failed > 0 {
                        parts.push(format!("{failed} of {total} checks failed"));
                    }
                    if degraded > 0 {
                        parts.push(format!("{degraded} of {total} checks were slow"));
                    }
                    format!("{}.", parts.join(", "))
                }
                // The page hides uptime, and with it how many checks failed.
                _ if day.status == PublicDayStatus::Outage => {
                    "Checks found the service down.".to_string()
                }
                _ => "Checks found it slow or partly unavailable.".to_string(),
            }
        }
    };
    let _ = write!(html, "<p>{summary}</p>");
    if let Some(pct) = day.uptime_percentage {
        let _ = write!(
            html,
            "<div class=\"kv\"><span>Uptime</span><span>{pct:.2}%</span></div>"
        );
    }
    if let Some(ms) = day.avg_response_time_ms {
        let _ = write!(
            html,
            "<div class=\"kv\"><span>Avg response</span><span>{ms} ms</span></div>"
        );
    }
    if !day.incidents.is_empty() {
        html.push_str("<ul>");
        for title in &day.incidents {
            let _ = write!(html, "<li>{}</li>", escape_html(title));
        }
        html.push_str("</ul>");
    }
}

/// A 90-day trend of the daily mean response time, as inline SVG (no image
/// request, allowed by the page's CSP). Days without measurements break the
/// line instead of being drawn as zero.
fn render_response_times(html: &mut String, component: &PublicStatusComponent) {
    let samples: Vec<u32> = component
        .days
        .iter()
        .filter_map(|d| d.avg_response_time_ms)
        .collect();
    let (Some(&max), Some(latest)) = (samples.iter().max(), component.avg_response_time_ms) else {
        return;
    };
    let max = f64::from(max.max(1));
    let width = 900.0;
    let height = 40.0;
    let step = if component.days.len() > 1 {
        width / (component.days.len() - 1) as f64
    } else {
        width
    };
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    for (index, day) in component.days.iter().enumerate() {
        match day.avg_response_time_ms {
            Some(ms) => {
                let x = index as f64 * step;
                // Keep a 2px margin so the stroke is never clipped.
                let y = 2.0 + (height - 4.0) * (1.0 - f64::from(ms) / max);
                let _ = write!(current, "{x:.1},{y:.1} ");
            }
            None if !current.is_empty() => segments.push(std::mem::take(&mut current)),
            None => {}
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    let name = escape_html(&component.name);
    let _ = write!(
        html,
        "<div class=\"rt\"><div class=\"row\"><span>Response time</span><span>{latest} ms average on the latest day &middot; peak {} ms</span></div>\
         <svg viewBox=\"0 0 {width} {height}\" preserveAspectRatio=\"none\" role=\"img\" aria-label=\"{name}: daily average response time, latest {latest} ms\">",
        max as u32
    );
    for points in segments {
        // A single point would draw nothing; repeat it so it shows as a dot.
        let points = points.trim();
        let points = if points.contains(' ') {
            points.to_string()
        } else {
            format!("{points} {points}")
        };
        let _ = write!(
            html,
            "<polyline points=\"{points}\" vector-effect=\"non-scaling-stroke\"></polyline>"
        );
    }
    html.push_str("</svg></div>");
}

fn affected_text(incident: &PublicIncident) -> String {
    if incident.affected_components.is_empty() {
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
    }
}

fn render_incident(html: &mut String, incident: &PublicIncident) {
    let resolved = incident
        .resolved_at
        .as_ref()
        .map(|t| format!(" · Resolved {}", format_time(t)))
        .unwrap_or_default();
    let _ = write!(
        html,
        "<section class=\"card inc {}{}\"><h3>{}</h3><div class=\"meta\">{} · Started {}{}{}</div>",
        severity_class(incident.severity),
        if incident.status == PublicIncidentStatus::Resolved {
            " resolved"
        } else {
            ""
        },
        escape_html(&incident.title),
        incident.status.label(),
        format_time(&incident.started_at),
        resolved,
        affected_text(incident)
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

/// The last [`PAST_INCIDENT_DAYS`] days, newest first, each with the incidents
/// that started on it, or "No incidents reported."
fn render_past_incidents(html: &mut String, page: &PublicStatusPageResponse) {
    html.push_str("<h2>Past incidents</h2><div>");
    let today = page.generated_at.date_naive();
    for offset in 0..PAST_INCIDENT_DAYS {
        let Some(day) = today.checked_sub_days(chrono::Days::new(offset)) else {
            break;
        };
        let _ = write!(
            html,
            "<div class=\"day\"><h4>{}</h4>",
            day.format("%-d %b %Y")
        );
        let started: Vec<&PublicIncident> = page
            .incidents
            .iter()
            .filter(|i| i.started_at.date_naive() == day)
            .collect();
        if started.is_empty() {
            html.push_str("<p class=\"none\">No incidents reported.</p>");
        }
        for incident in started {
            let latest = incident
                .updates
                .first()
                .map(|u| format!("<p>{}</p>", escape_html(&u.message)))
                .unwrap_or_default();
            let _ = write!(
                html,
                "<div class=\"item\"><strong>{}</strong> <span class=\"meta\">{}{}</span>{latest}</div>",
                escape_html(&incident.title),
                incident.status.label(),
                affected_text(incident)
            );
        }
        html.push_str("</div>");
    }
    html.push_str("</div>");
}

/// The body served for any slug that is not a published page. Identical for
/// unknown, malformed, unpublished and deleted slugs, and shared with the
/// proxy's answers on custom hostnames.
pub use temps_core::public_status_page::NOT_FOUND_HTML;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::types::StatusPageTheme;
    use crate::services::public_pages::types::{PublicIncidentUpdate, StatusPageAppearance};
    use chrono::Utc;

    fn page_with(name: &str, message: &str, title: &str) -> PublicStatusPageResponse {
        let now = Utc::now();
        PublicStatusPageResponse {
            title: title.to_string(),
            description: Some("<i>desc</i>".to_string()),
            appearance: StatusPageAppearance::default(),
            status: PublicComponentStatus::Degraded,
            components: vec![PublicStatusComponent {
                name: name.to_string(),
                group: None,
                status: PublicComponentStatus::Operational,
                uptime_percentage: Some(99.5),
                avg_response_time_ms: Some(142),
                days: vec![PublicUptimeDay {
                    date: "2026-10-11".to_string(),
                    status: PublicDayStatus::Operational,
                    uptime_percentage: Some(99.5),
                    avg_response_time_ms: Some(142),
                    total_checks: Some(1440),
                    failed_checks: Some(0),
                    degraded_checks: Some(0),
                    incidents: vec![message.to_string()],
                }],
            }],
            incidents: vec![PublicIncident {
                title: message.to_string(),
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

    fn grouped(
        name: &str,
        group: Option<&str>,
        status: PublicComponentStatus,
    ) -> PublicStatusComponent {
        let mut component = page_with(name, "x", "Status").components.remove(0);
        component.group = group.map(str::to_string);
        component.status = status;
        component
    }

    #[test]
    fn components_are_gathered_into_their_groups_in_page_order() {
        let components = vec![
            grouped("API", Some("Cloud"), PublicComponentStatus::Operational),
            grouped("Docs", None, PublicComponentStatus::Operational),
            grouped("Builds", Some("Cloud"), PublicComponentStatus::Operational),
        ];
        let entries = layout(&components);
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            Entry::Group(name, members) => {
                assert_eq!(*name, "Cloud");
                let names: Vec<&str> = members.iter().map(|c| c.name.as_str()).collect();
                assert_eq!(names, ["API", "Builds"]);
            }
            Entry::Single(_) => panic!("the group comes first, where its first component is"),
        }
        assert!(matches!(&entries[1], Entry::Single(c) if c.name == "Docs"));
    }

    #[test]
    fn a_group_shows_its_worst_status_and_opens_when_something_is_wrong() {
        let mut page = page_with("API", "x", "Status");
        page.components = vec![
            grouped("API", Some("Cloud <b>"), PublicComponentStatus::Operational),
            grouped("Builds", Some("Cloud <b>"), PublicComponentStatus::Degraded),
        ];
        let html = render_public_page_html(&page);
        assert!(html.contains("<details class=\"grp\" open>"));
        assert!(html.contains("Cloud &lt;b&gt;"));
        assert!(!html.contains("Cloud <b>"));
        assert!(html.contains("2 components"));
        assert!(html.contains("pill t-degraded"));

        page.components[1].status = PublicComponentStatus::Operational;
        let html = render_public_page_html(&page);
        assert!(
            html.contains("<details class=\"grp\">"),
            "all operational stays collapsed"
        );
    }

    #[test]
    fn narrow_screens_open_the_first_visible_tooltips_inwards() {
        let mut page = page_with("API", "x", "Status");
        let day = page.components[0].days[0].clone();
        page.components[0].days = vec![day; 90];
        page.history_days = 90;
        let html = render_public_page_html(&page);
        let classes: Vec<&str> = html
            .split("<div class=\"bar ")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap_or_default())
            .collect();
        assert_eq!(classes.len(), 90);
        // The oldest 60 are hidden on narrow screens...
        assert!(
            classes[..60].iter().all(|c| c.ends_with(" o")),
            "{classes:?}"
        );
        // ...so the next 12 are the left edge there and open inwards.
        assert!(
            classes[60..72].iter().all(|c| c.ends_with(" nl")),
            "{classes:?}"
        );
        assert!(classes[72..]
            .iter()
            .all(|c| !c.contains(" o") && !c.contains(" nl")));
        assert!(STYLE.contains(".bar.o{display:none}.bar.nl .tip{left:-4px;transform:none}"));
    }

    #[test]
    fn hovered_bar_does_not_make_its_tooltip_translucent() {
        // The tooltip is a child of the bar, so anything that fades the bar
        // (opacity, filter) fades the tooltip too and lets the text behind it
        // show through. The hover highlight must only paint the bar itself.
        let rule = STYLE
            .split('}')
            .find(|rule| rule.trim_start().starts_with(".bar:hover{"))
            .expect("the stylesheet has a .bar:hover rule");
        assert!(!rule.contains("opacity"), "{rule}");
        assert!(!rule.contains("filter"), "{rule}");
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

    #[test]
    fn day_tooltip_shows_date_uptime_latency_and_incidents() {
        let html = render_public_page_html(&page_with("API", "Delayed email delivery", "Status"));
        assert!(html.contains("<b>11 Oct 2026</b>"));
        assert!(html.contains("Avg response</span><span>142 ms"));
        assert!(html.contains("<li>Delayed email delivery</li>"));
        assert!(html.contains("Response time"));
        assert!(html.contains("<polyline"));
    }

    #[test]
    fn day_tooltip_says_no_downtime_or_no_data() {
        let mut page = page_with("API", "x", "Status");
        page.incidents.clear();
        let day = page.components[0].days[0].clone();
        page.components[0].days = vec![
            PublicUptimeDay {
                incidents: Vec::new(),
                ..day.clone()
            },
            PublicUptimeDay {
                status: PublicDayStatus::NoData,
                uptime_percentage: None,
                avg_response_time_ms: None,
                total_checks: Some(0),
                incidents: Vec::new(),
                ..day.clone()
            },
            PublicUptimeDay {
                status: PublicDayStatus::Outage,
                failed_checks: Some(300),
                incidents: Vec::new(),
                ..day
            },
        ];
        let html = render_public_page_html(&page);
        assert!(html.contains("No downtime recorded on this day."));
        assert!(html.contains("No data recorded on this day."));
        assert!(html.contains("300 of 1440 checks failed."));
    }

    #[test]
    fn day_tooltip_without_counts_when_uptime_is_hidden() {
        let mut page = page_with("API", "x", "Status");
        page.incidents.clear();
        let day = page.components[0].days[0].clone();
        page.components[0].days = vec![
            PublicUptimeDay {
                status: PublicDayStatus::Outage,
                total_checks: None,
                failed_checks: None,
                degraded_checks: None,
                incidents: Vec::new(),
                ..day.clone()
            },
            PublicUptimeDay {
                status: PublicDayStatus::Degraded,
                total_checks: None,
                failed_checks: None,
                degraded_checks: None,
                incidents: Vec::new(),
                ..day
            },
        ];
        let html = render_public_page_html(&page);
        assert!(html.contains("Checks found the service down."));
        assert!(html.contains("Checks found it slow or partly unavailable."));
        assert!(!html.contains(" checks failed"));
    }

    #[test]
    fn appearance_is_applied_and_escaped() {
        let mut page = page_with("API", "x", "Acme");
        page.appearance = StatusPageAppearance {
            theme: StatusPageTheme::Dark,
            accent_color: Some("#16a34a".to_string()),
            logo_url: Some("https://cdn.example.com/logo.svg?a=1&b=\"x".to_string()),
            website_url: Some("https://example.com/".to_string()),
            show_uptime_percentage: true,
            show_response_times: true,
        };
        let html = render_public_page_html(&page);
        assert!(html.contains("data-theme=\"dark\""));
        assert!(html.contains("--accent:#16a34a"));
        assert!(html.contains(
            "<img src=\"https://cdn.example.com/logo.svg?a=1&amp;b=&quot;x\" alt=\"Acme\">"
        ));
        assert!(html.contains("href=\"https://example.com/\" rel=\"noopener noreferrer nofollow\""));
    }

    #[test]
    fn invalid_stored_accent_or_url_scheme_never_reaches_the_page() {
        let mut page = page_with("API", "x", "Acme");
        page.appearance.accent_color = Some("red;}body{display:none".to_string());
        page.appearance.logo_url = Some("javascript:alert(1)".to_string());
        page.appearance.website_url = Some("http://example.com/".to_string());
        let html = render_public_page_html(&page);
        assert!(html.contains(&format!("--accent:{DEFAULT_ACCENT}")));
        // The stylesheet itself uses `display:none` (hidden tooltips), so
        // look for the injected value rather than the property.
        assert!(!html.contains("red;}body{"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("http://example.com/"));
    }

    #[test]
    fn past_incidents_list_fourteen_days_with_custom_titles() {
        let html = render_public_page_html(&page_with("API", "Delayed email delivery", "Status"));
        assert!(html.contains("Past incidents"));
        assert_eq!(html.matches("<div class=\"day\">").count(), 14);
        assert!(html.contains("<strong>Delayed email delivery</strong>"));
        assert!(html.contains("No incidents reported."));
    }
}
