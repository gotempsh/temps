// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional managed-control-plane integration for a self-hosted Temps instance.

#![forbid(unsafe_code)]

mod backup_credential_rotation;
mod backup_mirror;
mod handler;
mod plugin;
mod service;

pub use handler::{cloud_routes, CloudApiDoc};
pub use plugin::CloudPlugin;
pub use service::{
    CloudAiCapability, CloudCapability, CloudService, CloudServiceError, CloudStatus,
    ManagedBackupOutcome, ManagedBackupSetup, ManagedBackupSetupAction, ManagedBackupSetupStatus,
};
