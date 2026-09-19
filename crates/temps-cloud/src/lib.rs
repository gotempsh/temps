// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional managed-control-plane integration for a self-hosted Temps instance.

#![forbid(unsafe_code)]

mod backup_credential_rotation;
mod backup_mirror;
/// ADR-045 §4: the `ConsoleOidcSink` adapter provisioning the managed
/// console-access OIDC provider. See the module doc for the wiring point
/// once `temps-cloud-client`'s console-proxy worker exists.
pub mod console_oidc;
mod handler;
mod lifecycle_notify;
mod plugin;
mod service;

pub use console_oidc::ConsoleOidcAdapter;
pub use handler::{
    cloud_routes, record_backup_outcome_audit, record_console_access_default_enabled_audit,
    record_enrollment_audit, record_link_connected_audit, CloudApiDoc, CloudEnrollmentActor,
    UNATTENDED_ENROLLMENT_USER_AGENT,
};
pub use plugin::CloudPlugin;
pub use service::{
    CloudAiCapability, CloudCapability, CloudService, CloudServiceError, CloudStatus,
    ManagedBackupOutcome, ManagedBackupSetup, ManagedBackupSetupAction, ManagedBackupSetupStatus,
};
