// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Uptime check specs and the probe that runs them against public targets
//! (ADR 108 D3, D4), shared by the control plane and worker node agents so a
//! check judges the same way from every location (D5). No database here.

pub mod error;
pub mod outbound;
pub mod probe;
pub mod spec;
pub mod wire;

pub use error::{CheckConfigError, OutboundError};
pub use probe::{NetworkProber, ProbeOutcome, ProbeStatus, TargetProber};
pub use wire::{RemoteProbeRequest, REMOTE_PROBE_PATH};
