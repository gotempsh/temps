// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scheduled maintenance windows on status pages (ADR 108 D2).
//!
//! A window covers some of a page's monitors for a bounded time. While it
//! is in progress those monitors show "Under maintenance" on the page, their
//! checks are left out of uptime, and the outage detector raises no
//! incident or alert for them. A window is announced once, when it is
//! scheduled, through every channel of the page. It cannot be backdated, so
//! it can never erase an outage already recorded.

pub mod error;
pub mod service;
pub mod types;

pub use error::MaintenanceError;
pub use service::StatusPageMaintenanceService;
pub use types::*;

#[cfg(test)]
mod tests;
