// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod handler;
pub mod onboarding;
pub mod plugin;
pub mod private_ingress;

pub use handler::{configure_routes, SettingsApiDoc, SettingsState};
pub use plugin::ConfigPlugin;
