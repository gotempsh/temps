// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email templates for weekly digest.
//!
//! HTML email rendering rules followed here (do not "modernize" away):
//! - Layout is **table-based**, never `display: flex` / `display: grid`.
//!   Gmail, Outlook, and most mobile clients strip or ignore modern CSS
//!   layout, which collapses flex/grid children into one another (the
//!   `Visitors: 18Page Views: 26` regression). Tables render identically
//!   everywhere.
//! - Critical styling (spacing, colors, widths) is **inlined** on each
//!   element. Many clients drop the `<head><style>` block entirely.
//! - The `<style>` block is kept only as progressive enhancement; the email
//!   must look correct with it removed.

use super::digest_data::*;
use anyhow::Result;

// ── Shared palette ──────────────────────────────────────────────────────────
const BRAND: &str = "#0066cc";
const INK: &str = "#1a1a1a";
const MUTED: &str = "#6b7280";
const BORDER: &str = "#e5e7eb";
const CARD_BG: &str = "#f9fafb";
const PAGE_BG: &str = "#f3f4f6";
const POSITIVE: &str = "#15803d";
const POSITIVE_BG: &str = "#dcfce7";
const NEGATIVE: &str = "#b91c1c";
const NEGATIVE_BG: &str = "#fee2e2";
const NEUTRAL: &str = "#6b7280";
const NEUTRAL_BG: &str = "#f3f4f6";

