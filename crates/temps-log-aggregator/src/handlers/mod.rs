// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub(crate) mod log_handler;
pub(crate) mod types;

pub use log_handler::configure_routes;
pub use log_handler::LogAggregatorApiDoc;
pub use types::{create_log_aggregator_app_state, LogAggregatorAppState};
