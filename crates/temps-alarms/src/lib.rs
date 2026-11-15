// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Alarm lifecycle (firing, resolving, silencing, querying) and the built-in
//! alert rule sets.
//!
//! Split out of `temps-monitoring` so that crates which only raise alarms do
//! not inherit its container-runtime dependency. `temps-monitoring` re-exports
//! everything here and keeps the HTTP handlers and the plugin that registers
//! [`AlarmService`].

pub mod alarm_service;
pub mod default_rules;

pub use alarm_service::*;
pub use default_rules::{
    seed_default_container_rules, seed_default_rules, DefaultRuleSeedError,
    MAX_ALERT_RULES_PER_SERVICE,
};
