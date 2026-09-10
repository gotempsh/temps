// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Funnels analytics module
//!
//! Provides funnel analysis capabilities for tracking user conversion paths.

pub mod handlers;
pub mod plugin;
pub mod services;
pub mod types;

// Re-export plugin
pub use plugin::FunnelsPlugin;
