// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod audit;
pub mod custom_domains;
#[allow(clippy::module_inception)]
mod handlers;
mod preset_configs;
pub mod templates;
mod types;

pub use custom_domains::CustomDomainsApiDoc;
pub use handlers::*;
pub use preset_configs::*;
pub use types::*;
