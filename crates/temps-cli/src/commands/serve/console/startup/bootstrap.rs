// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 5: first-boot work that needs the populated service registry:
//! telemetry, the initial admin, host git credentials, console SSO and the
//! unattended Cloud enrollment.

use super::*;

/// What later steps keep from [`bootstrap_after_init`].
pub(super) struct PostInit {
    /// Kept for `complete_startup`, which runs only once the console listens.
    pub(super) startup_reporter: Option<Arc<dyn temps_core::telemetry::TelemetryReporter>>,
    /// The in-flight Cloud enrollment, joined (bounded) at shutdown.
    pub(super) enrollment_bootstrap: Option<tokio::task::JoinHandle<()>>,
}

/// Run the post-initialisation bootstrap.
pub(super) async fn bootstrap_after_init(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    config: &ServerConfig,
) -> anyhow::Result<PostInit> {
    // Check if any users exist, if not prompt for admin email
    // Kept for `complete_startup`, which runs only once the console listens.
    let startup_reporter =
        service_context.get_service::<dyn temps_core::telemetry::TelemetryReporter>();

    // Emit the anonymous `instance_started` telemetry event now that the
    // service registry is populated. Entirely best-effort: a missing reporter,
    // a failed count query, or a dead endpoint must never affect startup.
    if let Some(reporter) =
        service_context.get_service::<dyn temps_core::telemetry::TelemetryReporter>()
    {
        if reporter.is_enabled() {
            report_instance_started(reporter.as_ref(), db.as_ref()).await;
        }
        // Telemetry can be switched on or off at runtime from Settings >
        // Telemetry, so the periodic tasks always run and every tick checks the
        // current state: nothing is built or sent while it is off.
        //
        // Keep "active instances" honest: a daily heartbeat so a live-but-idle
        // instance still checks in even when it isn't deploying.
        spawn_heartbeat_task(reporter.clone(), db.clone());
        // Periodic aggregated error_summary flush (ERROR logs / console
        // 5xx / panics — counts only, never messages). The counters are
        // bounded in-process memory and are drained every tick either way.
        spawn_error_summary_task(reporter.clone());
    }
    if let Some(user_service) = service_context.get_service::<temps_auth::UserService>() {
        // Always ensure the system user (id=0) exists — needed for webhook-created
        // resources (e.g., GitHub App installations) that reference user_id=0
        ensure_system_user(db.as_ref()).await?;

        let users = user_service
            .get_all_users(false) // Don't include deleted users
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get users: {}", e))?;

        if users.is_empty() {
            debug!("No users found, prompting for admin email");

            // Initialize roles first to ensure they exist
            user_service
                .initialize_roles()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to initialize roles: {}", e))?;
            debug!("Initialized user roles");

            let configured_email = optional_environment_variable("TEMPS_ADMIN_EMAIL")?;
            let configured_password_file =
                optional_environment_variable("TEMPS_ADMIN_PASSWORD_FILE")?;
            if let Some((admin_email, admin_password)) = configured_initial_admin(
                configured_email.as_deref(),
                configured_password_file.as_deref(),
            )? {
                info!("Creating initial admin from configured email and password secret file");
                create_initial_admin_user(db.as_ref(), &admin_email, Some(&admin_password)).await?;
            } else if let Some(admin_email) = prompt_for_admin_email()? {
                create_initial_admin_user(db.as_ref(), &admin_email, None).await?;
            } else {
                return Err(crate::commands::serve::startup_failure::ConsoleStartupError::AdminEmailRequired.into());
            }
        }
    } else {
        debug!("UserService not available, skipping user initialization");
    }

    if let Some(manager) = service_context.get_service::<temps_git::GitProviderManager>() {
        let db = service_context.require_service::<temps_database::DbConnection>();
        let encryption = service_context.require_service::<temps_core::EncryptionService>();
        if let Err(error) =
            temps_git::services::host_import::import_host_credentials(db, encryption, manager).await
        {
            tracing::warn!(event = "host_git_bootstrap_failed", error = %error);
        }
    }

    // Cloud console-access SSO bootstrap from `<TEMPS_DATA_DIR>/cloud-oidc.json`
    // (ADR-045 §4; see the doc comment on `bootstrap_console_oidc_from_file`).
    // Runs, and is awaited, before the unattended enrollment bootstrap below:
    // the managed SSO provider must be in place by the time the console is
    // reachable, and unlike enrollment this does no network I/O of its own,
    // so there is nothing to gain by deferring it.
    bootstrap_console_oidc_from_file(service_context, &config.data_dir).await;

    // Unattended Temps Cloud enrollment via TEMPS_CLOUD_ENROLLMENT_CODE (see
    // the doc comment on `TEMPS_CLOUD_ENROLLMENT_CODE_VAR` above; ADR 0040 in
    // the Cloud repo). Deliberately not nested inside the `users.is_empty()`
    // block above: idempotency comes from the already-linked check inside
    // `bootstrap_cloud_enrollment_from_env`, not from whether this was the
    // instance's very first boot, so a restart with a leftover env var is
    // always safe. Best-effort and off the startup critical path: this spawns
    // the attempt and returns immediately, so listener binding below never
    // waits on the Cloud backend. The handle is joined (bounded) by the
    // graceful-shutdown future further down.
    let enrollment_bootstrap = bootstrap_cloud_enrollment_from_env(service_context);

    Ok(PostInit {
        startup_reporter,
        enrollment_bootstrap,
    })
}
