// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod handlers;
pub mod plugin;
pub mod services;
pub mod utils;

#[allow(ambiguous_glob_reexports)]
pub use handlers::*;
#[allow(ambiguous_glob_reexports)]
pub use services::*;
pub use utils::*;

// Export plugin
pub use plugin::ProjectsPlugin;
