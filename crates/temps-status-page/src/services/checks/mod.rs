// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Monitors for targets outside Temps and their check types
//! (ADR 108 D3, D4): HTTP with assertions, TCP port, TLS certificate
//! expiry and DNS records.

pub mod error;
pub mod probe;
pub mod runner;
pub mod service;
pub mod spec;
pub mod types;

#[cfg(test)]
mod tests;

pub use error::{CheckConfigError, TargetMonitorError};
pub use probe::{NetworkProber, ProbeOutcome, ProbeStatus, TargetProber};
pub use runner::TargetChecks;
pub use service::{TargetMonitorService, MAX_TARGET_MONITORS_PER_PROJECT};
pub use spec::{
    CheckRequest, CheckRequestHeader, CheckSpec, DnsCheck, DnsCheckRecordType, HttpCheck,
    HttpMethod, KeywordAssertion, MonitorCheckKind, TcpCheck, TlsCheck,
};
pub use types::{CreateTargetMonitorRequest, MonitorCheckView, UpdateTargetMonitorRequest};
