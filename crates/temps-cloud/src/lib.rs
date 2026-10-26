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
mod console_oidc_bootstrap;
mod handler;
mod lifecycle_notify;
mod plugin;
mod service;

pub use console_oidc::ConsoleOidcAdapter;
pub use console_oidc_bootstrap::{
    parse_console_oidc_bootstrap_file, ConsentTrace, ConsoleAccessConsent, ConsoleOidcApplyFailure,
    ConsoleOidcBootstrapError, ConsoleOidcBootstrapOutcome, ConsoleOidcBootstrapParseError,
    TelemetryConsent, CONSOLE_OIDC_BOOTSTRAP_FILENAME,
};
pub use handler::{
    cloud_routes, record_backend_url_bootstrapped_audit, record_backup_outcome_audit,
    record_console_oidc_bootstrap_failure_audit, record_console_oidc_bootstrap_outcome_audit,
    record_enrollment_audit, record_link_connected_audit, record_telemetry_bootstrap_audit,
    CloudApiDoc, CloudEnrollmentActor, CLOUD_CONSOLE_ACCESS_BOOTSTRAP_FAILED,
    CLOUD_CONSOLE_ACCESS_ENABLED_BY_BOOTSTRAP, CLOUD_CONSOLE_OIDC_BOOTSTRAPPED,
    CLOUD_TELEMETRY_ENABLED_BY_BOOTSTRAP, UNATTENDED_ENROLLMENT_USER_AGENT,
};
pub use plugin::CloudPlugin;
pub use service::{
    BootstrapBackendUrlOutcome, CloudAiCapability, CloudCapability, CloudService,
    CloudServiceError, CloudStatus, ManagedBackupOutcome, ManagedBackupSetup,
    ManagedBackupSetupAction, ManagedBackupSetupStatus,
};
