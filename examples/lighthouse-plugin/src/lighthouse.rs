//! Lighthouse CLI runner.
//!
//! Spawns the `lighthouse` CLI as a child process, captures JSON output,
//! and parses the results into our typed structs.

use std::time::{Duration, Instant};

use crate::types::*;

/// Run a Lighthouse audit against the given URL.
///
/// Spawns `lighthouse <url> --output=json --chrome-flags='...'` and parses
/// the JSON output into an [`AuditResult`].
///
/// Returns `Err` if:
/// - The `lighthouse` CLI is not found
/// - The process times out or crashes
/// - The output cannot be parsed
pub async fn run_audit(
    url: &str,
    settings: &PluginSettings,
    device_override: Option<&str>,
    categories_override: Option<&[String]>,
) -> Result<AuditResult, LighthouseError> {
    let device = device_override.unwrap_or(&settings.device);
    let categories = categories_override.unwrap_or(&settings.categories);

    let mut args = vec![
        url.to_string(),
        "--output=json".to_string(),
        "--output-path=stdout".to_string(),
        format!("--chrome-flags={}", settings.chrome_flags),
    ];

    // Set form factor
    match device {
        "desktop" => {
            args.push("--preset=desktop".to_string());
        }
        _ => {
            // mobile is the default
        }
    }

    // Set categories
    if !categories.is_empty() {
        for cat in categories {
            args.push(format!("--only-categories={}", cat));
        }
    }

    tracing::info!(url = %url, device = %device, "Running Lighthouse audit");

    let start = Instant::now();

    let output = tokio::time::timeout(
        Duration::from_secs(settings.timeout_secs),
        tokio::process::Command::new("lighthouse")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| LighthouseError::Timeout {
        url: url.to_string(),
        timeout_secs: settings.timeout_secs,
    })?
    .map_err(|e| LighthouseError::SpawnFailed {
        reason: e.to_string(),
    })?;

    let duration = start.elapsed();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LighthouseError::ProcessFailed {
            url: url.to_string(),
            exit_code: output.status.code(),
            stderr: stderr.to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw_json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| LighthouseError::ParseFailed {
            url: url.to_string(),
            reason: e.to_string(),
        })?;

    let result = parse_lighthouse_json(&raw_json, device, duration)?;

    tracing::info!(
        url = %url,
        performance = ?result.performance_score,
        accessibility = ?result.accessibility_score,
        best_practices = ?result.best_practices_score,
        seo = ?result.seo_score,
        duration_ms = duration.as_millis() as u64,
        "Lighthouse audit completed"
    );

    Ok(result)
}

/// Parsed result from a Lighthouse run.
pub struct AuditResult {
    pub performance_score: Option<u32>,
    pub accessibility_score: Option<u32>,
    pub best_practices_score: Option<u32>,
    pub seo_score: Option<u32>,
    pub metrics: Option<CoreWebVitals>,
    pub diagnostics: Vec<AuditDiagnostic>,
    pub raw_json: String,
    pub duration_ms: u64,
    #[allow(dead_code)]
    pub device: String,
}

/// Parse Lighthouse JSON output into our typed result.
fn parse_lighthouse_json(
    json: &serde_json::Value,
    device: &str,
    duration: Duration,
) -> Result<AuditResult, LighthouseError> {
    let categories = json.get("categories").unwrap_or(&serde_json::Value::Null);

    let performance_score = extract_category_score(categories, "performance");
    let accessibility_score = extract_category_score(categories, "accessibility");
    let best_practices_score = extract_category_score(categories, "best-practices");
    let seo_score = extract_category_score(categories, "seo");

    let metrics = extract_core_web_vitals(json);
    let diagnostics = extract_diagnostics(json);

    let raw_json = serde_json::to_string(json).unwrap_or_default();

    Ok(AuditResult {
        performance_score,
        accessibility_score,
        best_practices_score,
        seo_score,
        metrics,
        diagnostics,
        raw_json,
        duration_ms: duration.as_millis() as u64,
        device: device.to_string(),
    })
}

/// Extract a category score (0-100) from the Lighthouse JSON.
/// Lighthouse reports scores as 0.0 to 1.0 — we multiply by 100.
fn extract_category_score(categories: &serde_json::Value, category: &str) -> Option<u32> {
    categories
        .get(category)
        .and_then(|c| c.get("score"))
        .and_then(|s| s.as_f64())
        .map(|s| (s * 100.0).round() as u32)
}

