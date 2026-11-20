// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversion goals (ADR 109 D2), shown as "Conversions" in the console.
//!
//! A goal says which events count as a conversion: an event by name, with the
//! same filters funnel steps use, or a page view by path pattern. Goals are
//! read-time rules over `events`; the ingest path never consults them.

pub mod handlers;
pub(crate) mod query;
pub mod service;
pub mod types;

pub use handlers::{routes, AnalyticsGoalsApiDoc, AnalyticsGoalsState};
pub use service::AnalyticsGoalService;
pub use types::*;
