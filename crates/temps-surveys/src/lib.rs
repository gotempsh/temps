// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-app surveys (ADR 110 D1).
//!
//! A survey is a per-project definition: ordered questions and the
//! conditions under which a browser shows it. The analytics browser SDK
//! fetches the surveys a visitor may see from `/api/_temps/surveys`, renders
//! them, and records answers as ordinary analytics events (`survey_shown`,
//! `survey_dismissed`, `survey_sent`). Results are computed from those
//! events, so surveys add no answer store.

pub mod error;
pub mod handlers;
pub mod model;
pub mod plugin;
pub mod selection;
pub mod services;

pub use error::SurveyError;
pub use plugin::SurveysPlugin;
pub use services::SurveyService;
