// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 1: check what the plugins need before any of them is constructed.

use super::*;

/// Validate the console's hard dependencies before the plugin manager exists:
/// the Docker daemon (fatal only in the `full` profile), the GeoLite2
/// databases and a writable logs directory. Returns the Docker handle every
/// plugin resolves and whether a daemon actually answered.
pub(super) async fn validate_deps(
    profile: crate::commands::serve::ServeProfile,
    config: &ServerConfig,
    db: &Arc<DbConnection>,
    encryption_service: &Arc<EncryptionService>,
) -> anyhow::Result<(Arc<temps_core::DockerHandle>, bool)> {
    // PRE-VALIDATE all plugin dependencies BEFORE initializing plugin manager
    // This ensures clear error messages if any critical resources are missing
    debug!("Pre-validating plugin dependencies...");

    // 1. Validate Docker connectivity.
    //
    // In the `full` profile this is fatal and unchanged: that profile's entire
    // job is running containers here, so a missing daemon is a
    // misconfiguration the operator must see immediately.
    //
    // In `control-plane` the daemon is genuinely optional — the process runs
    // no workloads — so an unreachable one is reported at info level and
    // startup continues with no handle registered. Reachability is probed with
    // a real `ping`, not just client construction: a bollard client is a lazy
    // descriptor that "connects" successfully to a socket that does not exist,
    // and reporting `docker: true` on that basis would be a lie.
    debug!("Checking Docker daemon connectivity...");
    let (docker_handle, docker_available) = if profile.local_workloads_enabled() {
        // `full`: a daemon is this profile's entire job, so a missing one is a
        // misconfiguration the operator must see immediately. Probe it for
        // real rather than trusting client construction — a bollard client is
        // a lazy descriptor that "connects" successfully to a socket path
        // that does not exist, so construction alone proves nothing and
        // reporting `docker: true` on that basis would be a lie the console
        // shows to the operator.
        let client = bollard::Docker::connect_with_defaults()
            .map_err(|e| docker_unavailable_error(&e.to_string()))?;

        match tokio::time::timeout(DOCKER_PROBE_TIMEOUT, client.ping()).await {
            Ok(Ok(_)) => debug!("✓ Docker daemon is accessible"),
            Ok(Err(e)) => return Err(docker_unavailable_error(&e.to_string())),
            Err(_) => {
                return Err(docker_unavailable_error(&format!(
                    "the daemon did not answer a ping within {}s",
                    DOCKER_PROBE_TIMEOUT.as_secs()
                )))
            }
        }

        (temps_core::DockerHandle::available(Arc::new(client)), true)
    } else {
        // `control-plane`: no client is constructed at all. Not "constructed
        // and unused" — constructed. This profile is designed to run in a
        // container with no Docker socket and no DOCKER_HOST, where
        // `connect_with_defaults()` itself fails, and where any probe would
        // only produce a guaranteed error line on every boot. Every
        // daemon-dependent path resolves this handle and fails with a typed
        // `DockerUnavailable` naming the remedy.
        info!(
            profile = profile.as_str(),
            "Local workloads are disabled: no Docker client is constructed and no daemon is \
             contacted. Applications run on worker nodes joined with `temps join`; see \
             GET /api/platform/features"
        );
        (
            temps_core::DockerHandle::disabled(
                profile.as_str(),
                temps_core::CONTROL_PLANE_DOCKER_REASON,
            ),
            false,
        )
    };
    let docker_handle = Arc::new(docker_handle);

    // 2. Validate GeoPlugin dependencies (GeoLite2 database)
    debug!("Checking GeoLite2 database...");
    let geo_db_path = config.data_dir.join("GeoLite2-City.mmdb");
    // The license key is an admin setting on the `settings` row, not an env
    // var, so a startup download uses whatever the admin configured through
    // the UI -- the same value the scheduled refresh job reads each tick.
    let maxmind_license_key = configured_maxmind_license_key(db, encryption_service).await;
    validate_geolite2_database(&geo_db_path, maxmind_license_key.as_deref()).await?;
    debug!("✓ GeoLite2 database file found");
    ensure_geolite2_asn_database(
        &config.data_dir.join(temps_geo::ASN_DB_FILENAME),
        maxmind_license_key.as_deref(),
    )
    .await;

    // 3. Validate logs directory is writable
    debug!("Checking logs directory...");
    let logs_dir = config.data_dir.join("logs");
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        return Err(anyhow::Error::new(
            crate::commands::serve::startup_failure::ConsoleStartupError::LogsDirectory {
                path: logs_dir.clone(),
                data_dir: config.data_dir.clone(),
                reason: e.to_string(),
            },
        ));
    }
    debug!("✓ Logs directory is accessible");

    debug!("✓ All plugin dependencies validated successfully");

    Ok((docker_handle, docker_available))
}

/// How long the `control-plane` profile waits for a Docker ping before
/// deciding the daemon is unavailable. Short on purpose: this runs on the
/// startup path, and "no daemon" is an expected, supported answer here.
const DOCKER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The operator-facing error for "this profile needs Docker and it isn't
/// there". Shared by both failure points so the remediation steps can't drift
/// apart; names the endpoint that was tried and where it came from.
fn docker_unavailable_error(reason: &str) -> anyhow::Error {
    anyhow::Error::new(
        crate::commands::serve::startup_failure::ConsoleStartupError::DockerUnavailable {
            endpoint: crate::docker_context::docker_endpoint().describe(),
            reason: reason.to_string(),
        },
    )
}

