//! Email templates for weekly digest

use super::digest_data::*;
use anyhow::Result;

/// Render HTML email template for weekly digest
pub fn render_html_template(digest: &WeeklyDigestData) -> Result<String> {
    let project_name = digest.project_name.as_deref().unwrap_or("Your Project");

    let mut html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Weekly Digest</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 800px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }}
        .container {{
            background-color: white;
            border-radius: 8px;
            padding: 30px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
        }}
        .header {{
            border-bottom: 3px solid #0066cc;
            padding-bottom: 20px;
            margin-bottom: 30px;
        }}
        .header h1 {{
            margin: 0;
            color: #0066cc;
            font-size: 28px;
        }}
        .header .subtitle {{
            color: #666;
            margin-top: 10px;
            font-size: 14px;
        }}
        .section {{
            margin-bottom: 40px;
        }}
        .section-title {{
            font-size: 20px;
            font-weight: bold;
            color: #0066cc;
            margin-bottom: 15px;
            padding-bottom: 10px;
            border-bottom: 2px solid #e0e0e0;
        }}
        .metric-grid {{
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
            gap: 15px;
            margin-bottom: 20px;
        }}
        .metric {{
            padding: 15px;
            background-color: #f9f9f9;
            border-radius: 6px;
            border-left: 4px solid #0066cc;
        }}
        .metric-label {{
            font-size: 12px;
            color: #666;
            text-transform: uppercase;
            letter-spacing: 0.5px;
        }}
        .metric-value {{
            font-size: 24px;
            font-weight: bold;
            color: #333;
            margin-top: 5px;
        }}
        .metric-change {{
            font-size: 14px;
            margin-top: 5px;
        }}
        .metric-change.positive {{
            color: #22c55e;
        }}
        .metric-change.negative {{
            color: #ef4444;
        }}
        .metric-change.neutral {{
            color: #666;
        }}
        .list-item {{
            padding: 10px;
            border-bottom: 1px solid #e0e0e0;
        }}
        .list-item:last-child {{
            border-bottom: none;
        }}
        .badge {{
            display: inline-block;
            padding: 4px 8px;
            border-radius: 4px;
            font-size: 12px;
            font-weight: bold;
        }}
        .badge-success {{
            background-color: #dcfce7;
            color: #166534;
        }}
        .badge-error {{
            background-color: #fee2e2;
            color: #991b1b;
        }}
        .badge-warning {{
            background-color: #fef3c7;
            color: #92400e;
        }}
        .footer {{
            margin-top: 40px;
            padding-top: 20px;
            border-top: 2px solid #e0e0e0;
            text-align: center;
            color: #666;
            font-size: 12px;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>📊 Weekly Digest - {}</h1>
            <div class="subtitle">
                Week of {} to {}
            </div>
        </div>
"#,
        project_name,
        digest.week_start.format("%b %d, %Y"),
        digest.week_end.format("%b %d, %Y")
    );

    // Executive Summary
    html.push_str(&format!(
        r#"
        <div class="section">
            <div class="section-title">📈 Executive Summary</div>
            <div class="metric-grid">
                <div class="metric">
                    <div class="metric-label">Total Visitors</div>
                    <div class="metric-value">{}</div>
                    <div class="metric-change {}">{:+.1}%</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Deployments</div>
                    <div class="metric-value">{}</div>
                    <div class="metric-change {}">({} failed)</div>
                </div>
                <div class="metric">
                    <div class="metric-label">New Errors</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Uptime</div>
                    <div class="metric-value">{:.1}%</div>
                </div>
            </div>
        </div>
"#,
        format_number(digest.executive_summary.total_visitors),
        if digest.executive_summary.visitor_change_percent >= 0.0 {
            "positive"
        } else {
            "negative"
        },
        digest.executive_summary.visitor_change_percent,
        digest.executive_summary.total_deployments,
        if digest.executive_summary.failed_deployments == 0 {
            "neutral"
        } else {
            "negative"
        },
        digest.executive_summary.failed_deployments,
        digest.executive_summary.new_errors,
        digest.executive_summary.uptime_percent
    ));

    // Performance Section
    if let Some(perf) = &digest.performance {
        html.push_str(&format!(
            r#"
        <div class="section">
            <div class="section-title">👥 Performance & Analytics</div>
            <div class="metric-grid">
                <div class="metric">
                    <div class="metric-label">Total Visitors</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Page Views</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Unique Sessions</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Week/Week Change</div>
                    <div class="metric-value {}">{:+.1}%</div>
                </div>
            </div>
        </div>
"#,
            format_number(perf.total_visitors),
            format_number(perf.page_views),
            format_number(perf.unique_sessions),
            if perf.week_over_week_change >= 0.0 {
                "positive"
            } else {
                "negative"
            },
            perf.week_over_week_change
        ));
    }

    // Deployments Section
    if let Some(deploy) = &digest.deployments {
        html.push_str(&format!(
            r#"
        <div class="section">
            <div class="section-title">🚀 Deployments & Infrastructure</div>
            <div class="metric-grid">
                <div class="metric">
                    <div class="metric-label">Total Deployments</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Success Rate</div>
                    <div class="metric-value">{:.1}%</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Successful</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Failed</div>
                    <div class="metric-value">{}</div>
                </div>
            </div>
        </div>
"#,
            deploy.total_deployments,
            deploy.success_rate,
            deploy.successful_deployments,
            deploy.failed_deployments
        ));
    }

    // Errors Section
    if let Some(errors) = &digest.errors {
        html.push_str(&format!(
            r#"
        <div class="section">
            <div class="section-title">⚠️ Errors & Reliability</div>
            <div class="metric-grid">
                <div class="metric">
                    <div class="metric-label">Total Errors</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">New Error Types</div>
                    <div class="metric-value">{}</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Uptime</div>
                    <div class="metric-value">{:.2}%</div>
                </div>
                <div class="metric">
                    <div class="metric-label">Failed Health Checks</div>
                    <div class="metric-value">{}</div>
                </div>
            </div>
        </div>
"#,
            format_number(errors.total_errors),
            errors.new_error_types,
            errors.uptime_percentage,
            errors.failed_health_checks
        ));
    }

    // Projects Section
    if !digest.projects.is_empty() {
        html.push_str(
            r#"
        <div class="section">
            <div class="section-title">📦 Project Activity</div>
"#,
        );

        for project in &digest.projects {
            let trend_class = if project.week_over_week_change >= 0.0 {
                "positive"
            } else {
                "negative"
            };

            html.push_str(&format!(
                r#"
            <div style="margin-bottom: 15px; padding: 12px; background-color: #f9f9f9; border-radius: 6px; border-left: 4px solid #0066cc;">
                <div style="font-weight: bold; font-size: 14px; margin-bottom: 8px;">{}</div>
                <div style="display: flex; gap: 20px; flex-wrap: wrap; font-size: 12px;">
                    <span><strong>Visitors:</strong> {}</span>
                    <span><strong>Page Views:</strong> {}</span>
                    <span><strong>Sessions:</strong> {}</span>
                    <span><strong>Deployments:</strong> {}</span>
                    <span class="metric-change {}"><strong>Trend:</strong> {:+.1}%</span>
                </div>
            </div>
"#,
                project.project_name,
                format_number(project.visitors),
                format_number(project.page_views),
                format_number(project.unique_sessions),
                project.deployments,
                trend_class,
                project.week_over_week_change
            ));
        }

        html.push_str("        </div>\n");
    }

    // Footer
    html.push_str(
        r#"
        <div class="footer">
            <p>This is an automated weekly digest from Temps.</p>
            <p>Manage your notification preferences in your account settings.</p>
        </div>
    </div>
</body>
</html>
"#,
    );

    Ok(html)
}

