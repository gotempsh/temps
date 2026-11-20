// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::spec::{CheckRequest, CheckSpec, MonitorCheckKind, StoredCheck};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTargetMonitorRequest {
    /// Shown in the console and, unless renamed there, on status pages.
    pub name: String,
    /// What to check: a URL (`http`), `host:port` (`tcp`, `tls`; `tls`
    /// defaults to 443) or a host name (`dns`). It must have a public
    /// address.
    pub target: String,
    pub check: CheckSpec,
    /// Headers and body of an HTTP check. Stored encrypted; header values
    /// are never returned.
    #[serde(default)]
    pub request: Option<CheckRequest>,
    /// How often to check, in seconds (default 60, at least 30).
    #[serde(default)]
    pub check_interval_seconds: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateTargetMonitorRequest {
    pub name: Option<String>,
    pub target: Option<String>,
    /// Replaces the check, and may change its kind; the target must fit.
    pub check: Option<CheckSpec>,
    /// Replaces every header and the body. Omit to keep them.
    pub request: Option<CheckRequest>,
    pub check_interval_seconds: Option<i32>,
    /// `false` pauses the monitor.
    pub is_active: Option<bool>,
}

/// How a monitor outside Temps is checked. Header values are not included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MonitorCheckView {
    pub target: String,
    pub check: CheckSpec,
    /// Names of the request headers sent; their values are stored
    /// encrypted.
    pub header_names: Vec<String>,
    pub has_body: bool,
}

impl MonitorCheckView {
    /// The view of a monitor row, `None` for an environment monitor or an
    /// unreadable spec.
    pub fn from_row(
        kind: MonitorCheckKind,
        target: Option<&str>,
        spec: Option<&serde_json::Value>,
    ) -> Option<Self> {
        if kind == MonitorCheckKind::Environment {
            return None;
        }
        let stored: StoredCheck = serde_json::from_value(spec?.clone()).ok()?;
        Some(Self {
            target: target?.to_string(),
            check: stored.spec,
            header_names: stored.header_names,
            has_body: stored.has_body,
        })
    }
}
