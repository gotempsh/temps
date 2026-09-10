// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod handlers;
pub mod plugin;
pub mod services;
pub mod types;

// Re-export main types
pub use plugin::EventsPlugin;
pub use services::*;
pub use types::*;
