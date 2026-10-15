// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub mod agent;
pub mod agent_service;
pub mod api_key;
mod api_url;
pub mod backfill;
pub mod backup;
pub mod build;
pub mod ch_backfill_domains;
pub mod cloud_telemetry_backfill;
pub mod data_dir_discovery;
pub mod deploy;
pub mod doctor;
pub mod domain;
pub mod edge;
pub mod firecracker;
pub mod join;
pub mod microsandbox;
pub mod migrate;
pub mod network;
pub mod node;
pub mod proxy;
pub mod reset_password;
pub mod sandbox;
pub mod schema_upgrade;
pub mod serve;
pub mod services;
pub mod setup;
pub mod upgrade;

/// Value parser for boolean flags that can also be set from an environment
/// variable (`TEMPS_DISABLE_SELF_UPDATE`, `TEMPS_PRE_MIGRATION_BACKUP`).
///
/// clap's default for a plain `bool` flag treats every unrecognised
/// non-empty value as `true`, so `TEMPS_PRE_MIGRATION_BACKUP=disabled` would
/// silently turn the feature *on*. These settings gate destructive or
/// safety-critical behaviour, so anything that is not clearly a boolean is
/// rejected at startup instead. An empty value counts as `false`, which is
/// what `VAR=` and compose's `${VAR:-}` produce for "not set".
pub(crate) fn parse_bool_setting(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" | "n" | "f" => Ok(false),
        "1" | "true" | "yes" | "on" | "y" | "t" => Ok(true),
        _ => Err(format!(
            "'{raw}' is not a boolean; use true or false (also accepted: 1, 0, yes, no, on, off)"
        )),
    }
}

pub use agent::AgentCommand;
pub use api_key::ApiKeyCommand;
pub use backfill::BackfillCommand;
pub use backup::BackupCommand;
pub use build::BuildCommand;
pub use deploy::DeployCommand;
pub use doctor::DoctorCommand;
pub use domain::DomainCommand;
pub use edge::EdgeCommand;
pub use firecracker::FirecrackerCommand;
pub use join::JoinCommand;
pub use microsandbox::MicrosandboxCommand;
pub use migrate::MigrateCommand;
pub use network::NetworkCommand;
pub use node::NodeCommand;
pub use proxy::ProxyCommand;
pub use reset_password::ResetPasswordCommand;
pub use sandbox::SandboxCommand;
pub use serve::ServeCommand;
pub use services::ServicesCommand;
pub use setup::SetupCommand;
pub use upgrade::UpgradeCommand;