/// Extract Core Web Vitals from the audits section.
fn extract_core_web_vitals(json: &serde_json::Value) -> Option<CoreWebVitals> {
    let audits = json.get("audits")?;

    let lcp_ms = audits
        .get("largest-contentful-paint")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    let fcp_ms = audits
        .get("first-contentful-paint")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    let tbt_ms = audits
        .get("total-blocking-time")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    let cls = audits
        .get("cumulative-layout-shift")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    let speed_index_ms = audits
        .get("speed-index")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    let tti_ms = audits
        .get("interactive")
        .and_then(|a| a.get("numericValue"))
        .and_then(|v| v.as_f64());

    // Only return if we got at least one metric
    if lcp_ms.is_some()
        || fcp_ms.is_some()
        || tbt_ms.is_some()
        || cls.is_some()
        || speed_index_ms.is_some()
        || tti_ms.is_some()
    {
        Some(CoreWebVitals {
            lcp_ms,
            fcp_ms,
            tbt_ms,
            cls,
            speed_index_ms,
            tti_ms,
        })
    } else {
        None
    }
}

/// Extract key diagnostics and opportunities from Lighthouse audits.
/// Only includes audits with a score < 1.0 (failed or warning).
fn extract_diagnostics(json: &serde_json::Value) -> Vec<AuditDiagnostic> {
    let Some(audits) = json.get("audits").and_then(|a| a.as_object()) else {
        return Vec::new();
    };

    // Known opportunity/diagnostic audit IDs to extract
    let interesting_audits = [
        "render-blocking-resources",
        "uses-responsive-images",
        "offscreen-images",
        "unminified-css",
        "unminified-javascript",
        "unused-css-rules",
        "unused-javascript",
        "uses-optimized-images",
        "uses-webp-images",
        "uses-text-compression",
        "uses-rel-preconnect",
        "server-response-time",
        "redirects",
        "uses-http2",
        "efficient-animated-content",
        "dom-size",
        "total-byte-weight",
        "bootup-time",
        "mainthread-work-breakdown",
        "font-display",
        "image-alt",
        "link-name",
        "color-contrast",
        "meta-description",
        "document-title",
        "html-has-lang",
        "meta-viewport",
        "robots-txt",
        "canonical",
    ];

    let mut diagnostics = Vec::new();

    for audit_id in &interesting_audits {
        if let Some(audit) = audits.get(*audit_id) {
            let score = audit.get("score").and_then(|s| s.as_f64());

            // Skip passed audits (score == 1.0) and non-applicable (score is null)
            match score {
                Some(s) if s >= 1.0 => continue,
                None if audit.get("scoreDisplayMode").and_then(|m| m.as_str())
                    == Some("notApplicable") =>
                {
                    continue;
                }
                _ => {}
            }

            let title = audit
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or(*audit_id)
                .to_string();

            let savings = audit
                .get("details")
                .and_then(|d| d.get("overallSavingsMs"))
                .and_then(|ms| ms.as_f64())
                .map(|ms| {
                    if ms >= 1000.0 {
                        format!("Potential savings of {:.1} s", ms / 1000.0)
                    } else {
                        format!("Potential savings of {:.0} ms", ms)
                    }
                });

            let severity = match score {
                Some(s) if s < 0.5 => DiagnosticSeverity::Critical,
                Some(s) if s < 0.9 => DiagnosticSeverity::Warning,
                Some(_) => DiagnosticSeverity::Info,
                None => DiagnosticSeverity::Warning,
            };

            diagnostics.push(AuditDiagnostic {
                id: audit_id.to_string(),
                title,
                score,
                savings,
                severity,
            });
        }
    }

    // Sort: critical first, then warning, then info
    diagnostics.sort_by(|a, b| {
        let severity_order = |s: &DiagnosticSeverity| -> u8 {
            match s {
                DiagnosticSeverity::Critical => 0,
                DiagnosticSeverity::Warning => 1,
                DiagnosticSeverity::Info => 2,
                DiagnosticSeverity::Pass => 3,
            }
        };
        severity_order(&a.severity).cmp(&severity_order(&b.severity))
    });

    diagnostics
}

