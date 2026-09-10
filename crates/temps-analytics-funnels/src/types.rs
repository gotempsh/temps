// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Re-export common types from the services module
//!
//! This module provides a single entry point for the public API types.

// Re-export service layer domain types
pub use crate::services::{
    CreateFunnelRequest, CreateFunnelStep, FunnelFilter, FunnelMetrics, SmartFilter, StepConversion,
};

// Re-export handler layer HTTP types
pub use crate::handlers::types::{
    CreateFunnelResponse, EventType, EventTypesResponse, FunnelMetricsResponse, FunnelResponse,
    GetFunnelMetricsQuery, StepConversionResponse,
};
