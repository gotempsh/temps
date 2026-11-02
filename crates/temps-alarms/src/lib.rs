// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Alarm lifecycle: firing, resolving, silencing and querying alarms.
//!
//! Split out of `temps-monitoring` so that crates which only raise alarms do
//! not inherit its container-runtime dependency. `temps-monitoring` re-exports
//! everything here and keeps the HTTP handlers and the plugin that registers
//! [`AlarmService`].

pub mod alarm_service;

pub use alarm_service::*;