/// Check if the `lighthouse` CLI is available on PATH.
pub async fn is_lighthouse_available() -> bool {
    tokio::process::Command::new("lighthouse")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum LighthouseError {
    #[error("Failed to spawn Lighthouse process: {reason}")]
    SpawnFailed { reason: String },

    #[error("Lighthouse audit timed out for {url} after {timeout_secs}s")]
    Timeout { url: String, timeout_secs: u64 },

    #[error("Lighthouse process failed for {url} (exit code: {exit_code:?}): {stderr}")]
    ProcessFailed {
        url: String,
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("Failed to parse Lighthouse JSON for {url}: {reason}")]
    ParseFailed { url: String, reason: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_category_score() {
        let json = serde_json::json!({
            "performance": { "score": 0.85 },
            "accessibility": { "score": 0.92 },
            "best-practices": { "score": 1.0 },
            "seo": { "score": 0.78 },
        });

        assert_eq!(extract_category_score(&json, "performance"), Some(85));
        assert_eq!(extract_category_score(&json, "accessibility"), Some(92));
        assert_eq!(extract_category_score(&json, "best-practices"), Some(100));
        assert_eq!(extract_category_score(&json, "seo"), Some(78));
        assert_eq!(extract_category_score(&json, "pwa"), None);
    }

    #[test]
    fn test_extract_category_score_null() {
        let json = serde_json::json!({
            "performance": { "score": null },
        });
        assert_eq!(extract_category_score(&json, "performance"), None);
    }

    #[test]
    fn test_extract_core_web_vitals() {
        let json = serde_json::json!({
            "audits": {
                "largest-contentful-paint": { "numericValue": 2500.0 },
                "first-contentful-paint": { "numericValue": 1800.0 },
                "total-blocking-time": { "numericValue": 300.0 },
                "cumulative-layout-shift": { "numericValue": 0.1 },
                "speed-index": { "numericValue": 3000.0 },
                "interactive": { "numericValue": 4500.0 },
            }
        });

        let cwv = extract_core_web_vitals(&json).unwrap();
        assert_eq!(cwv.lcp_ms, Some(2500.0));
        assert_eq!(cwv.fcp_ms, Some(1800.0));
        assert_eq!(cwv.tbt_ms, Some(300.0));
        assert_eq!(cwv.cls, Some(0.1));
        assert_eq!(cwv.speed_index_ms, Some(3000.0));
        assert_eq!(cwv.tti_ms, Some(4500.0));
    }

    #[test]
    fn test_extract_core_web_vitals_partial() {
        let json = serde_json::json!({
            "audits": {
                "largest-contentful-paint": { "numericValue": 2500.0 },
            }
        });

        let cwv = extract_core_web_vitals(&json).unwrap();
        assert_eq!(cwv.lcp_ms, Some(2500.0));
        assert_eq!(cwv.fcp_ms, None);
    }

    #[test]
    fn test_extract_core_web_vitals_none_when_empty() {
        let json = serde_json::json!({ "audits": {} });
        assert!(extract_core_web_vitals(&json).is_none());
    }

    #[test]
    fn test_extract_diagnostics() {
        let json = serde_json::json!({
            "audits": {
                "render-blocking-resources": {
                    "title": "Eliminate render-blocking resources",
                    "score": 0.3,
                    "details": { "overallSavingsMs": 1200.0 },
                },
                "uses-text-compression": {
                    "title": "Enable text compression",
                    "score": 0.0,
                    "details": { "overallSavingsMs": 500.0 },
                },
                "dom-size": {
                    "title": "Avoids an excessive DOM size",
                    "score": 1.0,
                },
            }
        });

        let diagnostics = extract_diagnostics(&json);

        // dom-size should be excluded (score = 1.0)
        assert_eq!(diagnostics.len(), 2);

        // Both are critical — stable sort keeps iteration order from interesting_audits array
        assert_eq!(diagnostics[0].id, "render-blocking-resources");
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Critical);
        assert!(diagnostics[0].savings.as_ref().unwrap().contains("1.2 s"));

        assert_eq!(diagnostics[1].id, "uses-text-compression");
        assert_eq!(diagnostics[1].severity, DiagnosticSeverity::Critical);
        assert!(diagnostics[1].savings.as_ref().unwrap().contains("500 ms"));
    }

    #[test]
    fn test_parse_lighthouse_json() {
        let json = serde_json::json!({
            "categories": {
                "performance": { "score": 0.72 },
                "accessibility": { "score": 0.95 },
                "best-practices": { "score": 0.88 },
                "seo": { "score": 0.91 },
            },
            "audits": {
                "largest-contentful-paint": { "numericValue": 2500.0 },
                "first-contentful-paint": { "numericValue": 1200.0 },
                "total-blocking-time": { "numericValue": 250.0 },
                "cumulative-layout-shift": { "numericValue": 0.05 },
                "speed-index": { "numericValue": 3200.0 },
                "interactive": { "numericValue": 4100.0 },
            }
        });

        let result = parse_lighthouse_json(&json, "mobile", Duration::from_millis(5000)).unwrap();

        assert_eq!(result.performance_score, Some(72));
        assert_eq!(result.accessibility_score, Some(95));
        assert_eq!(result.best_practices_score, Some(88));
        assert_eq!(result.seo_score, Some(91));
        assert_eq!(result.device, "mobile");
        assert_eq!(result.duration_ms, 5000);

        let cwv = result.metrics.unwrap();
        assert_eq!(cwv.lcp_ms, Some(2500.0));
        assert_eq!(cwv.cls, Some(0.05));
    }

    #[test]
    fn test_lighthouse_error_display() {
        let err = LighthouseError::Timeout {
            url: "https://example.com".to_string(),
            timeout_secs: 60,
        };
        assert!(err.to_string().contains("timed out"));
        assert!(err.to_string().contains("60s"));
    }
}
