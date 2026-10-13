// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed console startup errors and their operator-facing summary.
//!
//! `start_console_api` returns `anyhow::Result` because it glues together
//! dozens of plugin and service errors. The checks an operator can actually
//! fix on a first run (Docker, GeoLite2, logs directory, log storage, the
//! initial admin, the listener address) raise a typed error from this module
//! instead of a formatted string, so [`summarize`] can recover *which* check
//! failed from the `anyhow` chain and pair it with concrete remediation. The
//! summary is recorded in the shared
//! [`ConsoleStartupState`](temps_core::console_startup::ConsoleStartupState),
//! where the proxy serves it as the console's status page.
//!
//! The `Display` text of each variant is what `temps serve` logs, so the log
//! keeps its full multi-line guidance.

use std::path::PathBuf;

use temps_core::console_startup::{ConsoleStartupCheck, ConsoleStartupFailure};

use super::console::{InitialAdminConfigError, LogStorageConfigError};

/// A console startup check failed.
#[derive(Debug, thiserror::Error)]
pub enum ConsoleStartupError {
    #[error("{}", docker_message(.endpoint, .reason))]
    DockerUnavailable {
        /// Endpoint description, e.g. `unix:///var/run/docker.sock (from DOCKER_HOST)`.
        endpoint: String,
        reason: String,
    },

    #[error("{message}")]
    GeoDatabase {
        /// Where the database was looked for, in order.
        checked: Vec<PathBuf>,
        /// Why the automatic download failed.
        reason: String,
        /// Full operator guidance, logged as-is.
        message: String,
    },

    #[error("{}", logs_directory_message(.path, .data_dir, .reason))]
    LogsDirectory {
        path: PathBuf,
        data_dir: PathBuf,
        reason: String,
    },

    #[error("❌ Log storage configuration is invalid\n\n{source}")]
    LogStorage {
        #[source]
        source: LogStorageConfigError,
    },

    #[error(
        "No users exist and no admin email was provided. Set TEMPS_ADMIN_EMAIL and \
         TEMPS_ADMIN_PASSWORD_FILE, or run `temps serve` interactively once to enter it"
    )]
    AdminEmailRequired,

    #[error("Plugin initialization failed: {reason}")]
    PluginInitialization { reason: String },

    #[error("Console listener could not bind {address}: {source}")]
    ListenerBind {
        address: String,
        #[source]
        source: std::io::Error,
    },
}

fn docker_message(endpoint: &str, reason: &str) -> String {
    format!(
        "❌ Docker dependency check FAILED\n\n\
        The system requires Docker to be running and accessible.\n\n\
        Docker endpoint: {endpoint}\n\
        Error details: {reason}\n\n\
        Solutions:\n\
        1. Ensure Docker daemon is running\n\
           - macOS: Check Docker Desktop application\n\
           - Linux: Run 'sudo systemctl start docker'\n\n\
        2. Verify Docker socket permissions\n\
           - Linux: Run 'sudo usermod -aG docker $USER'\n\n\
        3. Check Docker environment variables\n\
           - DOCKER_HOST may need to be set\n\n\
        4. Run this control plane without local workloads\n\
           - `temps serve --profile control-plane` needs no Docker daemon; \
             applications then run on worker nodes joined with `temps join`\n\n\
        Deployment features will not be available until Docker is accessible."
    )
}

fn logs_directory_message(
    path: &std::path::Path,
    data_dir: &std::path::Path,
    reason: &str,
) -> String {
    format!(
        "❌ Logs directory creation FAILED\n\n\
        Cannot create or access the logs directory.\n\n\
        Path: {}\n\
        Error: {}\n\n\
        Solutions:\n\
        1. Check directory permissions\n\
           - Ensure write permissions to parent directory: {}\n\n\
        2. Verify disk space\n\
           - Run: df -h\n\n\
        3. Check file ownership\n\
           - Run: ls -la {}\n\n\
        Logs are required for system diagnostics and operation tracking.",
        path.display(),
        reason,
        data_dir.display(),
        data_dir.display()
    )
}

/// Turn a console startup error into the summary shown on the status page.
///
/// Walks the whole `anyhow` chain so a typed error wrapped by a caller is
/// still recognised. Anything unrecognised is reported under
/// [`ConsoleStartupCheck::Other`] with its first lines as the detail.
pub fn summarize(error: &anyhow::Error) -> ConsoleStartupFailure {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<ConsoleStartupError>() {
            return summarize_typed(error);
        }
        if let Some(error) = cause.downcast_ref::<InitialAdminConfigError>() {
            return summarize_initial_admin(error);
        }
        if let Some(error) = cause.downcast_ref::<LogStorageConfigError>() {
            return summarize_log_storage(error);
        }
    }
    ConsoleStartupFailure::new(
        ConsoleStartupCheck::Other,
        "The console hit an error during startup",
        first_lines(&error.to_string(), 12),
        vec![
            "Read the full error in the server log; the message above is its beginning."
                .to_string(),
            "Run `temps doctor` on the server to check Docker, the database and migrations."
                .to_string(),
            "Restart `temps serve` after fixing the cause.".to_string(),
        ],
    )
}