/// Render HTML email template for weekly digest.
pub fn render_html_template(digest: &WeeklyDigestData) -> Result<String> {
    let project_name = digest.project_name.as_deref().unwrap_or("Your Project");

    let mut body = String::new();

    // ── Executive summary ───────────────────────────────────────────────
    let summary = &digest.executive_summary;
    body.push_str(&section_open("📈 Executive Summary"));
    body.push_str(&metric_grid(&[
        Metric::new("Total Visitors", &format_number(summary.total_visitors))
            .with_trend(summary.visitor_change_percent),
        Metric::new("Deployments", &summary.total_deployments.to_string()).with_note(
            &format!("{} failed", summary.failed_deployments),
            if summary.failed_deployments == 0 {
                Tone::Neutral
            } else {
                Tone::Negative
            },
        ),
        Metric::new("New Errors", &format_number(summary.new_errors)).with_note(
            "this week",
            if summary.new_errors == 0 {
                Tone::Neutral
            } else {
                Tone::Negative
            },
        ),
        Metric::new("Uptime", &format!("{:.1}%", summary.uptime_percent))
            .with_note("of the week", uptime_tone(summary.uptime_percent)),
    ]));
    body.push_str(&section_close());

    // ── Performance ─────────────────────────────────────────────────────
    if let Some(perf) = &digest.performance {
        body.push_str(&section_open("👥 Performance & Analytics"));
        body.push_str(&metric_grid(&[
            Metric::new("Total Visitors", &format_number(perf.total_visitors)),
            Metric::new("Page Views", &format_number(perf.page_views)),
            Metric::new(
                "Avg. Session",
                &format_duration(perf.average_session_duration),
            ),
            Metric::new("Bounce Rate", &format!("{:.1}%", perf.bounce_rate)),
        ]));

        if !perf.top_pages.is_empty() {
            body.push_str(&subhead("Top Pages"));
            let rows: Vec<[String; 3]> = perf
                .top_pages
                .iter()
                .take(5)
                .map(|p| {
                    [
                        escape_html(&p.path),
                        format_number(p.views),
                        format_number(p.unique_visitors),
                    ]
                })
                .collect();
            body.push_str(&data_table(
                &["Page", "Views", "Visitors"],
                &rows,
                &[Align::Left, Align::Right, Align::Right],
            ));
        }

        if !perf.geographic_distribution.is_empty() {
            body.push_str(&subhead("Top Countries"));
            let rows: Vec<[String; 3]> = perf
                .geographic_distribution
                .iter()
                .take(5)
                .map(|g| {
                    [
                        escape_html(&g.country),
                        format_number(g.visitors),
                        format!("{:.1}%", g.percentage),
                    ]
                })
                .collect();
            body.push_str(&data_table(
                &["Country", "Visitors", "Share"],
                &rows,
                &[Align::Left, Align::Right, Align::Right],
            ));
        }
        body.push_str(&section_close());
    }

    // ── Deployments ─────────────────────────────────────────────────────
    if let Some(deploy) = &digest.deployments {
        body.push_str(&section_open("🚀 Deployments & Infrastructure"));
        body.push_str(&metric_grid(&[
            Metric::new("Total", &deploy.total_deployments.to_string()),
            Metric::new("Success Rate", &format!("{:.1}%", deploy.success_rate))
                .with_note("", success_rate_tone(deploy.success_rate)),
            Metric::new("Successful", &deploy.successful_deployments.to_string()),
            Metric::new("Failed", &deploy.failed_deployments.to_string()).with_note(
                "",
                if deploy.failed_deployments == 0 {
                    Tone::Neutral
                } else {
                    Tone::Negative
                },
            ),
        ]));
        body.push_str(&section_close());
    }

    // ── Errors & reliability ────────────────────────────────────────────
    if let Some(errors) = &digest.errors {
        body.push_str(&section_open("⚠️ Errors & Reliability"));
        body.push_str(&metric_grid(&[
            Metric::new("Total Errors", &format_number(errors.total_errors)).with_note(
                "",
                if errors.total_errors == 0 {
                    Tone::Positive
                } else {
                    Tone::Negative
                },
            ),
            Metric::new("New Error Types", &errors.new_error_types.to_string()),
            Metric::new("Uptime", &format!("{:.2}%", errors.uptime_percentage))
                .with_note("", uptime_tone(errors.uptime_percentage)),
            Metric::new(
                "Failed Health Checks",
                &errors.failed_health_checks.to_string(),
            )
            .with_note(
                "",
                if errors.failed_health_checks == 0 {
                    Tone::Positive
                } else {
                    Tone::Negative
                },
            ),
        ]));

        if !errors.most_common_errors.is_empty() {
            body.push_str(&subhead("Most Common Errors"));
            let rows: Vec<[String; 3]> = errors
                .most_common_errors
                .iter()
                .take(5)
                .map(|e| {
                    [
                        escape_html(&e.error_type),
                        format_number(e.count),
                        format_number(e.affected_sessions),
                    ]
                })
                .collect();
            body.push_str(&data_table(
                &["Error", "Occurrences", "Sessions"],
                &rows,
                &[Align::Left, Align::Right, Align::Right],
            ));
        }
        body.push_str(&section_close());
    }

    // ── Funnels ─────────────────────────────────────────────────────────
    if let Some(funnels) = &digest.funnels {
        if funnels.total_funnels > 0 {
            body.push_str(&section_open("🎯 Conversion Funnels"));
            if funnels.funnel_stats.is_empty() {
                body.push_str(&empty_note(&format!(
                    "{} funnel(s) configured — no entries recorded this week.",
                    funnels.total_funnels
                )));
            } else {
                for stat in &funnels.funnel_stats {
                    body.push_str(&funnel_card(stat));
                }
            }
            body.push_str(&section_close());
        }
    }

    // ── Project activity ────────────────────────────────────────────────
    if !digest.projects.is_empty() {
        body.push_str(&section_open("📦 Project Activity"));
        for project in &digest.projects {
            body.push_str(&project_card(project));
        }
        body.push_str(&section_close());
    }

    Ok(wrap_document(
        project_name,
        digest.week_start.format("%b %d, %Y").to_string(),
        digest.week_end.format("%b %d, %Y").to_string(),
        &body,
    ))
}

// ── Document shell ──────────────────────────────────────────────────────────

