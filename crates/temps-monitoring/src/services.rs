// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Re-exports and service aggregation for the monitoring crate.
//!
//! The monitoring crate provides three main services:
//! - `AlarmService`: fires, resolves, and queries alarms (the unified alerting backbone)
//! - `ContainerHealthMonitor`: polls Docker containers for restarts, exits, resource spikes
//! - `OutageDetectionService`: detects outages from status checks and bridges to alarms
//! - `DiskSpaceMonitor`: watches disk usage against configurable thresholds