/// Render plain text email template for weekly digest
pub fn render_text_template(digest: &WeeklyDigestData) -> Result<String> {
    let project_name = digest.project_name.as_deref().unwrap_or("Your Project");

    let mut text = format!(
        r#"📊 WEEKLY DIGEST - {}
Week of {} to {}

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📈 EXECUTIVE SUMMARY
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

• {} total visitors ({:+.1}% from last week)
• {} deployments ({} failed)
• {} new errors detected
• {:.1}% uptime

"#,
        project_name,
        digest.week_start.format("%b %d, %Y"),
        digest.week_end.format("%b %d, %Y"),
        format_number(digest.executive_summary.total_visitors),
        digest.executive_summary.visitor_change_percent,
        digest.executive_summary.total_deployments,
        digest.executive_summary.failed_deployments,
        digest.executive_summary.new_errors,
        digest.executive_summary.uptime_percent
    );

    // Performance Section
    if let Some(perf) = &digest.performance {
        text.push_str(&format!(
            r#"━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
👥 PERFORMANCE & ANALYTICS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Total Visitors:     {}
Page Views:         {}
Unique Sessions:    {}
Week/Week Change:   {:+.1}%

"#,
            format_number(perf.total_visitors),
            format_number(perf.page_views),
            format_number(perf.unique_sessions),
            perf.week_over_week_change
        ));
    }

    // Deployments Section
    if let Some(deploy) = &digest.deployments {
        text.push_str(&format!(
            r#"━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
🚀 DEPLOYMENTS & INFRASTRUCTURE
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Total Deployments:  {}
Success Rate:       {:.1}%
Successful:         {}
Failed:             {}

"#,
            deploy.total_deployments,
            deploy.success_rate,
            deploy.successful_deployments,
            deploy.failed_deployments
        ));
    }

    // Errors Section
    if let Some(errors) = &digest.errors {
        text.push_str(&format!(
            r#"━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
⚠️ ERRORS & RELIABILITY
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Total Errors:       {}
New Error Types:    {}
Uptime:             {:.2}%
Failed Checks:      {}

"#,
            format_number(errors.total_errors),
            errors.new_error_types,
            errors.uptime_percentage,
            errors.failed_health_checks
        ));
    }

    // Projects Section
    if !digest.projects.is_empty() {
        text.push_str(
            r#"━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📦 PROJECT ACTIVITY
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

"#,
        );

        for project in &digest.projects {
            text.push_str(&format!(
                r#"{name}:
  Visitors: {visitors} | Page Views: {page_views} | Sessions: {sessions} | Deployments: {deployments} | Trend: {trend:+.1}%

"#,
                name = project.project_name,
                visitors = format_number(project.visitors),
                page_views = format_number(project.page_views),
                sessions = format_number(project.unique_sessions),
                deployments = project.deployments,
                trend = project.week_over_week_change
            ));
        }
    }

    text.push_str(
        r#"━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

This is an automated weekly digest from Temps.
Manage your notification preferences in your account settings.
"#,
    );

    Ok(text)
}

/// Format large numbers with commas
fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let mut count = 0;

    for c in s.chars().rev() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(c);
        count += 1;
    }

    result.chars().rev().collect()
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
            top_pages: vec![],
            geographic_distribution: vec![],
            visitor_trend: vec![],
            week_over_week_change: 15.0,
        });

        let html = render_html_template(&digest).expect("Failed to render HTML template");

        assert!(html.contains("1,234")); // Total visitors formatted
        assert!(html.contains("5,678")); // Page views formatted
        assert!(html.contains("Performance")); // Section exists
        assert!(html.contains("Analytics")); // Section exists
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

        assert!(text.contains("45")); // Total deployments
        assert!(text.contains("93.3%")); // Success rate
        assert!(text.contains("DEPLOYMENTS & INFRASTRUCTURE"));
    }
}