fn summarize_typed(error: &ConsoleStartupError) -> ConsoleStartupFailure {
    match error {
        ConsoleStartupError::DockerUnavailable { endpoint, reason } => ConsoleStartupFailure::new(
            ConsoleStartupCheck::Docker,
            format!("Docker is not reachable at {endpoint}"),
            reason,
            vec![
                "Start the Docker daemon (Docker Desktop on macOS, `sudo systemctl start docker` on Linux).".to_string(),
                "If Docker runs elsewhere (Colima, OrbStack, rootless Docker), set DOCKER_HOST to its socket, e.g. DOCKER_HOST=unix:///path/to/docker.sock, or select it with `docker context use <name>`.".to_string(),
                "On Linux, make sure the user running Temps can use the socket: `sudo usermod -aG docker <user>`, then log in again.".to_string(),
                "To run this server without local workloads, start it with `temps serve --profile control-plane` and add worker nodes with `temps join`.".to_string(),
                "Check with `temps doctor`, then restart `temps serve`.".to_string(),
            ],
        ),
        ConsoleStartupError::GeoDatabase { checked, reason, .. } => {
            let locations = checked
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            ConsoleStartupFailure::new(
                ConsoleStartupCheck::GeoDatabase,
                "GeoLite2-City.mmdb is missing and could not be downloaded",
                format!("Checked: {locations}. Download error: {reason}"),
                vec![
                    "Check that this server can reach the internet over HTTPS; the database is downloaded automatically on startup.".to_string(),
                    "Or download GeoLite2-City from MaxMind (free account) and copy GeoLite2-City.mmdb into the data directory.".to_string(),
                    "Restart `temps serve`.".to_string(),
                ],
            )
        }
        ConsoleStartupError::LogsDirectory {
            path,
            data_dir,
            reason,
        } => ConsoleStartupFailure::new(
            ConsoleStartupCheck::LogsDirectory,
            format!("The logs directory {} cannot be created", path.display()),
            reason,
            vec![
                format!(
                    "Make the data directory {} writable by the user running Temps.",
                    data_dir.display()
                ),
                "Check free disk space with `df -h`.".to_string(),
                "Restart `temps serve`.".to_string(),
            ],
        ),
        ConsoleStartupError::LogStorage { source } => summarize_log_storage(source),
        ConsoleStartupError::AdminEmailRequired => ConsoleStartupFailure::new(
            ConsoleStartupCheck::InitialAdmin,
            "No admin account exists and no admin email was provided",
            error.to_string(),
            vec![
                "Set TEMPS_ADMIN_EMAIL and TEMPS_ADMIN_PASSWORD_FILE (a file containing the password) in the environment of `temps serve`.".to_string(),
                "Or run `temps serve` once in an interactive terminal and enter the admin email when prompted.".to_string(),
            ],
        ),
        ConsoleStartupError::PluginInitialization { reason } => ConsoleStartupFailure::new(
            ConsoleStartupCheck::PluginInitialization,
            "A console plugin failed to initialize",
            first_lines(reason, 12),
            vec![
                "Read the plugin error above and the lines before \"Plugin initialization FAILED\" in the server log.".to_string(),
                "Run `temps doctor` to check the database, migrations and Docker.".to_string(),
                "Restart `temps serve` after fixing the cause.".to_string(),
            ],
        ),
        ConsoleStartupError::ListenerBind { address, source } => ConsoleStartupFailure::new(
            ConsoleStartupCheck::Listener,
            format!("The console could not listen on {address}"),
            source.to_string(),
            vec![
                format!(
                    "Make sure nothing else is listening on {address} (for example `ss -ltnp` or `lsof -i -P | grep LISTEN`)."
                ),
                "Or choose a free address with --console-address / TEMPS_CONSOLE_ADDRESS.".to_string(),
                "Restart `temps serve`.".to_string(),
            ],
        ),
    }
}

fn summarize_initial_admin(error: &InitialAdminConfigError) -> ConsoleStartupFailure {
    let remediation: Vec<String> = match error {
        InitialAdminConfigError::InvalidEmail => vec![
            "Set TEMPS_ADMIN_EMAIL to a valid email address.".to_string(),
        ],
        InitialAdminConfigError::IncompleteCredentials => vec![
            "Set both TEMPS_ADMIN_EMAIL and TEMPS_ADMIN_PASSWORD_FILE, or neither (to be prompted in an interactive terminal).".to_string(),
        ],
        InitialAdminConfigError::ReadPasswordFile { .. } => vec![
            "Make sure TEMPS_ADMIN_PASSWORD_FILE points at a file that exists and is readable by the user running Temps.".to_string(),
        ],
        InitialAdminConfigError::InvalidPassword { .. } => vec![
            "Put a password of 8 to 128 characters with at least one uppercase letter, one lowercase letter, one digit and one special character in the TEMPS_ADMIN_PASSWORD_FILE file.".to_string(),
        ],
        InitialAdminConfigError::DeletedUser { .. } => vec![
            "Restore the soft-deleted user, or set TEMPS_ADMIN_EMAIL to a different address.".to_string(),
        ],
        InitialAdminConfigError::InvalidEnvironment { name, .. } => vec![format!(
            "Set {name} to a valid UTF-8 value."
        )],
    };
    ConsoleStartupFailure::new(
        ConsoleStartupCheck::InitialAdmin,
        "The initial admin account could not be created",
        error.to_string(),
        remediation
            .into_iter()
            .chain(std::iter::once("Restart `temps serve`.".to_string())),
    )
}

