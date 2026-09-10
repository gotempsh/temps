// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub(crate) mod domain_handler;
pub(crate) mod types;

pub use domain_handler::configure_routes;
pub use types::{create_domain_app_state, create_domain_app_state_with_dns, DomainAppState};
