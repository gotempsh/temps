// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod approval;
mod audit;
mod handler;
pub use handler::{configure_routes, ServiceAccessApiDoc};
use std::sync::Arc;
pub struct AccessAppState {
    pub coordinator: Arc<crate::coordinator::ServiceAccessCoordinator>,
    pub service: Arc<crate::service::ServiceAccessService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Optional organization/team authorization extension, absent in OSS.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

mod vaults;

mod assignments;
mod sessions;

mod general_vaults;

mod links;

mod general_sessions;

mod general_services;