fn summarize_log_storage(error: &LogStorageConfigError) -> ConsoleStartupFailure {
    ConsoleStartupFailure::new(
        ConsoleStartupCheck::LogStorage,
        "The log storage configuration is invalid",
        error.to_string(),
        vec![
            "Set every TEMPS_LOG_S3_* variable, or unset TEMPS_LOG_STORAGE_BACKEND to keep logs on local disk.".to_string(),
            "Restart `temps serve`.".to_string(),
        ],
    )
}

/// The first `max` non-empty lines, which carry the cause; the rest of a
/// startup error is usually generic guidance already covered by remediation.
fn first_lines(text: &str, max: usize) -> String {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(max)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_failure_names_endpoint_and_remediation() {
        let error = anyhow::Error::new(ConsoleStartupError::DockerUnavailable {
            endpoint: "unix:///tmp/nonexistent.sock (from DOCKER_HOST)".to_string(),
            reason: "Socket not found: /tmp/nonexistent.sock".to_string(),
        });
        // The log keeps the full guidance plus the endpoint.
        let logged = error.to_string();
        assert!(logged.contains("Docker dependency check FAILED"));
        assert!(logged.contains("Docker endpoint: unix:///tmp/nonexistent.sock"));

        let failure = summarize(&error);
        assert_eq!(failure.check, ConsoleStartupCheck::Docker);
        assert!(failure.summary.contains("unix:///tmp/nonexistent.sock"));
        assert_eq!(failure.detail, "Socket not found: /tmp/nonexistent.sock");
        assert!(failure
            .remediation
            .iter()
            .any(|step| step.contains("DOCKER_HOST")));
    }

    #[test]
    fn invalid_admin_password_is_classified_without_leaking_it() {
        let error: anyhow::Error = InitialAdminConfigError::InvalidPassword {
            path: PathBuf::from("/run/secrets/admin-password"),
            reason: "Password must contain at least one digit".to_string(),
        }
        .into();
        let failure = summarize(&error);
        assert_eq!(failure.check, ConsoleStartupCheck::InitialAdmin);
        assert!(failure.detail.contains("/run/secrets/admin-password"));
        assert!(failure.detail.contains("at least one digit"));
        assert!(failure.remediation[0].contains("special character"));
    }

    #[test]
    fn typed_errors_are_found_behind_wrapping() {
        #[derive(Debug, thiserror::Error)]
        #[error("console listener")]
        struct Wrapper(#[source] ConsoleStartupError);

        let wrapped = anyhow::Error::new(Wrapper(ConsoleStartupError::ListenerBind {
            address: "127.0.0.1:8081".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::AddrInUse),
        }));
        let failure = summarize(&wrapped);
        assert_eq!(failure.check, ConsoleStartupCheck::Listener);
        assert!(failure.summary.contains("127.0.0.1:8081"));
    }

    #[test]
    fn log_storage_error_keeps_its_type() {
        let error = anyhow::Error::new(ConsoleStartupError::LogStorage {
            source: LogStorageConfigError::MissingS3Variable {
                variable: "TEMPS_LOG_S3_BUCKET",
            },
        });
        assert!(error
            .to_string()
            .starts_with("❌ Log storage configuration is invalid"));
        let failure = summarize(&error);
        assert_eq!(failure.check, ConsoleStartupCheck::LogStorage);
        assert!(failure.detail.contains("TEMPS_LOG_S3_BUCKET"));
    }

    #[test]
    fn unknown_errors_fall_back_with_redacted_first_lines() {
        let error = anyhow::anyhow!(
            "Failed to connect to postgres://temps:hunter2@db:5432/temps\n\nmore\n{}",
            "line\n".repeat(40)
        );
        let failure = summarize(&error);
        assert_eq!(failure.check, ConsoleStartupCheck::Other);
        assert!(!failure.detail.contains("hunter2"));
        assert!(failure.detail.contains("postgres://***@db:5432/temps"));
        assert_eq!(failure.detail.lines().count(), 12);
    }

    #[test]
    fn plugin_failure_is_classified() {
        let error = anyhow::Error::new(ConsoleStartupError::PluginInitialization {
            reason: "service 'Foo' failed: boom".to_string(),
        });
        let failure = summarize(&error);
        assert_eq!(failure.check, ConsoleStartupCheck::PluginInitialization);
        assert!(failure.detail.contains("boom"));
    }
}
