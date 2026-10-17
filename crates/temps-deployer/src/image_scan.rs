// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire contract for scanning an image on the node that built it.
//!
//! A worker build leaves its image on the worker. Scanning it there, instead
//! of copying it to the control plane first, keeps the multi-minute scan off
//! the control plane and lets a control plane with no Docker daemon scan at
//! all. The control plane persists the findings exactly as it persists those
//! of a local scan; the node only runs the scanner.
//!
//! The scanner's own types live in `temps-vulnerability-scanner`, which
//! depends on this crate, so the contract is restated here as plain data.

use serde::{Deserialize, Serialize};

/// How long the agent lets a scan wait for an image-operation slot. Builds
/// hold the same slot for up to their own 30-minute deadline.
pub const SCAN_ADMISSION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long a scan may run once it holds a slot.
pub const SCAN_RUN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// What the control plane waits for a scan answer: the agent's two
/// deadlines in sequence, plus a minute of transfer and serialization. A
/// client that gave up earlier would discard a scan the agent was still
/// entitled to finish.
pub const SCAN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(
    SCAN_ADMISSION_DEADLINE.as_secs() + SCAN_RUN_DEADLINE.as_secs() + 60,
);

/// Most findings one report may carry. A report over the limit is refused by
/// the agent rather than truncated: a partial list would read as a clean
/// bill of health for whatever fell off the end.
pub const MAX_SCAN_FINDINGS: usize = 50_000;

/// Largest report body the control plane reads from a node.
pub const MAX_SCAN_REPORT_BYTES: usize = 32 * 1024 * 1024;

/// `POST /agent/images/scan` body.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImageScanRequest {
    /// Image reference on the node, e.g. `temps-app:3f2a`.
    pub image: String,
    /// Image id the control plane recorded at build time. When present, the
    /// node refuses to scan a tag that now names a different image, so the
    /// findings always describe the image that was deployed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_image_id: Option<String>,
}

/// One vulnerability found in an image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImageScanFinding {
    pub vulnerability_id: String,
    pub package_name: String,
    pub installed_version: String,
    #[serde(default)]
    pub fixed_version: Option<String>,
    /// `CRITICAL`, `HIGH`, `MEDIUM`, `LOW`; anything else is `UNKNOWN`.
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub cvss_score: Option<f32>,
    #[serde(default)]
    pub primary_url: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub published_date: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_modified_date: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub vuln_type: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

/// The result of a scan run on a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImageScanReport {
    /// Id of the image that was scanned, as the node's daemon reports it.
    pub image_id: String,
    pub scanner: String,
    #[serde(default)]
    pub scanner_version: Option<String>,
    pub findings: Vec<ImageScanFinding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client must outwait the agent: a scan admitted at the end of the
    /// admission window still gets its full run time before the request ends.
    #[test]
    fn request_timeout_covers_admission_plus_the_run() {
        assert!(SCAN_REQUEST_TIMEOUT > SCAN_ADMISSION_DEADLINE + SCAN_RUN_DEADLINE);
    }
}