fn wrap_document(project_name: &str, week_start: String, week_end: String, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="color-scheme" content="light">
<title>Weekly Digest</title>
</head>
<body style="margin:0;padding:0;background-color:{page_bg};">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{page_bg};">
<tr><td align="center" style="padding:24px 12px;">
<table role="presentation" width="600" cellpadding="0" cellspacing="0" border="0" style="width:600px;max-width:600px;background-color:#ffffff;border-radius:10px;overflow:hidden;border:1px solid {border};">
  <tr><td style="background-color:{brand};padding:28px 32px;">
    <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#ffffff;font-size:22px;font-weight:700;line-height:1.3;">📊 Weekly Digest</div>
    <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#cfe4ff;font-size:14px;margin-top:6px;">{project_name} &nbsp;·&nbsp; {week_start} – {week_end}</div>
  </td></tr>
  <tr><td style="padding:8px 32px 24px 32px;">{body}</td></tr>
  <tr><td style="padding:20px 32px 28px 32px;border-top:1px solid {border};">
    <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:{muted};font-size:12px;line-height:1.6;text-align:center;">
      This is an automated weekly digest from Temps.<br>
      Manage your notification preferences in your account settings.
    </div>
  </td></tr>
</table>
</td></tr>
</table>
</body>
</html>
"#,
        page_bg = PAGE_BG,
        border = BORDER,
        brand = BRAND,
        muted = MUTED,
        project_name = escape_html(project_name),
        week_start = week_start,
        week_end = week_end,
        body = body,
    )
}

// ── Section helpers ─────────────────────────────────────────────────────────

fn section_open(title: &str) -> String {
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin-top:24px;">
<tr><td style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:16px;font-weight:700;color:{ink};padding-bottom:6px;border-bottom:2px solid {border};">{title}</td></tr>
<tr><td style="padding-top:14px;">"#,
        ink = INK,
        border = BORDER,
        title = escape_html(title),
    )
}

fn section_close() -> String {
    "</td></tr></table>".to_string()
}

fn subhead(text: &str) -> String {
    format!(
        r#"<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:13px;font-weight:700;color:{muted};text-transform:uppercase;letter-spacing:0.4px;margin:18px 0 8px 0;">{text}</div>"#,
        muted = MUTED,
        text = escape_html(text),
    )
}

fn empty_note(text: &str) -> String {
    format!(
        r#"<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:13px;color:{muted};padding:12px 14px;background-color:{card};border-radius:6px;">{text}</div>"#,
        muted = MUTED,
        card = CARD_BG,
        text = escape_html(text),
    )
}

// ── Metric cards (2-column table, never flex/grid) ──────────────────────────

#[derive(Clone, Copy)]
enum Tone {
    Positive,
    Negative,
    Neutral,
}

impl Tone {
    fn fg(self) -> &'static str {
        match self {
            Tone::Positive => POSITIVE,
            Tone::Negative => NEGATIVE,
            Tone::Neutral => NEUTRAL,
        }
    }
    fn bg(self) -> &'static str {
        match self {
            Tone::Positive => POSITIVE_BG,
            Tone::Negative => NEGATIVE_BG,
            Tone::Neutral => NEUTRAL_BG,
        }
    }
}

struct Metric {
    label: String,
    value: String,
    note: Option<(String, Tone)>,
}

impl Metric {
    fn new(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
            note: None,
        }
    }
    fn with_note(mut self, note: &str, tone: Tone) -> Self {
        if !note.is_empty() {
            self.note = Some((note.to_string(), tone));
        }
        self
    }
    fn with_trend(mut self, change: f64) -> Self {
        let tone = if change > 0.0 {
            Tone::Positive
        } else if change < 0.0 {
            Tone::Negative
        } else {
            Tone::Neutral
        };
        self.note = Some((format!("{:+.1}% vs last week", change), tone));
        self
    }
}

