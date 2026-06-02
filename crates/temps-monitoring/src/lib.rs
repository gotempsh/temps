//! Monitoring services and utilities
//!
//! This crate provides system monitoring capabilities including:
//! - Alarm service for firing, resolving, and querying alarms
//! - Container health monitoring (restart detection, resource usage)
//! - Disk space monitoring with configurable thresholds
//! - Outage detection and notification for status monitors
//! - Alert evaluator (metrics threshold alerts via MetricsStore)

pub mod alarm_service;
pub mod container_health;
pub mod disk_space;
pub mod evaluator;
pub mod outage;
pub mod services;

pub use alarm_service::*;
pub use container_health::*;
pub use disk_space::*;
pub use evaluator::{seed_default_container_rules, seed_default_rules, AlertEvaluator};
pub use outage::*;
// services module is documentation-only, no re-exports needed
