// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Monitors for targets outside Temps and their check types
//! (ADR 108 D3, D4): HTTP with assertions, TCP port, TLS certificate
//! expiry and DNS records.

pub mod error;
pub mod locations;
pub mod runner;
pub mod service;
pub mod types;

/// The specs and the probe live in `temps-uptime-probe`, which worker node
/// agents share, so a check judges the same way from every location.
pub use temps_uptime_probe::{probe, spec};

#[cfg(test)]
mod tests;

pub use error::{CheckConfigError, TargetMonitorError};
pub use locations::{
    MonitorLocationResult, MonitorLocationResults, NoNodeLocations, NodeProbeLocations,
    ProbeLocations,
};
pub use probe::{NetworkProber, ProbeOutcome, ProbeStatus, TargetProber};
pub use runner::{CheckResult, TargetChecks};
pub use service::{TargetMonitorService, MAX_TARGET_MONITORS_PER_PROJECT};
pub use spec::{
    CheckRequest, CheckRequestHeader, CheckSpec, DnsCheck, DnsCheckRecordType, HttpCheck,
    HttpMethod, KeywordAssertion, MonitorCheckKind, TcpCheck, TlsCheck,
};
pub use types::{
    CreateTargetMonitorRequest, MonitorCheckView, ProbeLocationView, ProbeLocationsResponse,
    UpdateTargetMonitorRequest,
};