/// Render metrics as a 2-per-row table. Each cell is a fixed 50% width so the
/// layout is stable in every client.
fn metric_grid(metrics: &[Metric]) -> String {
    let mut out = String::from(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">"#,
    );
    for pair in metrics.chunks(2) {
        out.push_str("<tr>");
        for i in 0..2 {
            // 8px gutter via cell padding; empty cell keeps the grid aligned
            // when there is an odd number of metrics.
            let pad = if i == 0 {
                "padding:6px 4px 6px 0;"
            } else {
                "padding:6px 0 6px 4px;"
            };
            match pair.get(i) {
                Some(m) => {
                    out.push_str(&format!(
                        r#"<td width="50%" valign="top" style="{pad}">{card}</td>"#,
                        pad = pad,
                        card = metric_card(m),
                    ));
                }
                None => out.push_str(r#"<td width="50%"></td>"#),
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

fn metric_card(m: &Metric) -> String {
    let note_html = match &m.note {
        Some((text, tone)) => format!(
            r#"<div style="margin-top:8px;"><span style="display:inline-block;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:12px;font-weight:600;color:{fg};background-color:{bg};padding:3px 8px;border-radius:10px;">{text}</span></div>"#,
            fg = tone.fg(),
            bg = tone.bg(),
            text = escape_html(text),
        ),
        None => String::new(),
    };
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{card};border-radius:8px;border-left:4px solid {brand};">
<tr><td style="padding:14px 16px;">
<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:11px;font-weight:600;color:{muted};text-transform:uppercase;letter-spacing:0.5px;">{label}</div>
<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:24px;font-weight:700;color:{ink};margin-top:4px;line-height:1.2;">{value}</div>
{note}
</td></tr></table>"#,
        card = CARD_BG,
        brand = BRAND,
        muted = MUTED,
        ink = INK,
        label = escape_html(&m.label),
        value = escape_html(&m.value),
        note = note_html,
    )
}

// ── Data tables (top pages / countries / errors) ────────────────────────────

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

impl Align {
    fn as_str(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Right => "right",
        }
    }
}

fn data_table<const N: usize>(
    headers: &[&str; N],
    rows: &[[String; N]],
    aligns: &[Align; N],
) -> String {
    let mut out = String::from(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="border-collapse:collapse;">"#,
    );
    // Header row.
    out.push_str("<tr>");
    for i in 0..N {
        out.push_str(&format!(
            r#"<td align="{align}" style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:11px;font-weight:700;color:{muted};text-transform:uppercase;letter-spacing:0.4px;padding:6px 8px;border-bottom:2px solid {border};">{h}</td>"#,
            align = aligns[i].as_str(),
            muted = MUTED,
            border = BORDER,
            h = escape_html(headers[i]),
        ));
    }
    out.push_str("</tr>");
    // Body rows.
    for row in rows {
        out.push_str("<tr>");
        for i in 0..N {
            out.push_str(&format!(
                r#"<td align="{align}" style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:13px;color:{ink};padding:8px;border-bottom:1px solid {border};">{v}</td>"#,
                align = aligns[i].as_str(),
                ink = INK,
                border = BORDER,
                v = row[i],
            ));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

// ── Funnel + project cards ──────────────────────────────────────────────────

fn funnel_card(stat: &FunnelStat) -> String {
    let trend_tone = if stat.week_over_week_change > 0.0 {
        Tone::Positive
    } else if stat.week_over_week_change < 0.0 {
        Tone::Negative
    } else {
        Tone::Neutral
    };
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{card};border-radius:8px;border-left:4px solid {brand};margin-bottom:10px;">
<tr><td style="padding:14px 16px;">
  <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:14px;font-weight:700;color:{ink};">{name}</div>
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin-top:10px;">
    <tr>
      {entries}
      {completions}
      {rate}
    </tr>
  </table>
  <div style="margin-top:10px;"><span style="display:inline-block;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:12px;font-weight:600;color:{trend_fg};background-color:{trend_bg};padding:3px 8px;border-radius:10px;">{trend:+.1}% vs last week</span></div>
</td></tr></table>"#,
        card = CARD_BG,
        brand = BRAND,
        ink = INK,
        name = escape_html(&stat.funnel_name),
        entries = inline_stat("Entries", &format_number(stat.total_entries)),
        completions = inline_stat("Completions", &format_number(stat.total_completions)),
        rate = inline_stat("Conversion", &format!("{:.1}%", stat.completion_rate)),
        trend_fg = trend_tone.fg(),
        trend_bg = trend_tone.bg(),
        trend = stat.week_over_week_change,
    )
}

fn project_card(project: &ProjectStats) -> String {
    let trend_tone = if project.week_over_week_change > 0.0 {
        Tone::Positive
    } else if project.week_over_week_change < 0.0 {
        Tone::Negative
    } else {
        Tone::Neutral
    };
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:{card};border-radius:8px;border-left:4px solid {brand};margin-bottom:10px;">
<tr><td style="padding:14px 16px;">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">
    <tr>
      <td style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:14px;font-weight:700;color:{ink};">{name}</td>
      <td align="right"><span style="display:inline-block;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:12px;font-weight:600;color:{trend_fg};background-color:{trend_bg};padding:3px 8px;border-radius:10px;">{trend:+.1}%</span></td>
    </tr>
  </table>
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin-top:10px;">
    <tr>
      {visitors}
      {page_views}
      {sessions}
      {deployments}
    </tr>
  </table>
</td></tr></table>"#,
        card = CARD_BG,
        brand = BRAND,
        ink = INK,
        name = escape_html(&project.project_name),
        trend_fg = trend_tone.fg(),
        trend_bg = trend_tone.bg(),
        trend = project.week_over_week_change,
        visitors = inline_stat("Visitors", &format_number(project.visitors)),
        page_views = inline_stat("Page Views", &format_number(project.page_views)),
        sessions = inline_stat("Sessions", &format_number(project.unique_sessions)),
        deployments = inline_stat("Deployments", &project.deployments.to_string()),
    )
}

/// A single label-over-value stat as its own table cell. Putting each stat in
/// its own `<td>` is what fixes the `Visitors: 18Page Views: 26` collision —
/// cells cannot run into each other the way inline `<span>`s do.
fn inline_stat(label: &str, value: &str) -> String {
    format!(
        r#"<td valign="top" style="padding-right:14px;">
  <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:11px;color:{muted};text-transform:uppercase;letter-spacing:0.3px;">{label}</div>
  <div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:16px;font-weight:700;color:{ink};margin-top:2px;">{value}</div>
</td>"#,
        muted = MUTED,
        ink = INK,
        label = escape_html(label),
        value = escape_html(value),
    )
}

/// Render plain text email template for weekly digest.
pub fn render_text_template(digest: &WeeklyDigestData) -> Result<String> {
    let project_name = digest.project_name.as_deref().unwrap_or("Your Project");
    let rule = "═".repeat(52);

    let mut text = format!(
        "📊 WEEKLY DIGEST - {}\nWeek of {} to {}\n\n{rule}\n📈 EXECUTIVE SUMMARY\n{rule}\n\n",
        project_name,
        digest.week_start.format("%b %d, %Y"),
        digest.week_end.format("%b %d, %Y"),
        rule = rule,
    );

    let s = &digest.executive_summary;
    text.push_str(&format!(
        "• {} total visitors ({:+.1}% from last week)\n• {} deployments ({} failed)\n• {} new errors detected\n• {:.1}% uptime\n\n",
        format_number(s.total_visitors),
        s.visitor_change_percent,
        s.total_deployments,
        s.failed_deployments,
        format_number(s.new_errors),
        s.uptime_percent,
    ));

    if let Some(perf) = &digest.performance {
        text.push_str(&format!(
            "{rule}\n👥 PERFORMANCE & ANALYTICS\n{rule}\n\nTotal Visitors:   {}\nPage Views:       {}\nUnique Sessions:  {}\nAvg. Session:     {}\nBounce Rate:      {:.1}%\nWeek/Week Change: {:+.1}%\n\n",
            format_number(perf.total_visitors),
            format_number(perf.page_views),
            format_number(perf.unique_sessions),
            format_duration(perf.average_session_duration),
            perf.bounce_rate,
            perf.week_over_week_change,
            rule = rule,
        ));
        if !perf.top_pages.is_empty() {
            text.push_str("Top Pages:\n");
            for p in perf.top_pages.iter().take(5) {
                text.push_str(&format!(
                    "  {} — {} views, {} visitors\n",
                    p.path,
                    format_number(p.views),
                    format_number(p.unique_visitors),
                ));
            }
            text.push('\n');
        }
        if !perf.geographic_distribution.is_empty() {
            text.push_str("Top Countries:\n");
            for g in perf.geographic_distribution.iter().take(5) {
                text.push_str(&format!(
                    "  {} — {} visitors ({:.1}%)\n",
                    g.country,
                    format_number(g.visitors),
                    g.percentage,
                ));
            }
            text.push('\n');
        }
    }

    if let Some(deploy) = &digest.deployments {
        text.push_str(&format!(
            "{rule}\n🚀 DEPLOYMENTS & INFRASTRUCTURE\n{rule}\n\nTotal Deployments: {}\nSuccess Rate:      {:.1}%\nSuccessful:        {}\nFailed:            {}\n\n",
            deploy.total_deployments,
            deploy.success_rate,
            deploy.successful_deployments,
            deploy.failed_deployments,
            rule = rule,
        ));
    }

    if let Some(errors) = &digest.errors {
        text.push_str(&format!(
            "{rule}\n⚠️  ERRORS & RELIABILITY\n{rule}\n\nTotal Errors:        {}\nNew Error Types:     {}\nUptime:              {:.2}%\nFailed Health Checks: {}\n\n",
            format_number(errors.total_errors),
            errors.new_error_types,
            errors.uptime_percentage,
            errors.failed_health_checks,
            rule = rule,
        ));
        if !errors.most_common_errors.is_empty() {
            text.push_str("Most Common Errors:\n");
            for e in errors.most_common_errors.iter().take(5) {
                text.push_str(&format!(
                    "  {} — {} occurrences, {} sessions\n",
                    e.error_type,
                    format_number(e.count),
                    format_number(e.affected_sessions),
                ));
            }
            text.push('\n');
        }
    }

    if let Some(funnels) = &digest.funnels {
        if funnels.total_funnels > 0 {
            text.push_str(&format!(
                "{rule}\n🎯 CONVERSION FUNNELS\n{rule}\n\n",
                rule = rule
            ));
            if funnels.funnel_stats.is_empty() {
                text.push_str(&format!(
                    "{} funnel(s) configured — no entries recorded this week.\n\n",
                    funnels.total_funnels
                ));
            } else {
                for stat in &funnels.funnel_stats {
                    text.push_str(&format!(
                        "{}:\n  {} entries → {} completions | {:.1}% conversion | {:+.1}% vs last week\n\n",
                        stat.funnel_name,
                        format_number(stat.total_entries),
                        format_number(stat.total_completions),
                        stat.completion_rate,
                        stat.week_over_week_change,
                    ));
                }
            }
        }
    }

    if !digest.projects.is_empty() {
        text.push_str(&format!(
            "{rule}\n📦 PROJECT ACTIVITY\n{rule}\n\n",
            rule = rule
        ));
        for project in &digest.projects {
            text.push_str(&format!(
                "{name}:\n  Visitors: {visitors} | Page Views: {page_views} | Sessions: {sessions} | Deployments: {deployments} | Trend: {trend:+.1}%\n\n",
                name = project.project_name,
                visitors = format_number(project.visitors),
                page_views = format_number(project.page_views),
                sessions = format_number(project.unique_sessions),
                deployments = project.deployments,
                trend = project.week_over_week_change,
            ));
        }
    }

    text.push_str(&format!(
        "{rule}\n\nThis is an automated weekly digest from Temps.\nManage your notification preferences in your account settings.\n",
        rule = rule,
    ));

    Ok(text)
}

// ── Formatting helpers ──────────────────────────────────────────────────────

/// Format large numbers with thousands separators.
fn format_number(n: i64) -> String {
    let negative = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (count, c) in digits.chars().rev().enumerate() {
        if count > 0 && count % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let mut result: String = grouped.chars().rev().collect();
    if negative {
        result.insert(0, '-');
    }
    result
}

/// Format a duration given in minutes into a human-readable string.
fn format_duration(minutes: f64) -> String {
    if minutes <= 0.0 {
        return "0s".to_string();
    }
    let total_seconds = (minutes * 60.0).round() as i64;
    let mins = total_seconds / 60;
    let secs = total_seconds % 60;
    if mins == 0 {
        format!("{}s", secs)
    } else if secs == 0 {
        format!("{}m", mins)
    } else {
        format!("{}m {}s", mins, secs)
    }
}

/// Escape a string for safe inclusion in HTML email content. Project names,
/// error types, and page paths are user-controlled.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
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

fn uptime_tone(pct: f64) -> Tone {
    if pct >= 99.9 {
        Tone::Positive
    } else if pct >= 99.0 {
        Tone::Neutral
    } else {
        Tone::Negative
    }
}

fn success_rate_tone(pct: f64) -> Tone {
    if pct >= 95.0 {
        Tone::Positive
    } else if pct >= 80.0 {
        Tone::Neutral
    } else {
        Tone::Negative
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(1234567890), "1,234,567,890");
        assert_eq!(format_number(-4200), "-4,200");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(-1.0), "0s");
        assert_eq!(format_duration(0.5), "30s");
        assert_eq!(format_duration(2.0), "2m");
        assert_eq!(format_duration(2.5), "2m 30s");
    }

    #[test]
    fn test_escape_html() {
        assert_eq!(
            escape_html("<script>alert('x')</script>"),
            "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"
        );
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn test_render_html_template_basic() {
        let now = Utc::now();
        let week_start = now - chrono::Duration::days(7);
        let digest = WeeklyDigestData::new(week_start, now);

        let html = render_html_template(&digest).expect("Failed to render HTML template");

        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Weekly Digest"));
        assert!(html.contains("Executive Summary"));
        // Email-safety: no flex/grid layout that collapses in Gmail/Outlook.
        assert!(!html.contains("display:flex"));
        assert!(!html.contains("display: flex"));
        assert!(!html.contains("display:grid"));
        assert!(!html.contains("display: grid"));
    }

    #[test]
    fn test_render_text_template_basic() {
        let now = Utc::now();
        let week_start = now - chrono::Duration::days(7);
        let digest = WeeklyDigestData::new(week_start, now);

        let text = render_text_template(&digest).expect("Failed to render text template");

        assert!(text.contains("WEEKLY DIGEST"));
        assert!(text.contains("EXECUTIVE SUMMARY"));
    }

    #[test]
    fn test_render_html_with_performance_data() {
        let now = Utc::now();
        let week_start = now - chrono::Duration::days(7);
        let mut digest = WeeklyDigestData::new(week_start, now);

        digest.performance = Some(PerformanceData {
            total_visitors: 1234,
            unique_sessions: 1234,
            page_views: 5678,
            average_session_duration: 5.5,
            bounce_rate: 30.0,
            top_pages: vec![TopPage {
                path: "/pricing".to_string(),
                views: 900,
                unique_visitors: 700,
            }],
            geographic_distribution: vec![],
            visitor_trend: vec![],
            week_over_week_change: 15.0,
        });

        let html = render_html_template(&digest).expect("Failed to render HTML template");

        assert!(html.contains("1,234"));
        assert!(html.contains("5,678"));
        assert!(html.contains("Performance"));
        assert!(html.contains("/pricing")); // top page rendered
    }

    #[test]
    fn test_render_text_with_deployment_data() {
        let now = Utc::now();
        let week_start = now - chrono::Duration::days(7);
        let mut digest = WeeklyDigestData::new(week_start, now);

        digest.deployments = Some(DeploymentData {
            total_deployments: 45,
            successful_deployments: 42,
            failed_deployments: 3,
            success_rate: 93.3,
            average_duration: 2.5,
            preview_environments_created: 10,
            preview_environments_destroyed: 8,
            most_active_projects: vec![],
            deployment_trend: vec![],
        });

        let text = render_text_template(&digest).expect("Failed to render text template");

        assert!(text.contains("45"));
        assert!(text.contains("93.3%"));
        assert!(text.contains("DEPLOYMENTS & INFRASTRUCTURE"));
    }

    #[test]
    fn test_project_card_stats_do_not_collide() {
        // Regression test for the `Visitors: 18Page Views: 26` bug: each stat
        // must be in its own table cell, never inline spans.
        let now = Utc::now();
        let week_start = now - chrono::Duration::days(7);
        let mut digest = WeeklyDigestData::new(week_start, now);
        digest.projects = vec![ProjectStats {
            project_id: 1,
            project_name: "davidviejo-dev".to_string(),
            project_slug: "davidviejo-dev".to_string(),
            visitors: 18,
            page_views: 26,
            unique_sessions: 18,
            deployments: 0,
            week_over_week_change: -14.3,
        }];

        let html = render_html_template(&digest).expect("render");
        // Labels and values are in separate cells, so the rendered output must
        // never contain the run-together strings.
        assert!(!html.contains("18Page"));
        assert!(!html.contains("26Sessions"));
        assert!(html.contains("davidviejo-dev"));
        assert!(html.contains("Project Activity"));
    }
}
