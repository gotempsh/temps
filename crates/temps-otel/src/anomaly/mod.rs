//! Anomaly detection and insight generation.
//!
//! Runs every 60 seconds, computing Z-scores per metric per service/environment
//! with time-aware baselines (hour-of-day, day-of-week) using a 14-day lookback.

pub mod detector;
pub mod insights;

pub use detector::AnomalyDetector;
pub use insights::InsightGenerator;
