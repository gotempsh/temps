// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod plugin;
pub mod routes;
pub mod services;

#[cfg(test)]
mod tests;

pub use plugin::StatusPagePlugin;
pub use services::{
    CreateIncidentRequest, CreateMonitorRequest, IncidentResponse, IncidentService,
    MonitorResponse, MonitorService, StatusPageError, StatusPageOverview, StatusPageService,
};
