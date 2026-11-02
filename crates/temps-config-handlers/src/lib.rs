// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod handler;
pub mod plugin;

pub use handler::{configure_routes, SettingsApiDoc, SettingsState};
pub use plugin::ConfigPlugin;
