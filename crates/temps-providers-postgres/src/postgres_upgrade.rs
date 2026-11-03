// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL major version upgrade orchestrator
//!
//! CNPG-inspired declarative major upgrades for Postgres external services
//! managed by Temps. The process is a phase-driven state machine that is
//! resumable on failure and retains the pre-upgrade volume for 7 days for
//! rollback.
//!
//! Phases:
//! 1. `snapshot`       — rename old volume to `{container}_data_rollback_{ts}`,
//!    create fresh empty volume under original name
//! 2. `dump`           — pg_dumpall from old container into a named volume
//! 3. `new_container`  — create new-version container mounted on fresh volume
//! 4. `restore`        — psql restore from dump volume into new container
//! 5. `swap`           — rename/retarget container to final name; start it
//! 6. `analyze`        — run `ANALYZE` to refresh planner stats
//! 7. `completed`      — terminal success
//!
//! On any phase error, the row is marked `failed` with `phase` set to the
//! last attempted step and `error_message` populated. The user can retry,
//! which resumes from the current phase without redoing earlier phases.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bollard::Docker;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use temps_core::telemetry::{NoopTelemetryReporter, TelemetryReporter};
use temps_deployment_logs::LogService;
use temps_entities::postgres_major_upgrades;
use thiserror::Error;

use super::postgres::{shell_escape, validate_pg_username};

/// Pre-upgrade backup provider trait.
///
/// Implemented by `temps-backup::BackupService` to avoid a circular dep
/// between `temps-providers` and `temps-backup`. The orchestrator invokes
/// this before dropping the old volume so the user always has a recovery
/// path independent of the rollback volume.
#[async_trait]
pub trait PreUpgradeBackupProvider: Send + Sync {
    /// Resolve the default S3 source id for the given service. Returns
    /// `None` if the user has not marked any source as default — in which
    /// case the upgrade must be rejected at start time.
    async fn default_s3_source_id(&self, service_id: i32) -> Result<Option<i32>, String>;

    /// Trigger a full backup for the given service using the supplied S3 source.
    /// Returns the resulting `backups.id` (DB primary key) on success.
    async fn create_pre_upgrade_backup(
        &self,
        service_id: i32,
        s3_source_id: i32,
        created_by: i32,
    ) -> Result<i32, String>;
}

/// Postgres container lifecycle trait — the upgrade orchestrator's view of
/// "how to run a Postgres container for this service". Mirrors the
/// `PreUpgradeBackupProvider` pattern so the orchestrator stays decoupled
/// from the full `PostgresService` surface and its state-holding fields.
///
/// Implemented by `PostgresLifecycleAdapter` which reads
/// `external_services.parameters` for connection details and persists
/// image/port changes back to the DB during swap.
///
/// Every method is keyed by `service_id`: the implementation is expected
/// to look up the Postgres service config at call time so a mid-upgrade
/// reconfigure (e.g., user changes `docker_image` manually) is picked up.
#[async_trait]
pub trait PostgresContainerLifecycle: Send + Sync {
    /// Canonical container name for the given service. Matches the naming
    /// formula used by `PostgresService` (`postgres-{service.name}`).
    async fn container_name(&self, service_id: i32) -> Result<String, String>;

    /// Load connection params from `external_services.parameters`. Returns
    /// a minimal view sufficient for the orchestrator to shell out to
    /// `pg_dumpall` and `psql`.
    async fn connection_params(&self, service_id: i32) -> Result<PostgresConnection, String>;

    /// Return the image currently persisted in the service configuration.
    /// Upgrade requests must name this exact image as their source so callers
    /// cannot use the upgrade API as an arbitrary image execution primitive.
    async fn docker_image(&self, service_id: i32) -> Result<String, String>;

    /// Stop (best-effort) and remove any existing container for this
    /// service, preserving its named volume. Safe to call on a
    /// non-existent container.
    async fn stop_and_remove(&self, service_id: i32) -> Result<(), String>;

    /// Create and start a Postgres container for this service using the
    /// given image. The container mounts the canonical data volume
    /// (`{container_name}_data`) and joins the app network. Blocks until
    /// Postgres responds to `pg_isready` or the max wait is reached.
    async fn create_and_start(&self, service_id: i32, image: &str) -> Result<(), String>;

    /// Persist the new Docker image onto the service's config so future
    /// restarts (and the reconcile loop) use the upgraded version. Called
    /// during `phase_swap` after the new container is live.
    async fn set_docker_image(&self, service_id: i32, image: &str) -> Result<(), String>;
}

/// Minimal connection-details snapshot returned by
/// `PostgresContainerLifecycle::connection_params`.
#[derive(Debug, Clone)]
pub struct PostgresConnection {
    pub username: String,
    pub password: String,
    pub database: String,
    /// Published host port (for documentation/logging — the orchestrator
    /// itself reaches Postgres via the Docker network, not the host port).
    pub port: String,
}

/// Typed errors for the PostgreSQL major-upgrade pipeline.
#[derive(Error, Debug)]
pub enum PostgresUpgradeError {
    #[error("Upgrade record {upgrade_id} not found")]
    NotFound { upgrade_id: i32 },

    #[error("Service {service_id} is not a Postgres service (actual: {service_type})")]
    WrongServiceType {
        service_id: i32,
        service_type: String,
    },

    #[error("PostgreSQL Docker image '{image}' is not supported for service {service_id}")]
    UnsupportedImage { service_id: i32, image: String },

    #[error(
        "PostgreSQL image mismatch for service {service_id}: configured '{configured_image}', expected '{expected_image}'"
    )]
    SourceImageMismatch {
        service_id: i32,
        configured_image: String,
        expected_image: String,
    },

    #[error("Could not validate PostgreSQL service {service_id} configuration: {reason}")]
    ServiceConfiguration { service_id: i32, reason: String },

    #[error(
        "Cannot upgrade service {service_id} from {from_image} to {to_image}: OS family mismatch ({from_os} -> {to_os}). Cross-OS-family upgrades are unsupported."
    )]
    OsFamilyMismatch {
        service_id: i32,
        from_image: String,
        to_image: String,
        from_os: String,
        to_os: String,
    },

    #[error(
        "Invalid version transition for service {service_id}: {from_version} -> {to_version} ({reason})"
    )]
    InvalidVersionTransition {
        service_id: i32,
        from_version: String,
        to_version: String,
        reason: String,
    },

    #[error("No default S3 source configured for service {service_id} — required for pre-upgrade backup")]
    NoDefaultS3Source { service_id: i32 },

    #[error("Pre-upgrade backup failed for service {service_id}: {reason}. Nothing was migrated and the database is unchanged; fix the cause and retry the upgrade")]
    PreBackupFailed { service_id: i32, reason: String },

    #[error("Another upgrade is already active for service {service_id} (upgrade {existing_upgrade_id}, status {status})")]
    ConcurrentUpgrade {
        service_id: i32,
        existing_upgrade_id: i32,
        status: String,
    },

    #[error("Upgrade {upgrade_id} was cancelled at phase '{phase}'")]
    CancelRequested { upgrade_id: i32, phase: String },

    #[error("Cannot cancel upgrade {upgrade_id}: current status '{status}' is already terminal")]
    NotCancellable { upgrade_id: i32, status: String },

    #[error("Cannot rollback upgrade {upgrade_id}: {reason}")]
    NotRollbackable { upgrade_id: i32, reason: String },

    #[error("Rollback phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    RollbackFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Snapshot phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    SnapshotFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Dump phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    DumpFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error(
        "New-container phase failed for upgrade {upgrade_id} (service {service_id}, image {image}): {reason}"
    )]
    NewContainerFailed {
        upgrade_id: i32,
        service_id: i32,
        image: String,
        reason: String,
    },

    #[error("Restore phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    RestoreFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Swap phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    SwapFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Analyze phase failed for upgrade {upgrade_id} (service {service_id}): {reason}")]
    AnalyzeFailed {
        upgrade_id: i32,
        service_id: i32,
        reason: String,
    },

    #[error("Docker API error during upgrade {upgrade_id}: {reason}")]
    Docker { upgrade_id: i32, reason: String },

    #[error("Log write failed for upgrade {upgrade_id}: {reason}")]
    Log { upgrade_id: i32, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error(transparent)]
    DockerUnavailable(#[from] temps_core::DockerUnavailable),
}

/// Escape a string for safe interpolation into a `sed` BRE/ERE pattern.
/// Used when the dumped service username is baked into a regex that
/// filters out the dump's `DROP ROLE` statements for the connected user.
/// Only identifier-legal characters (letters, digits, `_`) are expected
/// in Postgres role names after Temps normalization, so we escape only
/// the regex metacharacters that could appear in pathological inputs.
fn regex_escape_for_sed(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '.' | '*' | '[' | ']' | '\\' | '^' | '$' | '(' | ')' | '|' | '+' | '?' | '{' | '}'
            | '/' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Derive PGDATA subdir from the image version, matching
/// `PostgresService::get_pgdata_path`. Used when the orchestrator needs
/// to exec inside a container (env var must match what the container was
/// created with).
fn pgdata_path_for(image: &str) -> Result<String, String> {
    let tag = image
        .split(':')
        .nth(1)
        .ok_or_else(|| format!("image '{}' has no tag", image))?;
    let version = tag
        .trim_start_matches("pg")
        .split('-')
        .next()
        .and_then(|v| v.split('.').next())
        .ok_or_else(|| format!("could not extract version from '{}'", image))?
        .parse::<u32>()
        .map_err(|e| format!("bad version in '{}': {}", image, e))?;
    Ok(format!("/var/lib/postgresql/{}/docker", version))
}

/// Retention window for the renamed pre-upgrade PGDATA volume.
///
/// After a successful upgrade the user has this long to invoke rollback
/// before a scheduled sweep removes the volume. Intentionally short enough
/// to reclaim disk and long enough to catch subtle post-upgrade regressions.
pub const ROLLBACK_RETENTION_DAYS: i64 = 7;

/// OS-family classification of a Postgres Docker image tag.
///
/// PostgreSQL data directories are NOT portable across glibc (Debian/Ubuntu)
/// and musl (Alpine) base images due to locale/collation differences. A
/// cross-OS upgrade is unsafe and is rejected before any work starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    /// Alpine / musl-based images (e.g., `postgres:16-alpine`).
    Alpine,
    /// Debian-based images (bookworm, bullseye, etc.) — the default postgres tag.
    Debian,
    /// Custom/unknown base (e.g., `gotempsh/postgres-ha:17-bookworm`).
    /// Matched by substring against known Debian codenames.
    Unknown,
}

impl OsFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            OsFamily::Alpine => "alpine",
            OsFamily::Debian => "debian",
            OsFamily::Unknown => "unknown",
        }
    }
}

/// Best-effort OS-family detection from a Postgres Docker image reference.
///
/// Heuristics:
///   - "alpine" anywhere in the tag  -> Alpine
///   - "bookworm"/"bullseye"/"buster"/"trixie" -> Debian
///   - no tag, or bare `postgres:17` -> Debian (official default)
///   - anything else                 -> Unknown
pub fn detect_os_family(image: &str) -> OsFamily {
    let lower = image.to_ascii_lowercase();
    if lower.contains("alpine") {
        return OsFamily::Alpine;
    }
    if lower.contains("bookworm")
        || lower.contains("bullseye")
        || lower.contains("buster")
        || lower.contains("trixie")
    {
        return OsFamily::Debian;
    }
    // Bare `postgres` / `postgres:17` with no variant — the official image
    // defaults to Debian.
    match lower.split(':').nth(1) {
        None => OsFamily::Debian,
        Some(tag) => {
            // Tag is just a version like "17" or "17.2" -> Debian default.
            if tag
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                && !tag.contains("alpine")
            {
                OsFamily::Debian
            } else {
                OsFamily::Unknown
            }
        }
    }
}

/// Validate that a proposed upgrade is safe. Must be called before any work.
///
/// Enforces:
///   - OS families match (Alpine->Alpine or Debian->Debian; Unknown requires
///     exact string equality of the base image portion)
///   - to_version > from_version (major version must increase)
pub fn validate_os_family(
    service_id: i32,
    from_image: &str,
    to_image: &str,
) -> Result<(), PostgresUpgradeError> {
    let from_os = detect_os_family(from_image);
    let to_os = detect_os_family(to_image);

    if from_os == to_os && from_os != OsFamily::Unknown {
        return Ok(());
    }

    // Unknown family on either side — require the non-version portion to match
    // exactly so a `gotempsh/postgres-ha:16-bookworm` -> `...:17-bookworm`
    // upgrade is still allowed.
    if from_os == to_os && from_os == OsFamily::Unknown {
        let from_base = image_base(from_image);
        let to_base = image_base(to_image);
        if from_base == to_base {
            return Ok(());
        }
    }

    Err(PostgresUpgradeError::OsFamilyMismatch {
        service_id,
        from_image: from_image.to_string(),
        to_image: to_image.to_string(),
        from_os: from_os.as_str().to_string(),
        to_os: to_os.as_str().to_string(),
    })
}

fn image_major_version(image: &str) -> Option<u32> {
    image
        .rsplit_once(':')
        .map(|(_, tag)| tag)?
        .trim_start_matches("pg")
        .split(['-', '.'])
        .next()?
        .parse()
        .ok()
}

/// Validate every image/version invariant needed before an upgrade row may
/// execute. This is shared by request validation, retry/resumption, and
/// rollback so legacy or manually modified rows cannot bypass the checks.
pub fn validate_image_transition(
    service_id: i32,
    from_version: &str,
    to_version: &str,
    from_image: &str,
    to_image: &str,
) -> Result<(), PostgresUpgradeError> {
    for image in [from_image, to_image] {
        super::postgres::validate_postgres_docker_image(image).map_err(|_| {
            PostgresUpgradeError::UnsupportedImage {
                service_id,
                image: image.to_string(),
            }
        })?;
    }
    validate_os_family(service_id, from_image, to_image)?;

    let from_major = from_version.parse::<u32>().unwrap_or(0);
    let to_major = to_version.parse::<u32>().unwrap_or(0);
    if from_major == 0 || to_major == 0 || to_major <= from_major {
        return Err(PostgresUpgradeError::InvalidVersionTransition {
            service_id,
            from_version: from_version.to_string(),
            to_version: to_version.to_string(),
            reason: "to_version must be a greater major version than from_version".into(),
        });
    }
    for (image, declared_version) in [(from_image, from_major), (to_image, to_major)] {
        if image_major_version(image) != Some(declared_version) {
            return Err(PostgresUpgradeError::InvalidVersionTransition {
                service_id,
                from_version: from_version.to_string(),
                to_version: to_version.to_string(),
                reason: format!(
                    "image '{}' does not match its declared major version {}",
                    image, declared_version
                ),
            });
        }
    }
    Ok(())
}

fn validate_phase_config_image(
    service_id: i32,
    current_phase: &str,
    configured_image: &str,
    from_image: &str,
    to_image: &str,
) -> Result<(), PostgresUpgradeError> {
    let (matches_expected, expected_image) = match current_phase {
        // `set_docker_image` and the phase update are separate durable
        // operations. A retry at SWAP may therefore observe either value.
        phase::SWAP => (
            configured_image == from_image || configured_image == to_image,
            format!("{} or {}", from_image, to_image),
        ),
        phase::ANALYZE | phase::COMPLETED => (configured_image == to_image, to_image.to_string()),
        _ => (configured_image == from_image, from_image.to_string()),
    };
    if matches_expected {
        return Ok(());
    }
    Err(PostgresUpgradeError::SourceImageMismatch {
        service_id,
        configured_image: configured_image.to_string(),
        expected_image,
    })
}

fn validate_rollback_config_image(
    service_id: i32,
    rollback_status: &str,
    configured_image: &str,
    from_image: &str,
    to_image: &str,
) -> Result<(), PostgresUpgradeError> {
    let retrying = rollback_status == status::ROLLING_BACK;
    if configured_image == to_image || (retrying && configured_image == from_image) {
        return Ok(());
    }
    Err(PostgresUpgradeError::SourceImageMismatch {
        service_id,
        configured_image: configured_image.to_string(),
        expected_image: if retrying {
            format!("{} or {}", to_image, from_image)
        } else {
            to_image.to_string()
        },
    })
}

/// Strip the version prefix from a tag, keeping the OS portion.
/// `postgres:17-alpine` -> `postgres:alpine`; `gotempsh/postgres-ha:17-bookworm` -> `gotempsh/postgres-ha:bookworm`.
fn image_base(image: &str) -> String {
    match image.split_once(':') {
        None => image.to_string(),
        Some((repo, tag)) => {
            let suffix = tag.split_once('-').map(|(_, rest)| rest).unwrap_or("");
            if suffix.is_empty() {
                repo.to_string()
            } else {
                format!("{}:{}", repo, suffix)
            }
        }
    }
}

/// Phase identifiers persisted to `postgres_major_upgrades.phase`.
///
/// String values are the canonical DB encoding. Keep these stable — they
/// are user-visible via the API and drive retry resumption.
pub mod phase {
    /// Run a full pg_dump-based backup to the default S3 source before
    /// touching any volumes. Unrecoverable failure here aborts the upgrade.
    pub const PRE_BACKUP: &str = "pre_backup";
    pub const SNAPSHOT: &str = "snapshot";
    pub const DUMP: &str = "dump";
    pub const NEW_CONTAINER: &str = "new_container";
    pub const RESTORE: &str = "restore";
    pub const SWAP: &str = "swap";
    pub const ANALYZE: &str = "analyze";
    pub const COMPLETED: &str = "completed";
}

/// Status identifiers persisted to `postgres_major_upgrades.status`.
pub mod status {
    pub const PENDING: &str = "pending";
    pub const RUNNING: &str = "running";
    pub const FAILED: &str = "failed";
    pub const COMPLETED: &str = "completed";
    pub const CANCELLED: &str = "cancelled";
    /// A completed upgrade is actively being rolled back: the live volume
    /// has been (or is about to be) deleted and is being replaced by a copy
    /// of the rollback volume. Active for `ensure_no_active_upgrade`
    /// purposes, same as PENDING/RUNNING — a concurrent start must not
    /// recreate the container against a volume mid-restore. Retryable: a
    /// `rollback()` call is idempotent, so a row stuck here after a failed
    /// attempt can simply be re-rolled-back.
    pub const ROLLING_BACK: &str = "rolling_back";
    /// Upgrade was completed, then rolled back to the pre-upgrade PGDATA
    /// volume and old image. Terminal — a new upgrade must be started
    /// separately to re-attempt.
    pub const ROLLED_BACK: &str = "rolled_back";
}

/// Orchestrates the pg major upgrade state machine for a single upgrade row.
///
/// Constructed per-run by the service layer. Holds only `Arc` clones of
/// shared dependencies; safe to `tokio::spawn`.
pub struct PostgresUpgradeOrchestrator {
    db: Arc<DatabaseConnection>,
    docker: Arc<Docker>,
    backup_provider: Arc<dyn PreUpgradeBackupProvider>,
    lifecycle: Arc<dyn PostgresContainerLifecycle>,
    log_service: Arc<LogService>,
    /// Late-bound telemetry reporter. Set via [`Self::set_telemetry`] after
    /// construction so the orchestrator can be created without requiring the
    /// telemetry reporter to be known at construction time. Falls back to a
    /// no-op reporter when absent.
    telemetry: OnceLock<Arc<dyn TelemetryReporter>>,
}

impl PostgresUpgradeOrchestrator {
    async fn validate_service_image(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        let configured_image =
            self.lifecycle
                .docker_image(row.service_id)
                .await
                .map_err(|reason| PostgresUpgradeError::ServiceConfiguration {
                    service_id: row.service_id,
                    reason,
                })?;
        validate_phase_config_image(
            row.service_id,
            &row.phase,
            &configured_image,
            &row.from_image,
            &row.to_image,
        )
    }

    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: Arc<Docker>,
        backup_provider: Arc<dyn PreUpgradeBackupProvider>,
        lifecycle: Arc<dyn PostgresContainerLifecycle>,
        log_service: Arc<LogService>,
    ) -> Self {
        Self {
            db,
            docker,
            backup_provider,
            lifecycle,
            log_service,
            telemetry: OnceLock::new(),
        }
    }

    /// Attach an optional anonymous telemetry reporter. Call this after
    /// [`Self::new`] before spawning the orchestrator task. Safe to skip —
    /// the orchestrator falls back to a no-op reporter when unset.
    pub fn set_telemetry(&self, reporter: Arc<dyn TelemetryReporter>) {
        // Ignore the error: if already set (e.g., from a resumed upgrade path)
        // we keep the first value.
        let _ = self.telemetry.set(reporter);
    }

    /// Returns the registered reporter or a static no-op fallback.
    fn telemetry(&self) -> &dyn TelemetryReporter {
        static NOOP: NoopTelemetryReporter = NoopTelemetryReporter;
        self.telemetry.get().map(|r| r.as_ref()).unwrap_or(&NOOP)
    }

    /// Drive the upgrade to completion or failure.
    ///
    /// Reads the current phase from the DB and dispatches to phase methods.
    /// Each phase:
    ///   1. Writes a start log line
    ///   2. Performs idempotent work (safe to re-run from failure)
    ///   3. On success, advances `phase` to the next value
    ///   4. On error, marks status=`failed`, stores `error_message`, returns
    pub async fn run(&self, upgrade_id: i32) -> Result<(), PostgresUpgradeError> {
        let mut row = self.load_upgrade(upgrade_id).await?;

        // Defense in depth for retries and boot-time resumption: legacy or
        // manually modified rows must satisfy the same invariants as requests.
        validate_image_transition(
            row.service_id,
            &row.from_version,
            &row.to_version,
            &row.from_image,
            &row.to_image,
        )?;
        self.validate_service_image(&row).await?;

        // Honour a cancel that arrived before we even started dispatching.
        if row.status == status::CANCELLED {
            self.log_info(&row.log_id, "orchestrator aborted: cancel requested")
                .await?;
            return Err(PostgresUpgradeError::CancelRequested {
                upgrade_id,
                phase: row.phase.clone(),
            });
        }

        // Mark running + stamp started_at on first entry
        if row.status == status::PENDING {
            row = self
                .set_status(upgrade_id, status::RUNNING, Some(true))
                .await?;
        }

        // Every phase transition re-validates OS family — cheap, and guards
        // against the service's Docker image being edited mid-upgrade.
        validate_image_transition(
            row.service_id,
            &row.from_version,
            &row.to_version,
            &row.from_image,
            &row.to_image,
        )?;

        // Dispatch loop — each phase method advances `phase` on success.
        // On failure, the phase method itself writes status=failed + error_message.
        // Between phases we re-read status so a cancel posted mid-run is
        // honoured without having to interrupt the currently-running phase.
        loop {
            // Re-read the authoritative service image at every phase boundary.
            // This catches an out-of-band reconfiguration before another image
            // is pulled, started, restored, or persisted.
            self.validate_service_image(&row).await?;
            match row.phase.as_str() {
                phase::PRE_BACKUP => self.phase_pre_backup(&row).await?,
                phase::SNAPSHOT => self.phase_snapshot(&row).await?,
                phase::DUMP => self.phase_dump(&row).await?,
                phase::NEW_CONTAINER => self.phase_new_container(&row).await?,
                phase::RESTORE => self.phase_restore(&row).await?,
                phase::SWAP => self.phase_swap(&row).await?,
                phase::ANALYZE => self.phase_analyze(&row).await?,
                phase::COMPLETED => {
                    self.finalize_completed(upgrade_id).await?;
                    return Ok(());
                }
                other => {
                    return Err(PostgresUpgradeError::InvalidVersionTransition {
                        service_id: row.service_id,
                        from_version: row.from_version.clone(),
                        to_version: row.to_version.clone(),
                        reason: format!("unknown phase '{}'", other),
                    });
                }
            }
            row = self.load_upgrade(upgrade_id).await?;

            // Phase-boundary cancel check. If the user cancelled while a
            // phase was executing, the row now has status=cancelled; stop
            // here without advancing further phases.
            if row.status == status::CANCELLED {
                self.log_info(
                    &row.log_id,
                    format!(
                        "orchestrator stopping at phase '{}': cancel requested",
                        row.phase
                    ),
                )
                .await?;
                return Err(PostgresUpgradeError::CancelRequested {
                    upgrade_id,
                    phase: row.phase.clone(),
                });
            }
        }
    }

    // ---- Phase implementations ------------------------------------------

    /// Mandatory pre-upgrade backup. Resolves the default S3 source, triggers
    /// a full backup via the provider, and records the resulting backup id
    /// on the upgrade row. Idempotent: skipped if a backup id is already set.
    async fn phase_pre_backup(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        if row.pre_upgrade_backup_id.is_some() {
            self.log_info(&row.log_id, "phase=pre_backup skipped (already done)")
                .await?;
            self.advance_phase(row.id, phase::SNAPSHOT).await?;
            return Ok(());
        }

        self.log_info(&row.log_id, "phase=pre_backup start").await?;

        let s3_source_id = self
            .backup_provider
            .default_s3_source_id(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::PreBackupFailed {
                service_id: row.service_id,
                reason: format!("failed to resolve default S3 source: {}", e),
            })?
            .ok_or(PostgresUpgradeError::NoDefaultS3Source {
                service_id: row.service_id,
            })?;

        let backup_id = self
            .backup_provider
            .create_pre_upgrade_backup(row.service_id, s3_source_id, row.created_by)
            .await
            .map_err(|e| PostgresUpgradeError::PreBackupFailed {
                service_id: row.service_id,
                reason: e,
            })?;

        // Persist the backup id and advance.
        let current = self.load_upgrade(row.id).await?;
        let mut active: postgres_major_upgrades::ActiveModel = current.into();
        active.pre_upgrade_backup_id = Set(Some(backup_id));
        active.phase = Set(phase::SNAPSHOT.to_string());
        active.update(self.db.as_ref()).await?;

        self.log_info(
            &row.log_id,
            format!("phase=pre_backup done (backup_id={})", backup_id),
        )
        .await?;
        Ok(())
    }

    /// Snapshot phase: stop the live Postgres container, then copy its PGDATA
    /// volume into a rollback volume. After the copy completes, remove the
    /// live volume so `phase_new_container` can recreate it empty.
    ///
    /// Ordering is critical for data integrity: **stop BEFORE copy**. If the
    /// container keeps accepting writes while we copy the volume, those
    /// writes land in the rollback volume (filesystem-level copy sees them)
    /// but NOT in the logical dump produced by `phase_dump` (which runs
    /// against the frozen rollback volume). The result is silent data loss
    /// on the new-version cluster. Stopping first makes writes fail loudly
    /// (connection refused) for the duration of the copy — the app sees
    /// downtime instead of lost data.
    ///
    /// Docker also refuses to delete a mounted volume, so the live volume
    /// removal at the end must follow the container stop regardless.
    ///
    /// Idempotent: if `rollback_volume_name` is already set, the phase
    /// simply advances (the copy step is the slow part; avoid redoing it).
    pub async fn phase_snapshot(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        if row.rollback_volume_name.is_some() {
            self.log_info(
                &row.log_id,
                "phase=snapshot skipped (rollback volume exists)",
            )
            .await?;
            self.advance_phase(row.id, phase::DUMP).await?;
            return Ok(());
        }

        self.log_info(&row.log_id, "phase=snapshot start").await?;

        let container_name = self.lifecycle_container_name(row).await?;
        let source_volume = format!("{}_data", container_name);
        let rollback_volume = format!("{}_data_rollback_{}", container_name, row.id);

        // The snapshot copies `{container}_data` and the dump reads PGDATA
        // at `<from major>/docker` inside it. A container keeping its cluster
        // anywhere else (an older layout, an imported container, a bind
        // mount) would be dumped as an empty cluster and its live volume
        // deleted, so refuse before stopping anything (#1415).
        let from_pgdata = pgdata_path_for(&row.from_image).map_err(|reason| {
            PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason,
            }
        })?;
        super::recreate_guard::preflight(
            &self.docker,
            super::recreate_guard::ManagedEngine::Postgres,
            &format!("service {}", row.service_id),
            &container_name,
            &super::recreate_guard::ReplacementStorage {
                data_dir: from_pgdata,
                mounts: vec![super::recreate_guard::PlannedMount {
                    volume: source_volume.clone(),
                    target: "/var/lib/postgresql".to_string(),
                }],
            },
            "a major-version upgrade",
        )
        .await
        .map_err(|e| PostgresUpgradeError::SnapshotFailed {
            upgrade_id: row.id,
            service_id: row.service_id,
            reason: e.to_string(),
        })?;

        self.ensure_busybox_pulled(row).await?;
        self.create_volume_if_missing(row, &rollback_volume).await?;

        // Stop the live container BEFORE copying so writes can't race the
        // copy and produce a rollback volume newer than the dump.
        self.lifecycle
            .stop_and_remove(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("failed to stop old container before snapshot copy: {}", e),
            })?;

        self.copy_volume(row, &source_volume, &rollback_volume)
            .await?;
        // Remove the original. The new_container phase must start from an
        // empty live volume; reusing old-version PGDATA breaks Postgres 18+.
        self.remove_volume_or_fail(&source_volume)
            .await
            .map_err(|reason| PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("remove old live volume '{}': {}", source_volume, reason),
            })?;

        let expires_at = chrono::Utc::now() + chrono::Duration::days(ROLLBACK_RETENTION_DAYS);
        let current = self.load_upgrade(row.id).await?;
        let mut active: postgres_major_upgrades::ActiveModel = current.into();
        active.rollback_volume_name = Set(Some(rollback_volume.clone()));
        active.rollback_volume_expires_at = Set(Some(expires_at));
        active.phase = Set(phase::DUMP.to_string());
        active.update(self.db.as_ref()).await?;

        self.log_info(
            &row.log_id,
            format!(
                "phase=snapshot done (rollback volume='{}', expires_at={})",
                rollback_volume,
                expires_at.to_rfc3339()
            ),
        )
        .await?;
        Ok(())
    }

    /// Dump phase: runs `pg_dumpall` from the rollback volume into a named
    /// dump volume via a throwaway old-version Postgres container.
    ///
    /// Idempotent: if the marker file `/dump/.done` exists in the dump
    /// volume, the phase is a no-op. The marker is written atomically
    /// (via `mv .done.tmp .done`) after pg_dumpall succeeds, so a crash
    /// mid-dump leaves no marker and the retry does a clean redo.
    ///
    /// Uses a throwaway old-version container (read-only rollback volume
    /// mount → `pg_dumpall`) on an ephemeral internal port. The container
    /// is force-removed on exit regardless of success so no stale
    /// containers accumulate.
    pub async fn phase_dump(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_info(&row.log_id, "phase=dump start").await?;

        let container_name = self.lifecycle_container_name(row).await?;
        let rollback_volume =
            row.rollback_volume_name
                .clone()
                .ok_or_else(|| PostgresUpgradeError::DumpFailed {
                    upgrade_id: row.id,
                    service_id: row.service_id,
                    reason: "snapshot phase did not produce a rollback_volume_name".into(),
                })?;
        let dump_volume = format!("{}_pgdump_{}", container_name, row.id);
        let dump_container = format!("temps_pg_upgrade_{}_dumper", row.id);

        self.ensure_busybox_pulled(row).await?;
        self.create_volume_if_missing(row, &dump_volume).await?;

        // Idempotency check: has a previous attempt already finished?
        if self.dump_marker_present(&dump_volume).await? {
            self.log_info(&row.log_id, "phase=dump skipped (marker present)")
                .await?;
            self.advance_phase(row.id, phase::NEW_CONTAINER).await?;
            return Ok(());
        }

        // Load connection params — we need the user/password/db to exec
        // pg_dumpall inside the throwaway container.
        let conn = self
            .lifecycle
            .connection_params(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::DumpFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("failed to load connection params: {}", e),
            })?;
        let pgdata_path =
            pgdata_path_for(&row.from_image).map_err(|e| PostgresUpgradeError::DumpFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: e,
            })?;

        // Pull the old-version image in case it was pruned since service
        // creation.
        self.pull_image(row, &row.from_image).await?;

        // Start old-version container with rollback volume mounted
        // read-write at PGDATA (postgres needs to write lockfiles/etc even
        // with a seeded data dir). Dump volume goes at /dump.
        let env_vars = vec![
            format!("POSTGRES_USER={}", conn.username),
            format!("POSTGRES_PASSWORD={}", conn.password),
            format!("POSTGRES_DB={}", conn.database),
            format!("PGDATA={}", pgdata_path),
            "POSTGRES_HOST_AUTH_METHOD=md5".to_string(),
        ];

        let cfg = bollard::models::ContainerCreateBody {
            image: Some(row.from_image.clone()),
            env: Some(env_vars),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![
                    bollard::models::Mount {
                        target: Some("/var/lib/postgresql".to_string()),
                        source: Some(rollback_volume.clone()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    },
                    bollard::models::Mount {
                        target: Some("/dump".to_string()),
                        source: Some(dump_volume.clone()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    },
                ]),
                auto_remove: Some(false), // we remove manually so we can inspect on failure
                ..Default::default()
            }),
            ..Default::default()
        };

        // Remove any previous dumper container left over from a prior attempt.
        let _ = self
            .docker
            .remove_container(
                &dump_container,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let created = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&dump_container)
                        .build(),
                ),
                cfg,
            )
            .await
            .map_err(|e| PostgresUpgradeError::DumpFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("create dumper container: {}", e),
            })?;

        self.docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| PostgresUpgradeError::DumpFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("start dumper container: {}", e),
            })?;

        // Wait for Postgres inside the dumper to come up.
        self.wait_for_pg_ready(row, &dump_container, &conn.username, &conn.database)
            .await
            .map_err(|reason| PostgresUpgradeError::DumpFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason,
            })?;

        // Exec pg_dumpall → /dump/data.sql, then atomic-rename marker.
        // Note: pg_dumpall dumps the entire cluster (all databases + global
        // objects like roles/tablespaces). Its `-d` flag means *connection
        // string*, not "which database"; we omit it and connect to the
        // superuser's default DB via `-U`.
        let dump_cmd = format!(
            "set -eu; pg_dumpall -U {user} --clean --if-exists > /dump/data.sql && sync && : > /dump/.done.tmp && mv /dump/.done.tmp /dump/.done",
            user = shell_escape(&conn.username),
        );
        let exec_res = self
            .exec_and_wait(
                row,
                &dump_container,
                vec!["sh".to_string(), "-c".to_string(), dump_cmd],
                Some(&conn.password),
                // Some reviewed images run as a non-root UID whose primary
                // group cannot write a freshly-created Docker volume. The
                // dump client does not need to run as the database server
                // user; root writes a world-readable SQL file for restore.
                Some("0"),
            )
            .await;

        // Always tear down the dumper before evaluating the result.
        let _ = self
            .docker
            .stop_container(
                &dump_container,
                None::<bollard::query_parameters::StopContainerOptions>,
            )
            .await;
        let _ = self
            .docker
            .remove_container(
                &dump_container,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        exec_res.map_err(|reason| PostgresUpgradeError::DumpFailed {
            upgrade_id: row.id,
            service_id: row.service_id,
            reason,
        })?;

        self.log_info(
            &row.log_id,
            format!("phase=dump done (dump volume='{}')", dump_volume),
        )
        .await?;

        self.advance_phase(row.id, phase::NEW_CONTAINER).await?;
        Ok(())
    }

    /// New-container phase: boots the new-version Postgres on a fresh
    /// empty data volume. Postgres' entrypoint runs `initdb` because the
    /// volume is empty (snapshot phase removed the old one). Restore will
    /// then load the dump into this running instance.
    ///
    /// Idempotent: `lifecycle.create_and_start` first stops/removes any
    /// existing container with the same name and creates the volume if
    /// missing, so a retry is a clean redo.
    async fn phase_new_container(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_info(&row.log_id, "phase=new_container start")
            .await?;

        self.lifecycle
            .create_and_start(row.service_id, &row.to_image)
            .await
            .map_err(|e| PostgresUpgradeError::NewContainerFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                image: row.to_image.clone(),
                reason: e,
            })?;

        self.log_info(
            &row.log_id,
            format!(
                "phase=new_container done (image='{}' running)",
                row.to_image
            ),
        )
        .await?;
        self.advance_phase(row.id, phase::RESTORE).await?;
        Ok(())
    }

    /// Restore phase: streams `/dump/data.sql` into the newly-started
    /// new-version container using a throwaway `psql` sidecar.
    ///
    /// The sidecar runs the SAME `from_image` (matches the dump's dialect)
    /// and shares the app network so it can reach the new container by
    /// name. It mounts the dump volume read-only at `/dump`. The dump
    /// was generated with `--clean --if-exists` so re-running against a
    /// partially-restored DB converges to the same state — retry-safe.
    async fn phase_restore(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_info(&row.log_id, "phase=restore start").await?;

        let container_name = self.lifecycle_container_name(row).await?;
        let dump_volume = format!("{}_pgdump_{}", container_name, row.id);
        let restore_container = format!("temps_pg_upgrade_{}_restorer", row.id);

        let conn = self
            .lifecycle
            .connection_params(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::RestoreFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("failed to load connection params: {}", e),
            })?;

        // Use the new-version image for psql so we can reach server-side
        // catalogs that the dump may reference.
        self.pull_image(row, &row.to_image).await?;

        // Ensure the new container is reachable by DNS on the app network
        // before we start the restorer. create_and_start already waited
        // for pg_isready on a local exec, but we additionally confirm the
        // sidecar can see it.
        //
        // Restore strategy:
        //   - connect as the service user (the only superuser on the fresh
        //     new-version cluster) to the built-in `postgres` maintenance DB.
        //     We use `postgres` rather than `{db}` so the dump's
        //     `DROP DATABASE {db}` / `CREATE DATABASE {db}` / `\c {db}`
        //     directives rebuild the target DB cleanly even though
        //     POSTGRES_DB pre-created it on initdb.
        //   - stream the dump through a sed filter that strips the dump's
        //     own `DROP ROLE IF EXISTS {user}` / `CREATE ROLE {user}` /
        //     `ALTER ROLE {user}` statements for the connected user.
        //     Postgres refuses to drop the currently-connected role; without
        //     this filter the restore fails with "current user cannot be
        //     dropped". The user's grants and memberships are re-applied
        //     later in the dump via `GRANT ... TO {user}` which works fine.
        // Match any of:
        //   DROP ROLE IF EXISTS {user};
        //   CREATE ROLE {user};
        //   ALTER ROLE {user} WITH ...
        // The role name appears after an optional "IF EXISTS " for DROP.
        // Defense-in-depth: reject any username outside a plain identifier
        // charset before it is baked into the sed program below. The sed
        // argument is fully shell-escaped, so this is not the primary guard —
        // it just ensures a crafted username can never reach the shell or the
        // sed program in the first place.
        validate_pg_username(&conn.username).map_err(|reason| {
            PostgresUpgradeError::RestoreFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason,
            }
        })?;

        // Build the sed role-filter program, then shell-escape the ENTIRE sed
        // argument. `regex_escape_for_sed` neutralizes regex metacharacters but
        // NOT the single quotes that wrap the program, so the sed string must
        // still pass through `shell_escape` before interpolation — the
        // `-U {user}` arg below already does; this one previously did not, so a
        // `'` in the username could break out of the sed quotes into the shell.
        let sed_program = format!(
            "/^(DROP ROLE (IF EXISTS )?|CREATE ROLE |ALTER ROLE ){user}($| |;)/d",
            user = regex_escape_for_sed(&conn.username),
        );
        let sed_filter = format!("sed -E {}", shell_escape(&sed_program));
        let psql_cmd = format!(
            "set -eu; export PGPASSWORD={pw}; {sed} /dump/data.sql | psql -v ON_ERROR_STOP=1 -h {host} -U {user} -d postgres",
            pw = shell_escape(&conn.password),
            sed = sed_filter,
            host = shell_escape(&container_name),
            user = shell_escape(&conn.username),
        );

        let cfg = bollard::models::ContainerCreateBody {
            image: Some(row.to_image.clone()),
            entrypoint: Some(vec!["sh".to_string(), "-c".to_string(), psql_cmd]),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![bollard::models::Mount {
                    target: Some("/dump".to_string()),
                    source: Some(dump_volume.clone()),
                    typ: Some(bollard::models::MountTypeEnum::VOLUME),
                    read_only: Some(true),
                    ..Default::default()
                }]),
                auto_remove: Some(false),
                ..Default::default()
            }),
            networking_config: Some(bollard::models::NetworkingConfig {
                endpoints_config: Some(std::collections::HashMap::from([(
                    temps_core::NETWORK_NAME.to_string(),
                    bollard::models::EndpointSettings::default(),
                )])),
            }),
            ..Default::default()
        };

        // Clean up any stale restorer from prior attempt.
        let _ = self
            .docker
            .remove_container(
                &restore_container,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let created = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&restore_container)
                        .build(),
                ),
                cfg,
            )
            .await
            .map_err(|e| PostgresUpgradeError::RestoreFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("create restorer container: {}", e),
            })?;

        self.docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| PostgresUpgradeError::RestoreFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("start restorer container: {}", e),
            })?;

        let exit_res = self.wait_container_exit(row, &created.id).await;

        // Tear down regardless of outcome.
        let _ = self
            .docker
            .remove_container(
                &restore_container,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let exit_code = exit_res.map_err(|reason| PostgresUpgradeError::RestoreFailed {
            upgrade_id: row.id,
            service_id: row.service_id,
            reason,
        })?;

        if exit_code != 0 {
            return Err(PostgresUpgradeError::RestoreFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("psql restore exited with code {}", exit_code),
            });
        }

        self.log_info(&row.log_id, "phase=restore done (dump applied)")
            .await?;
        self.advance_phase(row.id, phase::SWAP).await?;
        Ok(())
    }

    /// Swap phase: persist the new image onto the service row so future
    /// restarts and the reconcile loop pick up the upgraded version.
    ///
    /// The running container is already on `row.to_image` (phase
    /// `new_container` started it that way), so this is a purely
    /// metadata-level swap. Idempotent — re-running sets the same value.
    async fn phase_swap(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_info(&row.log_id, "phase=swap start").await?;

        self.lifecycle
            .set_docker_image(row.service_id, &row.to_image)
            .await
            .map_err(|e| PostgresUpgradeError::SwapFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("failed to persist docker_image: {}", e),
            })?;

        self.log_info(
            &row.log_id,
            format!("phase=swap done (service now on '{}')", row.to_image),
        )
        .await?;
        self.advance_phase(row.id, phase::ANALYZE).await?;
        Ok(())
    }

    /// Analyze phase: runs `ANALYZE` on the new-version container so the
    /// planner has fresh statistics. Matches CNPG's final step.
    ///
    /// Uses docker exec on the already-running service container with
    /// `PGPASSWORD` set in the exec env. ANALYZE is idempotent.
    async fn phase_analyze(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_info(&row.log_id, "phase=analyze start").await?;

        let container_name = self.lifecycle_container_name(row).await?;
        let conn = self
            .lifecycle
            .connection_params(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::AnalyzeFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("failed to load connection params: {}", e),
            })?;

        let analyze_cmd = vec![
            "psql".to_string(),
            "-U".to_string(),
            conn.username.clone(),
            "-d".to_string(),
            conn.database.clone(),
            "-c".to_string(),
            "ANALYZE;".to_string(),
        ];

        self.exec_and_wait(
            row,
            &container_name,
            analyze_cmd,
            Some(&conn.password),
            None,
        )
        .await
        .map_err(|reason| PostgresUpgradeError::AnalyzeFailed {
            upgrade_id: row.id,
            service_id: row.service_id,
            reason,
        })?;

        self.log_info(&row.log_id, "phase=analyze done").await?;
        self.advance_phase(row.id, phase::COMPLETED).await?;
        Ok(())
    }

    // ---- Docker helpers -------------------------------------------------

    /// Look up the container name via the lifecycle trait. Central point
    /// so all phases stay consistent with the trait's naming formula.
    async fn lifecycle_container_name(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<String, PostgresUpgradeError> {
        self.lifecycle
            .container_name(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::Docker {
                upgrade_id: row.id,
                reason: format!("lifecycle.container_name: {}", e),
            })
    }

    /// Pull a Docker image — idempotent; Docker short-circuits on cached.
    async fn pull_image(
        &self,
        row: &postgres_major_upgrades::Model,
        image: &str,
    ) -> Result<(), PostgresUpgradeError> {
        crate::utils::pull_image_with_retry(&self.docker, image, None)
            .await
            .map_err(|e| PostgresUpgradeError::Docker {
                upgrade_id: row.id,
                reason: e,
            })
    }

    /// Check whether the dump volume already contains the `/dump/.done`
    /// marker written atomically by a prior successful pg_dumpall. Uses a
    /// throwaway busybox sidecar since we can't introspect volume contents
    /// directly via the Docker API.
    async fn dump_marker_present(&self, dump_volume: &str) -> Result<bool, PostgresUpgradeError> {
        let probe_name = format!(
            "temps_pg_upgrade_probe_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let cfg = bollard::models::ContainerCreateBody {
            image: Some("busybox:latest".to_string()),
            entrypoint: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "test -f /dump/.done".to_string(),
            ]),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![bollard::models::Mount {
                    target: Some("/dump".to_string()),
                    source: Some(dump_volume.to_string()),
                    typ: Some(bollard::models::MountTypeEnum::VOLUME),
                    read_only: Some(true),
                    ..Default::default()
                }]),
                auto_remove: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let created = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&probe_name)
                        .build(),
                ),
                cfg,
            )
            .await
            .map_err(|e| PostgresUpgradeError::Docker {
                upgrade_id: 0,
                reason: format!("probe create: {}", e),
            })?;
        self.docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| PostgresUpgradeError::Docker {
                upgrade_id: 0,
                reason: format!("probe start: {}", e),
            })?;
        use futures::TryStreamExt;
        let waits = self
            .docker
            .wait_container(
                &created.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .try_collect::<Vec<_>>()
            .await;
        let exit_code = waits
            .ok()
            .and_then(|v| v.into_iter().next().map(|r| r.status_code))
            .unwrap_or(1);
        Ok(exit_code == 0)
    }

    /// Poll `pg_isready` inside a container until it returns 0 or the
    /// deadline elapses.
    async fn wait_for_pg_ready(
        &self,
        row: &postgres_major_upgrades::Model,
        container: &str,
        user: &str,
        db: &str,
    ) -> Result<(), String> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if Instant::now() > deadline {
                return Err(format!(
                    "pg_isready timeout in container '{}' (120s)",
                    container
                ));
            }
            let exec = self
                .docker
                .create_exec(
                    container,
                    bollard::models::ExecConfig {
                        cmd: Some(vec![
                            "pg_isready".to_string(),
                            "-U".to_string(),
                            user.to_string(),
                            "-d".to_string(),
                            db.to_string(),
                        ]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| format!("create_exec pg_isready: {}", e))?;
            // start_exec returns a stream; we MUST drain it before polling
            // inspect_exec, or stdout backpressure can stall the exec so
            // inspect_exec never reports an exit code. This bug caused the
            // 120s timeout on healthy containers — see the integration test
            // (wait_ready) which already drains correctly.
            use futures::StreamExt;
            if let Ok(bollard::exec::StartExecResults::Attached { mut output, .. }) =
                self.docker.start_exec(&exec.id, None).await
            {
                while output.next().await.is_some() {}
            }
            if let Ok(info) = self.docker.inspect_exec(&exec.id).await {
                if info.exit_code == Some(0) {
                    let _ = self
                        .log_service
                        .log_info(&row.log_id, format!("pg_isready OK in '{}'", container))
                        .await;
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Run a docker exec, wait for it, fail on non-zero. Optionally sets
    /// `PGPASSWORD` for psql/pg_dumpall authentication.
    async fn exec_and_wait(
        &self,
        row: &postgres_major_upgrades::Model,
        container: &str,
        cmd: Vec<String>,
        pgpassword: Option<&str>,
        user: Option<&str>,
    ) -> Result<(), String> {
        let env = pgpassword.map(|p| vec![format!("PGPASSWORD={}", p)]);
        let exec = self
            .docker
            .create_exec(
                container,
                bollard::models::ExecConfig {
                    cmd: Some(cmd.clone()),
                    env,
                    user: user.map(str::to_string),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("create_exec({:?}): {}", cmd, e))?;

        use futures::StreamExt;
        // start_exec returns a stream of stdout/stderr; drain it so the
        // exec isn't blocked on a backpressured buffer.
        let stream = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| format!("start_exec: {}", e))?;
        const MAX_EXEC_DIAGNOSTIC_CHARS: usize = 2_000;
        let mut exec_output = String::new();
        let mut exec_output_chars = 0usize;
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = stream {
            while let Some(chunk) = output.next().await {
                if let Ok(msg) = chunk {
                    let text = msg.to_string();
                    let remaining = MAX_EXEC_DIAGNOSTIC_CHARS.saturating_sub(exec_output_chars);
                    if remaining > 0 {
                        let captured: String = text.chars().take(remaining).collect();
                        exec_output_chars += captured.chars().count();
                        exec_output.push_str(&captured);
                    }
                    if !text.trim().is_empty() {
                        let _ = self
                            .log_service
                            .log_info(&row.log_id, format!("exec: {}", text.trim_end()))
                            .await;
                    }
                }
            }
        }

        // Poll inspect_exec until Running = false, then check exit_code.
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(1800); // 30 min cap for large dumps
        loop {
            if Instant::now() > deadline {
                return Err(format!("exec timeout ({:?})", cmd));
            }
            match self.docker.inspect_exec(&exec.id).await {
                Ok(info) => {
                    if info.running == Some(false) {
                        match info.exit_code {
                            Some(0) => return Ok(()),
                            Some(code) => {
                                let diagnostic = exec_output.trim();
                                return Err(if diagnostic.is_empty() {
                                    format!("exec exited with code {}", code)
                                } else {
                                    format!("exec exited with code {}: {}", code, diagnostic)
                                });
                            }
                            None => return Err("exec finished with no exit code".to_string()),
                        }
                    }
                }
                Err(e) => return Err(format!("inspect_exec: {}", e)),
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Wait for a container to exit, return its exit code.
    async fn wait_container_exit(
        &self,
        _row: &postgres_major_upgrades::Model,
        container_id: &str,
    ) -> Result<i64, String> {
        use futures::TryStreamExt;
        let waits = self
            .docker
            .wait_container(
                container_id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| format!("wait_container: {}", e))?;
        Ok(waits
            .into_iter()
            .next()
            .map(|r| r.status_code)
            .unwrap_or(-1))
    }

    /// Pull `busybox:latest` for copy/init sidecars. Idempotent — Docker
    /// returns fast if the image is already present.
    async fn ensure_busybox_pulled(
        &self,
        row: &postgres_major_upgrades::Model,
    ) -> Result<(), PostgresUpgradeError> {
        crate::utils::pull_image_with_retry(&self.docker, "busybox:latest", None)
            .await
            .map_err(|e| PostgresUpgradeError::Docker {
                upgrade_id: row.id,
                reason: e,
            })
    }

    /// Create a named Docker volume. Ignores "already exists" — we treat
    /// the result as idempotent since volumes are name-addressable.
    async fn create_volume_if_missing(
        &self,
        row: &postgres_major_upgrades::Model,
        volume_name: &str,
    ) -> Result<(), PostgresUpgradeError> {
        match self
            .docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume_name.to_string()),
                ..Default::default()
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already exists") || msg.contains("Conflict") {
                    Ok(())
                } else {
                    Err(PostgresUpgradeError::Docker {
                        upgrade_id: row.id,
                        reason: format!("create_volume({}) failed: {}", volume_name, msg),
                    })
                }
            }
        }
    }

    /// Copy one volume into another via a busybox sidecar. Blocks until
    /// the sidecar exits. The destination must already exist.
    async fn copy_volume(
        &self,
        row: &postgres_major_upgrades::Model,
        source: &str,
        dest: &str,
    ) -> Result<(), PostgresUpgradeError> {
        use futures::TryStreamExt;
        use std::time::{Duration, Instant};

        let copy_container_name = format!(
            "temps_pg_upgrade_{}_copy_{}",
            row.id,
            chrono::Utc::now().timestamp()
        );

        let cfg = bollard::models::ContainerCreateBody {
            image: Some("busybox:latest".to_string()),
            entrypoint: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "cp -a /src/. /dest/ && sync".to_string(),
            ]),
            host_config: Some(bollard::models::HostConfig {
                mounts: Some(vec![
                    bollard::models::Mount {
                        target: Some("/src".to_string()),
                        source: Some(source.to_string()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        read_only: Some(true),
                        ..Default::default()
                    },
                    bollard::models::Mount {
                        target: Some("/dest".to_string()),
                        source: Some(dest.to_string()),
                        typ: Some(bollard::models::MountTypeEnum::VOLUME),
                        ..Default::default()
                    },
                ]),
                // We remove manually below (with an exit-code check and a
                // wait-for-removal poll) so a lost race against Docker's own
                // auto-remove can't be mistaken for a hard failure.
                auto_remove: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&copy_container_name)
                        .build(),
                ),
                cfg,
            )
            .await
            .map_err(|e| PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("create copy container: {}", e),
            })?;

        self.docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("start copy container: {}", e),
            })?;

        // Capture the wait outcome instead of propagating it immediately:
        // `auto_remove` is disabled above, so an early `?` return here would
        // skip the removal below and leak the sidecar holding a reference to
        // both volumes, wedging any retry's volume removal ("volume is in
        // use") forever. Every exit path from here must still try to remove
        // the container.
        let wait_outcome = self
            .docker
            .wait_container(
                &created.id,
                None::<bollard::query_parameters::WaitContainerOptions>,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| format!("wait copy container: {}", e));

        let remove_result = self
            .docker
            .remove_container(
                &created.id,
                Some(bollard::query_parameters::RemoveContainerOptions {
                    v: true,
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if let Err(e) = remove_result {
            let msg = e.to_string();
            let already_gone = msg.contains("No such container")
                || msg.contains("no such container")
                || msg.contains("not found")
                || msg.contains("404");
            let already_removing =
                msg.contains("removal of container") && msg.contains("is already in progress");
            if !already_gone && !already_removing {
                return Err(PostgresUpgradeError::SnapshotFailed {
                    upgrade_id: row.id,
                    service_id: row.service_id,
                    reason: format!("remove copy container: {}", msg),
                });
            }
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self
                .docker
                .inspect_container(
                    &created.id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
            {
                Ok(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Ok(_) => {
                    return Err(PostgresUpgradeError::SnapshotFailed {
                        upgrade_id: row.id,
                        service_id: row.service_id,
                        reason: format!(
                            "copy container '{}' still exists after removal timeout",
                            created.id
                        ),
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("No such container")
                        || msg.contains("no such container")
                        || msg.contains("not found")
                        || msg.contains("404")
                    {
                        break;
                    }
                    return Err(PostgresUpgradeError::SnapshotFailed {
                        upgrade_id: row.id,
                        service_id: row.service_id,
                        reason: format!("inspect copy container after removal: {}", msg),
                    });
                }
            }
        }

        let wait_result = wait_outcome.map_err(|reason| PostgresUpgradeError::SnapshotFailed {
            upgrade_id: row.id,
            service_id: row.service_id,
            reason,
        })?;

        if let Some(nonzero) = wait_result
            .iter()
            .map(|status| status.status_code)
            .find(|status_code| *status_code != 0)
        {
            return Err(PostgresUpgradeError::SnapshotFailed {
                upgrade_id: row.id,
                service_id: row.service_id,
                reason: format!("copy container exited with status {}", nonzero),
            });
        }

        Ok(())
    }

    /// Best-effort volume removal — swallows errors; the retention sweeper
    /// will retry for rollback volumes, and for transient workspaces a
    /// leaked volume is benign.
    /// Remove a Docker volume, retrying on transient failures. Docker can
    /// briefly report a volume as still in use for a moment after its
    /// container is removed (the detach isn't perfectly synchronous with
    /// `stop_and_remove`) — a single attempt can silently fail and leave the
    /// old volume in place. That matters here specifically: `phase_new_container`
    /// creates its volume with "create if missing", so a volume this call
    /// fails to remove gets *reused* by the next version's container,
    /// carrying over the previous major version's on-disk data layout. New
    /// Postgres images (18+) detect that as an incompatible upgrade and
    /// refuse to start, crash-looping instead of coming up empty as intended.
    /// Still best-effort overall: gives up (silently) after retries exhaust,
    /// same as before, just with a real chance to succeed first.
    async fn remove_volume_best_effort(&self, volume_name: &str) {
        const MAX_ATTEMPTS: u32 = 5;
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .docker
                .remove_volume(
                    volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await
            {
                Ok(()) => return,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                        .await;
                }
                Err(_) => return,
            }
        }
    }

    async fn remove_volume_or_fail(&self, volume_name: &str) -> Result<(), String> {
        self.docker
            .remove_volume(
                volume_name,
                Some(bollard::query_parameters::RemoveVolumeOptions { force: true }),
            )
            .await
            .map_err(|e| e.to_string())
            .or_else(|reason| {
                if reason.contains("No such volume")
                    || reason.contains("not found")
                    || reason.contains("404")
                {
                    Ok(())
                } else {
                    Err(reason)
                }
            })
    }

    // ---- Rollback volume retention -------------------------------------

    /// Sweep rollback volumes whose 7-day retention has expired.
    ///
    /// Called from a scheduled job (hourly is fine). For each completed
    /// upgrade whose `rollback_volume_expires_at` is in the past and whose
    /// `rollback_volume_name` is still set, this removes the Docker volume
    /// and clears the column — a best-effort, idempotent operation.
    ///
    /// Returns the number of volumes successfully removed. Volume-removal
    /// failures are logged but do not abort the sweep for remaining rows.
    pub async fn sweep_expired_rollback_volumes(&self) -> Result<u64, PostgresUpgradeError> {
        use sea_orm::{ColumnTrait, QueryFilter};

        let now = chrono::Utc::now();

        let rows = postgres_major_upgrades::Entity::find()
            .filter(postgres_major_upgrades::Column::Status.eq(status::COMPLETED))
            .filter(postgres_major_upgrades::Column::RollbackVolumeName.is_not_null())
            .filter(postgres_major_upgrades::Column::RollbackVolumeExpiresAt.lte(now))
            .all(self.db.as_ref())
            .await?;

        let mut removed: u64 = 0;
        for row in rows {
            let Some(volume_name) = row.rollback_volume_name.clone() else {
                continue;
            };

            let remove_res = self
                .docker
                .remove_volume(
                    &volume_name,
                    None::<bollard::query_parameters::RemoveVolumeOptions>,
                )
                .await;

            match remove_res {
                Ok(_) => {
                    let _ = self
                        .log_info(
                            &row.log_id,
                            format!("rollback volume '{}' expired and removed", volume_name),
                        )
                        .await;
                }
                Err(e) => {
                    let msg = e.to_string();
                    // Treat "already gone" as success so we can clear the column.
                    if !msg.contains("no such volume") && !msg.contains("No such volume") {
                        tracing::warn!(
                            "Failed to remove expired rollback volume '{}' for upgrade {}: {}",
                            volume_name,
                            row.id,
                            msg
                        );
                        let _ = self
                            .log_service
                            .log_warning(
                                &row.log_id,
                                format!(
                                    "rollback volume '{}' removal failed: {}",
                                    volume_name, msg
                                ),
                            )
                            .await;
                        continue;
                    }
                }
            }

            // Clear the column so this row stops matching on the next sweep.
            let id = row.id;
            let mut active: postgres_major_upgrades::ActiveModel = row.into();
            active.rollback_volume_name = Set(None);
            active.rollback_volume_expires_at = Set(None);
            if let Err(e) = active.update(self.db.as_ref()).await {
                tracing::warn!(
                    "Failed to clear rollback_volume_name on upgrade {}: {}",
                    id,
                    e
                );
                continue;
            }
            removed += 1;
        }

        Ok(removed)
    }

    /// Roll a completed upgrade back to its pre-upgrade PGDATA volume and
    /// the old Docker image.
    ///
    /// Preconditions (enforced here so the caller gets typed errors rather
    /// than a partially-applied rollback):
    ///   - row exists and `status == completed`
    ///   - `rollback_volume_name` is still set
    ///   - `rollback_volume_expires_at` is in the future
    ///
    /// Steps:
    ///   1. Stop + remove the current (new-version) container, keeping its
    ///      volume intact in case we need to debug post-rollback.
    ///   2. Copy the rollback volume's contents back into the live data
    ///      volume. We overwrite rather than rename because Docker has no
    ///      native volume rename and the live volume is the one referenced
    ///      by the lifecycle's `create_and_start`.
    ///   3. Persist `docker_image = from_image` so reconcile loops use the
    ///      old tag.
    ///   4. Boot the old-version container.
    ///   5. Mark the row `rolled_back`, clear rollback metadata, remove the
    ///      now-unused rollback volume.
    ///
    /// Idempotent against partial failure: step 2 is a full copy (not a
    /// rename), and each Docker operation tolerates "already in that state".
    pub async fn rollback(
        &self,
        upgrade_id: i32,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        let row = self.load_upgrade(upgrade_id).await?;

        validate_image_transition(
            row.service_id,
            &row.from_version,
            &row.to_version,
            &row.from_image,
            &row.to_image,
        )?;

        // Preconditions. ROLLING_BACK is accepted alongside COMPLETED so a
        // rollback that failed partway (leaving the row stuck in
        // ROLLING_BACK) can be retried through this same entry point —
        // every step below tolerates "already in that state".
        if row.status != status::COMPLETED && row.status != status::ROLLING_BACK {
            return Err(PostgresUpgradeError::NotRollbackable {
                upgrade_id,
                reason: format!(
                    "only completed (or a previously interrupted rollback) can be rolled back; current status is '{}'",
                    row.status
                ),
            });
        }
        let configured_image =
            self.lifecycle
                .docker_image(row.service_id)
                .await
                .map_err(|reason| PostgresUpgradeError::ServiceConfiguration {
                    service_id: row.service_id,
                    reason,
                })?;
        validate_rollback_config_image(
            row.service_id,
            &row.status,
            &configured_image,
            &row.from_image,
            &row.to_image,
        )?;
        let rollback_volume = row.rollback_volume_name.clone().ok_or_else(|| {
            PostgresUpgradeError::NotRollbackable {
                upgrade_id,
                reason: "rollback volume has already been cleared (likely swept after retention window expired)".into(),
            }
        })?;
        if let Some(expires_at) = row.rollback_volume_expires_at {
            if expires_at <= chrono::Utc::now() {
                return Err(PostgresUpgradeError::NotRollbackable {
                    upgrade_id,
                    reason: format!(
                        "rollback retention window expired at {}",
                        expires_at.to_rfc3339()
                    ),
                });
            }
        }

        // Mark the row actively rolling back *before* touching any
        // container or volume, so `ensure_no_active_upgrade` blocks a
        // concurrent start for the entire multi-step restore below, not
        // just while status was PENDING/RUNNING during the original
        // upgrade. Idempotent if already ROLLING_BACK (a retry).
        self.set_status(upgrade_id, status::ROLLING_BACK, None)
            .await?;

        self.log_service
            .log_warning(
                &row.log_id,
                format!(
                    "rollback requested: restoring pre-upgrade PGDATA from '{}' and reverting image to '{}'",
                    rollback_volume, row.from_image
                ),
            )
            .await
            .map_err(|e| PostgresUpgradeError::Log {
                upgrade_id,
                reason: e.to_string(),
            })?;

        let container_name = self.lifecycle_container_name(&row).await?;
        let live_volume = format!("{}_data", container_name);

        // 1. Stop + remove the new-version container (keeps the volume).
        self.lifecycle
            .stop_and_remove(row.service_id)
            .await
            .map_err(|e| PostgresUpgradeError::RollbackFailed {
                upgrade_id,
                service_id: row.service_id,
                reason: format!("stop_and_remove new-version container: {}", e),
            })?;

        // 2. Remove the live volume so the copy produces a clean replica of
        //    the rollback volume (any lingering new-version WAL/catalog is
        //    discarded). This is required; overlaying rollback data onto a
        //    dirty live volume can leave incompatible catalog/WAL files.
        self.remove_volume_or_fail(&live_volume)
            .await
            .map_err(|reason| PostgresUpgradeError::RollbackFailed {
                upgrade_id,
                service_id: row.service_id,
                reason: format!("remove live volume '{}': {}", live_volume, reason),
            })?;
        self.create_volume_if_missing(&row, &live_volume).await?;
        self.copy_volume(&row, &rollback_volume, &live_volume)
            .await
            .map_err(|e| match e {
                PostgresUpgradeError::SnapshotFailed { reason, .. } => {
                    PostgresUpgradeError::RollbackFailed {
                        upgrade_id,
                        service_id: row.service_id,
                        reason: format!("copy rollback volume -> live volume: {}", reason),
                    }
                }
                other => other,
            })?;

        // 3. Persist the old image back on the service row.
        self.lifecycle
            .set_docker_image(row.service_id, &row.from_image)
            .await
            .map_err(|e| PostgresUpgradeError::RollbackFailed {
                upgrade_id,
                service_id: row.service_id,
                reason: format!("set_docker_image({}): {}", row.from_image, e),
            })?;

        // 4. Boot the old-version container against the restored volume.
        self.lifecycle
            .create_and_start(row.service_id, &row.from_image)
            .await
            .map_err(|e| PostgresUpgradeError::RollbackFailed {
                upgrade_id,
                service_id: row.service_id,
                reason: format!("create_and_start(image='{}'): {}", row.from_image, e),
            })?;

        // 5. Stamp the row as rolled_back and clear rollback metadata.
        let row_id = row.id;
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.status = Set(status::ROLLED_BACK.to_string());
        active.finished_at = Set(Some(chrono::Utc::now()));
        active.rollback_volume_name = Set(None);
        active.rollback_volume_expires_at = Set(None);
        let updated = active.update(self.db.as_ref()).await?;

        // 6. Remove the old rollback volume (best-effort; sweep would
        //    eventually catch it but a leaked volume is wasteful).
        self.remove_volume_best_effort(&rollback_volume).await;

        let _ = self
            .log_service
            .log_info(
                &updated.log_id,
                format!(
                    "rollback complete: service now running '{}' against restored PGDATA (upgrade {} marked rolled_back)",
                    updated.from_image, row_id
                ),
            )
            .await;

        Ok(updated)
    }

    // ---- DB helpers -----------------------------------------------------

    async fn load_upgrade(
        &self,
        upgrade_id: i32,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        postgres_major_upgrades::Entity::find_by_id(upgrade_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(PostgresUpgradeError::NotFound { upgrade_id })
    }

    async fn advance_phase(
        &self,
        upgrade_id: i32,
        next_phase: &str,
    ) -> Result<(), PostgresUpgradeError> {
        let row = self.load_upgrade(upgrade_id).await?;
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.phase = Set(next_phase.to_string());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    async fn set_status(
        &self,
        upgrade_id: i32,
        new_status: &str,
        stamp_started: Option<bool>,
    ) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
        let row = self.load_upgrade(upgrade_id).await?;
        let mut active: postgres_major_upgrades::ActiveModel = row.into();
        active.status = Set(new_status.to_string());
        if stamp_started.unwrap_or(false) {
            active.started_at = Set(Some(chrono::Utc::now()));
        }
        if matches!(
            new_status,
            status::COMPLETED | status::FAILED | status::CANCELLED
        ) {
            active.finished_at = Set(Some(chrono::Utc::now()));
        }
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    async fn finalize_completed(&self, upgrade_id: i32) -> Result<(), PostgresUpgradeError> {
        let row = self.set_status(upgrade_id, status::COMPLETED, None).await?;

        // Fire-and-forget anonymous telemetry. Properties are non-identifying
        // Postgres major version numbers only (e.g., "16", "17").
        self.telemetry().report(
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::PgMajorUpgradeCompleted,
            )
            .with("from_version", row.from_version.clone())
            .with("to_version", row.to_version.clone()),
        );

        Ok(())
    }

    // ---- Log helpers ----------------------------------------------------

    async fn log_info(
        &self,
        log_id: &str,
        message: impl Into<String>,
    ) -> Result<(), PostgresUpgradeError> {
        self.log_service
            .log_info(log_id, message)
            .await
            .map_err(|e| PostgresUpgradeError::Log {
                upgrade_id: 0,
                reason: e.to_string(),
            })
    }
}

// ---- HTTP error mapping (RFC 7807) ------------------------------------

impl From<PostgresUpgradeError> for temps_core::problemdetails::Problem {
    fn from(error: PostgresUpgradeError) -> Self {
        use axum::http::StatusCode;
        use temps_core::problemdetails;

        match error {
            PostgresUpgradeError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Upgrade Not Found")
                .with_detail(error.to_string()),

            PostgresUpgradeError::WrongServiceType { .. }
            | PostgresUpgradeError::InvalidVersionTransition { .. }
            | PostgresUpgradeError::OsFamilyMismatch { .. }
            | PostgresUpgradeError::UnsupportedImage { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Upgrade Request")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::NoDefaultS3Source { .. } => {
                problemdetails::new(StatusCode::PRECONDITION_FAILED)
                    .with_title("No Default S3 Source")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::ConcurrentUpgrade { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Upgrade Already In Progress")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::SourceImageMismatch { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("PostgreSQL Service Changed")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::CancelRequested { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Upgrade Cancelled")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::NotCancellable { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Upgrade Not Cancellable")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::NotRollbackable { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Upgrade Not Rollbackable")
                    .with_detail(error.to_string())
            }

            PostgresUpgradeError::DockerUnavailable(_) => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Docker Unavailable")
                .with_detail(error.to_string()),

            internal_error @ (PostgresUpgradeError::PreBackupFailed { .. }
            | PostgresUpgradeError::SnapshotFailed { .. }
            | PostgresUpgradeError::DumpFailed { .. }
            | PostgresUpgradeError::NewContainerFailed { .. }
            | PostgresUpgradeError::RestoreFailed { .. }
            | PostgresUpgradeError::SwapFailed { .. }
            | PostgresUpgradeError::AnalyzeFailed { .. }
            | PostgresUpgradeError::RollbackFailed { .. }
            | PostgresUpgradeError::Docker { .. }
            | PostgresUpgradeError::Log { .. }
            | PostgresUpgradeError::ServiceConfiguration { .. }
            | PostgresUpgradeError::Database(_)) => {
                tracing::error!(error = %internal_error, "PostgreSQL upgrade request failed internally");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail("The PostgreSQL upgrade operation could not be completed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "docker-tests")]
    use crate::externalsvc::test_utils::{run_exec_capture, run_exec_ok};

    #[test]
    fn detect_alpine_family() {
        assert_eq!(detect_os_family("postgres:16-alpine"), OsFamily::Alpine);
        assert_eq!(detect_os_family("postgres:17-alpine3.19"), OsFamily::Alpine);
    }

    #[test]
    fn detect_debian_family_from_codename() {
        assert_eq!(detect_os_family("postgres:16-bookworm"), OsFamily::Debian);
        assert_eq!(detect_os_family("postgres:15-bullseye"), OsFamily::Debian);
        assert_eq!(
            detect_os_family("gotempsh/postgres-ha:17-bookworm"),
            OsFamily::Debian
        );
    }

    #[test]
    fn detect_debian_family_from_bare_version() {
        // Official postgres image defaults to Debian-based.
        assert_eq!(detect_os_family("postgres:17"), OsFamily::Debian);
        assert_eq!(detect_os_family("postgres:17.2"), OsFamily::Debian);
        assert_eq!(detect_os_family("postgres"), OsFamily::Debian);
    }

    #[test]
    fn validate_rejects_cross_os_upgrade() {
        let err = validate_os_family(42, "postgres:16-alpine", "postgres:17-bookworm")
            .expect_err("cross-os must be rejected");
        assert!(matches!(
            err,
            PostgresUpgradeError::OsFamilyMismatch { service_id: 42, .. }
        ));
    }

    #[test]
    fn validate_accepts_same_os_upgrade() {
        validate_os_family(1, "postgres:16-alpine", "postgres:17-alpine")
            .expect("alpine->alpine should pass");
        validate_os_family(1, "postgres:16-bookworm", "postgres:17-bookworm")
            .expect("debian->debian should pass");
        validate_os_family(1, "postgres:16", "postgres:17")
            .expect("bare debian->debian should pass");
    }

    #[test]
    fn validate_accepts_custom_repo_matching_base() {
        validate_os_family(
            1,
            "gotempsh/postgres-ha:16-bookworm",
            "gotempsh/postgres-ha:17-bookworm",
        )
        .expect("same custom repo with same base should pass");
    }

    #[test]
    fn phase_image_validation_preserves_idempotent_swap_retries() {
        let from = "gotempsh/postgres-walg:16-bookworm";
        let to = "gotempsh/postgres-walg:17-bookworm";

        validate_phase_config_image(1, phase::SWAP, from, from, to)
            .expect("swap may resume before the image metadata write");
        validate_phase_config_image(1, phase::SWAP, to, from, to)
            .expect("swap may resume after the image metadata write");
        assert!(matches!(
            validate_phase_config_image(1, phase::SWAP, "postgres:latest", from, to),
            Err(PostgresUpgradeError::SourceImageMismatch { .. })
        ));
    }

    #[test]
    fn phase_image_validation_requires_new_image_after_swap() {
        let from = "gotempsh/postgres-walg:16-bookworm";
        let to = "gotempsh/postgres-walg:17-bookworm";

        assert!(matches!(
            validate_phase_config_image(1, phase::ANALYZE, from, from, to),
            Err(PostgresUpgradeError::SourceImageMismatch { .. })
        ));
        validate_phase_config_image(1, phase::ANALYZE, to, from, to)
            .expect("analyze must use the persisted target image");
    }

    #[test]
    fn resumed_rows_revalidate_image_versions_and_direction() {
        let from = "gotempsh/postgres-walg:17-bookworm";
        let to = "gotempsh/postgres-walg:16-bookworm";
        assert!(matches!(
            validate_image_transition(1, "17", "16", from, to),
            Err(PostgresUpgradeError::InvalidVersionTransition { .. })
        ));
        assert!(matches!(
            validate_image_transition(1, "16", "17", from, to),
            Err(PostgresUpgradeError::InvalidVersionTransition { .. })
        ));
    }

    #[test]
    fn rollback_image_validation_is_phase_aware_and_fail_closed() {
        let from = "gotempsh/postgres-walg:16-bookworm";
        let to = "gotempsh/postgres-walg:17-bookworm";

        validate_rollback_config_image(1, status::COMPLETED, to, from, to)
            .expect("a fresh rollback starts from the target image");
        assert!(validate_rollback_config_image(1, status::COMPLETED, from, from, to).is_err());
        validate_rollback_config_image(1, status::ROLLING_BACK, from, from, to)
            .expect("a rollback retry may resume after persisting the source image");
        assert!(validate_rollback_config_image(
            1,
            status::ROLLING_BACK,
            "postgres:latest",
            from,
            to
        )
        .is_err());
    }

    #[test]
    fn image_base_strips_version() {
        assert_eq!(image_base("postgres:17-alpine"), "postgres:alpine");
        assert_eq!(
            image_base("gotempsh/postgres-ha:17-bookworm"),
            "gotempsh/postgres-ha:bookworm"
        );
        assert_eq!(image_base("postgres:17"), "postgres");
        assert_eq!(image_base("postgres"), "postgres");
    }

    #[test]
    fn shell_escape_handles_quotes() {
        assert_eq!(shell_escape("simple"), "'simple'");
        assert_eq!(shell_escape("with space"), "'with space'");
        assert_eq!(shell_escape("with'quote"), "'with'\\''quote'");
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn pgdata_path_matches_postgres_service_formula() {
        assert_eq!(
            pgdata_path_for("postgres:16-bookworm").unwrap(),
            "/var/lib/postgresql/16/docker"
        );
        assert_eq!(
            pgdata_path_for("postgres:17-alpine").unwrap(),
            "/var/lib/postgresql/17/docker"
        );
        assert_eq!(
            pgdata_path_for("gotempsh/postgres-ha:17-bookworm").unwrap(),
            "/var/lib/postgresql/17/docker"
        );
        assert!(pgdata_path_for("postgres").is_err());
    }

    /// Docker integration test: reproduces the exact dump→restore cycle
    /// that `phase_dump` and `phase_restore` execute, verifying that:
    ///   1. A real `pg_dumpall --clean --if-exists` against an old-version
    ///      Postgres instance produces a SQL file in a shared volume.
    ///   2. That SQL file replayed via `psql -v ON_ERROR_STOP=1 -f` into a
    ///      fresh new-version Postgres instance restores the original data
    ///      byte-for-byte.
    ///   3. The shell-escape + command construction used by the orchestrator
    ///      survives round-trip through `sh -c` with real user/password/db
    ///      values containing typical characters.
    ///   4. `pg_dumpall` carries the WHOLE cluster: several databases (not just
    ///      the default one) all survive the restore, and every database's
    ///      table count is identical before and after.
    ///
    /// This validates the failure-prone shell/Docker-exec plumbing without
    /// having to stand up the full control-plane database, the encryption
    /// service, or the plugin wiring. If Docker is unavailable the test
    /// skips gracefully per project convention.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn docker_dump_restore_v16_to_v17_preserves_data() {
        use bollard::Docker;
        use futures::TryStreamExt;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let docker = match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(e) => {
                println!("Docker not available, skipping: {}", e);
                return;
            }
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }

        // Unique suffix so parallel test runs don't collide.
        let run_id = format!(
            "{}_{}",
            chrono::Utc::now().timestamp(),
            rand::random::<u32>()
        );
        let old_container = format!("temps_upgrade_test_old_{}", run_id);
        let new_container = format!("temps_upgrade_test_new_{}", run_id);
        let dump_volume = format!("temps_upgrade_test_dump_{}", run_id);
        let old_data_volume = format!("temps_upgrade_test_old_data_{}", run_id);
        let new_data_volume = format!("temps_upgrade_test_new_data_{}", run_id);
        let network_name = format!("temps_upgrade_test_net_{}", run_id);

        let cleanup = |docker: Arc<Docker>,
                       old_c: String,
                       new_c: String,
                       old_v: String,
                       new_v: String,
                       dump_v: String,
                       net: String| async move {
            use bollard::query_parameters::{RemoveContainerOptions, RemoveVolumeOptions};
            for c in [&old_c, &new_c] {
                let _ = docker
                    .remove_container(
                        c,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: false,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            for v in [&old_v, &new_v, &dump_v] {
                let _ = docker.remove_volume(v, None::<RemoveVolumeOptions>).await;
            }
            let _ = docker.remove_network(&net).await;
        };

        // Helper: run test body, ensure cleanup even on panic.
        let result = async {
            use bollard::models::*;
            use bollard::query_parameters::*;

            let user = "upgradeuser";
            let password = "p@ss wo'rd";
            let db = "upgradetest";

            // Create network so restorer can reach new container by DNS.
            docker
                .create_network(NetworkCreateRequest {
                    name: network_name.clone(),
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("create_network: {}", e))?;

            // Pull both Postgres images.
            for image in ["postgres:16-bookworm", "postgres:17-bookworm"] {
                let mut s = docker.create_image(
                    Some(CreateImageOptions {
                        from_image: Some(image.to_string()),
                        ..Default::default()
                    }),
                    None,
                    None,
                );
                use futures::StreamExt;
                while let Some(r) = s.next().await {
                    r.map_err(|e| format!("pull {}: {}", image, e))?;
                }
            }

            // Pull busybox for marker-file checks.
            let _ = docker
                .create_image(
                    Some(CreateImageOptions {
                        from_image: Some("busybox".to_string()),
                        tag: Some("latest".to_string()),
                        ..Default::default()
                    }),
                    None,
                    None,
                )
                .try_collect::<Vec<_>>()
                .await;

            // Create volumes.
            for v in [&old_data_volume, &new_data_volume, &dump_volume] {
                docker
                    .create_volume(VolumeCreateRequest {
                        name: Some(v.clone()),
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| format!("create_volume {}: {}", v, e))?;
            }

            // Start OLD (v16) with test user/password/db, seed data.
            let env_vars = vec![
                format!("POSTGRES_USER={}", user),
                format!("POSTGRES_PASSWORD={}", password),
                format!("POSTGRES_DB={}", db),
                "POSTGRES_HOST_AUTH_METHOD=md5".to_string(),
            ];
            let old_cfg = ContainerCreateBody {
                image: Some("postgres:16-bookworm".to_string()),
                env: Some(env_vars.clone()),
                host_config: Some(HostConfig {
                    mounts: Some(vec![
                        Mount {
                            target: Some("/var/lib/postgresql/data".to_string()),
                            source: Some(old_data_volume.clone()),
                            typ: Some(MountTypeEnum::VOLUME),
                            ..Default::default()
                        },
                        Mount {
                            target: Some("/dump".to_string()),
                            source: Some(dump_volume.clone()),
                            typ: Some(MountTypeEnum::VOLUME),
                            ..Default::default()
                        },
                    ]),
                    network_mode: Some(network_name.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let created = docker
                .create_container(
                    Some(
                        CreateContainerOptionsBuilder::new()
                            .name(&old_container)
                            .build(),
                    ),
                    old_cfg,
                )
                .await
                .map_err(|e| format!("create old: {}", e))?;
            docker
                .start_container(&created.id, None::<StartContainerOptions>)
                .await
                .map_err(|e| format!("start old: {}", e))?;

            // Give Postgres a moment to start initdb before polling.
            tokio::time::sleep(Duration::from_secs(2)).await;

            // If the container has already exited, surface the logs.
            if let Ok(insp) = docker
                .inspect_container(&old_container, None::<InspectContainerOptions>)
                .await
            {
                if let Some(state) = insp.state.as_ref() {
                    if state.running == Some(false) {
                        let logs = docker
                            .logs(
                                &old_container,
                                Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()),
                            )
                            .try_collect::<Vec<_>>()
                            .await
                            .map(|v| {
                                v.into_iter()
                                    .map(|c| c.to_string())
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        return Err(format!(
                            "old container exited early: status={:?}, logs:\n{}",
                            state.status, logs
                        ));
                    }
                }
            }

            // Wait for old Postgres ready.
            let wait_ready = |container: &str| {
                let docker = Arc::clone(&docker);
                let container = container.to_string();
                let user = user.to_string();
                let db = db.to_string();
                async move {
                    use futures::StreamExt;
                    let deadline = Instant::now() + Duration::from_secs(90);
                    let mut last_exit: Option<i64> = None;
                    while Instant::now() < deadline {
                        if let Ok(exec) = docker
                            .create_exec(
                                &container,
                                ExecConfig {
                                    cmd: Some(vec![
                                        "pg_isready".into(),
                                        "-U".into(),
                                        user.clone(),
                                        "-d".into(),
                                        db.clone(),
                                    ]),
                                    attach_stdout: Some(true),
                                    attach_stderr: Some(true),
                                    ..Default::default()
                                },
                            )
                            .await
                        {
                            if let Ok(bollard::exec::StartExecResults::Attached {
                                mut output,
                                ..
                            }) = docker.start_exec(&exec.id, None).await
                            {
                                while output.next().await.is_some() {}
                            }
                            if let Ok(info) = docker.inspect_exec(&exec.id).await {
                                last_exit = info.exit_code;
                                if info.exit_code == Some(0) {
                                    return Ok::<(), String>(());
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    // Dump container logs on timeout for diagnosis.
                    use futures::TryStreamExt;
                    let logs = docker
                        .logs(
                            &container,
                            Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()),
                        )
                        .try_collect::<Vec<_>>()
                        .await
                        .map(|v| v.into_iter().map(|c| c.to_string()).collect::<String>())
                        .unwrap_or_else(|e| format!("<failed to read logs: {}>", e));
                    Err(format!(
                        "pg_isready timeout in {} (last exit={:?}), logs:\n{}",
                        container, last_exit, logs
                    ))
                }
            };
            wait_ready(&old_container).await?;

            // Seed test data using the SAME shell-escape path the orchestrator uses.
            let seed_sql = "CREATE TABLE upgrade_probe(id SERIAL PRIMARY KEY, payload TEXT NOT NULL); \
                INSERT INTO upgrade_probe(payload) VALUES ('hello'), ('world'), ('with''apostrophe');";
            let seed_cmd = format!(
                "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -c {sql}",
                pw = shell_escape(password),
                u = shell_escape(user),
                d = shell_escape(db),
                sql = shell_escape(seed_sql),
            );
            run_exec_ok(&docker, &old_container, &seed_cmd, None).await?;

            // --- Whole-cluster coverage --------------------------------------
            // `pg_dumpall` dumps the ENTIRE cluster (every database + globals),
            // which is the whole reason the orchestrator uses it. Prove that:
            // create two databases besides the default one, give each a DISTINCT
            // set of tables, then after the restore assert (a) every database
            // came back and (b) each database's table count is identical before
            // vs after. The single-table probe above only proves one table in
            // one database survived — it would not catch a restore that silently
            // dropped a database or some of its tables.
            let extra_dbs = ["upgrade_extra_a", "upgrade_extra_b"];
            let all_dbs = [db, extra_dbs[0], extra_dbs[1]];

            // Each CREATE DATABASE must be its own statement: it cannot run
            // inside a transaction block, so it can't share a multi-statement -c.
            for extra in extra_dbs {
                let create_db = format!(
                    "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d postgres -c {sql}",
                    pw = shell_escape(password),
                    u = shell_escape(user),
                    sql = shell_escape(&format!("CREATE DATABASE {extra}")),
                );
                run_exec_ok(&docker, &old_container, &create_db, Some(password)).await?;
            }

            // Seed a distinct number of tables (each with a row) per database.
            // The default `upgradetest` already has `upgrade_probe`, so +1 => 2
            // tables; extra_a gets 3; extra_b gets 5 — all distinct, so a
            // per-database comparison is a real signal, not a coincidence.
            let seed_plan = [(db, 1usize), (extra_dbs[0], 3), (extra_dbs[1], 5)];
            for (dbname, n) in seed_plan {
                let mut stmts = String::new();
                for i in 0..n {
                    stmts.push_str(&format!(
                        "CREATE TABLE seed_{i}(id INT PRIMARY KEY, note TEXT NOT NULL); \
                         INSERT INTO seed_{i}(id, note) VALUES ({i}, 'r{i}'); "
                    ));
                }
                let seed_more = format!(
                    "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -c {sql}",
                    pw = shell_escape(password),
                    u = shell_escape(user),
                    d = shell_escape(dbname),
                    sql = shell_escape(&stmts),
                );
                run_exec_ok(&docker, &old_container, &seed_more, Some(password)).await?;
            }

            // Count base tables in a database of a given container. Parses the
            // first integer line so an incidental psql notice on stderr can't
            // corrupt the result. Mirrors the `wait_ready` closure's shape:
            // owned copies moved into the future so there are no borrow snags.
            let count_tables = |container: &str, dbname: &str| {
                let docker = Arc::clone(&docker);
                let container = container.to_string();
                let dbname = dbname.to_string();
                let user = user.to_string();
                let password = password.to_string();
                async move {
                    let sql = "SELECT count(*) FROM information_schema.tables \
                               WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                               AND table_type = 'BASE TABLE'";
                    let cmd = format!(
                        "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -tAc {sql}",
                        pw = shell_escape(&password),
                        u = shell_escape(&user),
                        d = shell_escape(&dbname),
                        sql = shell_escape(sql),
                    );
                    let out =
                        run_exec_capture(&docker, &container, &cmd, Some(password.as_str())).await?;
                    out.lines()
                        .filter_map(|l| l.trim().parse::<i64>().ok())
                        .next()
                        .ok_or_else(|| format!("no table count for {dbname} (raw output: {out:?})"))
                }
            };

            // Snapshot every database's table count BEFORE the dump.
            let mut before_counts: Vec<(&str, i64)> = Vec::new();
            for dbname in all_dbs {
                before_counts.push((dbname, count_tables(&old_container, dbname).await?));
            }

            // Dump — mirrors exactly what `phase_dump` shells out, including
            // the atomic marker file write.
            let dump_cmd = format!(
                "set -eu; pg_dumpall -U {u} --clean --if-exists > /dump/data.sql && sync && : > /dump/.done.tmp && mv /dump/.done.tmp /dump/.done",
                u = shell_escape(user),
            );
            run_exec_ok(&docker, &old_container, &dump_cmd, Some(password)).await?;

            // Verify marker file exists via a throwaway busybox sidecar,
            // matching `dump_marker_present`.
            let marker_cfg = ContainerCreateBody {
                image: Some("busybox:latest".to_string()),
                cmd: Some(vec![
                    "sh".into(),
                    "-c".into(),
                    "test -f /dump/.done".into(),
                ]),
                host_config: Some(HostConfig {
                    mounts: Some(vec![Mount {
                        target: Some("/dump".to_string()),
                        source: Some(dump_volume.clone()),
                        typ: Some(MountTypeEnum::VOLUME),
                        read_only: Some(true),
                        ..Default::default()
                    }]),
                    auto_remove: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            };
            // Use auto_remove=false so inspection works reliably.
            let marker_cfg = ContainerCreateBody {
                host_config: Some(HostConfig {
                    auto_remove: Some(false),
                    ..marker_cfg.host_config.clone().unwrap_or_default()
                }),
                ..marker_cfg
            };
            let marker = docker
                .create_container(None::<CreateContainerOptions>, marker_cfg)
                .await
                .map_err(|e| format!("create marker: {}", e))?;
            docker
                .start_container(&marker.id, None::<StartContainerOptions>)
                .await
                .map_err(|e| format!("start marker: {}", e))?;
            let marker_deadline = Instant::now() + Duration::from_secs(30);
            let marker_exit = loop {
                if Instant::now() > marker_deadline {
                    break -1;
                }
                if let Ok(insp) = docker
                    .inspect_container(&marker.id, None::<InspectContainerOptions>)
                    .await
                {
                    if let Some(s) = insp.state.as_ref() {
                        if s.running == Some(false) {
                            break s.exit_code.unwrap_or(-1);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            let _ = docker
                .remove_container(
                    &marker.id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            if marker_exit != 0 {
                return Err(format!("marker file missing (busybox exit {})", marker_exit));
            }

            // Stop OLD so it doesn't conflict with NEW on name resolution.
            let _ = docker
                .stop_container(&old_container, None::<StopContainerOptions>)
                .await;

            // Start NEW (v17) on a fresh empty data volume, same creds.
            let new_cfg = ContainerCreateBody {
                image: Some("postgres:17-bookworm".to_string()),
                env: Some(env_vars),
                host_config: Some(HostConfig {
                    mounts: Some(vec![Mount {
                        target: Some("/var/lib/postgresql/data".to_string()),
                        source: Some(new_data_volume.clone()),
                        typ: Some(MountTypeEnum::VOLUME),
                        ..Default::default()
                    }]),
                    network_mode: Some(network_name.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let created_new = docker
                .create_container(
                    Some(
                        CreateContainerOptionsBuilder::new()
                            .name(&new_container)
                            .build(),
                    ),
                    new_cfg,
                )
                .await
                .map_err(|e| format!("create new: {}", e))?;
            docker
                .start_container(&created_new.id, None::<StartContainerOptions>)
                .await
                .map_err(|e| format!("start new: {}", e))?;
            wait_ready(&new_container).await?;

            // Restore via throwaway psql sidecar — same shape as `phase_restore`.
            let sed_program = format!(
                "/^(DROP ROLE (IF EXISTS )?|CREATE ROLE |ALTER ROLE ){u}($| |;)/d",
                u = regex_escape_for_sed(user),
            );
            let sed_filter = format!("sed -E {}", shell_escape(&sed_program));
            let psql_cmd = format!(
                "set -eu; export PGPASSWORD={pw}; {sed} /dump/data.sql | psql -v ON_ERROR_STOP=1 -h {host} -U {u} -d postgres",
                pw = shell_escape(password),
                sed = sed_filter,
                host = shell_escape(&new_container),
                u = shell_escape(user),
            );
            let _ = db; // target DB name comes from the dump's `\c` directives
            let restorer_cfg = ContainerCreateBody {
                image: Some("postgres:17-bookworm".to_string()),
                entrypoint: Some(vec!["sh".into(), "-c".into(), psql_cmd]),
                host_config: Some(HostConfig {
                    mounts: Some(vec![Mount {
                        target: Some("/dump".to_string()),
                        source: Some(dump_volume.clone()),
                        typ: Some(MountTypeEnum::VOLUME),
                        read_only: Some(true),
                        ..Default::default()
                    }]),
                    network_mode: Some(network_name.clone()),
                    // Keep the container around on exit so we can wait/inspect/read logs.
                    auto_remove: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            };
            // `wait_ready` above confirms `pg_isready` succeeds via `docker
            // exec` inside the new container, but that doesn't guarantee the
            // container's network-facing TCP listener is attached yet — under
            // heavy concurrent Docker load (this test's CI job runs many
            // containers in parallel) the restorer, a *separate* container
            // connecting over the network, can still hit "Connection
            // refused" in that brief window. Retry with a fresh restorer
            // container on that specific failure rather than failing outright.
            const MAX_RESTORER_ATTEMPTS: u32 = 3;
            let mut restore_exit;
            let mut restorer_logs;
            let mut attempt = 1;
            loop {
                let restorer_name = format!("temps_upgrade_test_restorer_{}_{}", run_id, attempt);
                let restorer = docker
                    .create_container(
                        Some(
                            CreateContainerOptionsBuilder::new()
                                .name(&restorer_name)
                                .build(),
                        ),
                        restorer_cfg.clone(),
                    )
                    .await
                    .map_err(|e| format!("create restorer: {}", e))?;
                docker
                    .start_container(&restorer.id, None::<StartContainerOptions>)
                    .await
                    .map_err(|e| format!("start restorer: {}", e))?;

                // Poll until exit rather than using wait_container — the wait
                // API sometimes returns early with an empty error on
                // fast-exiting containers on macOS Docker Desktop.
                let exit_deadline = Instant::now() + Duration::from_secs(120);
                restore_exit = loop {
                    if Instant::now() > exit_deadline {
                        return Err("restorer exit wait timeout".into());
                    }
                    match docker
                        .inspect_container(&restorer.id, None::<InspectContainerOptions>)
                        .await
                    {
                        Ok(insp) => {
                            if let Some(state) = insp.state.as_ref() {
                                if state.running == Some(false) {
                                    break state.exit_code.unwrap_or(-1);
                                }
                            }
                        }
                        Err(e) => return Err(format!("inspect restorer: {}", e)),
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                };
                restorer_logs = docker
                    .logs(
                        &restorer.id,
                        Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()),
                    )
                    .try_collect::<Vec<_>>()
                    .await
                    .map(|v| v.into_iter().map(|c| c.to_string()).collect::<String>())
                    .unwrap_or_default();
                let _ = docker
                    .remove_container(
                        &restorer.id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;

                if restore_exit == 0
                    || attempt >= MAX_RESTORER_ATTEMPTS
                    || !restorer_logs.contains("Connection refused")
                {
                    break;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if restore_exit != 0 {
                return Err(format!(
                    "restorer exited with {}, logs:\n{}",
                    restore_exit, restorer_logs
                ));
            }

            // Verify seeded data survived into v17. This also confirms the
            // embedded apostrophe round-tripped through shell escaping.
            let verify_cmd = format!(
                "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d {d} -tAc \"SELECT string_agg(payload, ',' ORDER BY id) FROM upgrade_probe\"",
                pw = shell_escape(password),
                u = shell_escape(user),
                d = shell_escape(db),
            );
            let stdout =
                run_exec_capture(&docker, &new_container, &verify_cmd, Some(password)).await?;
            let got = stdout.trim();
            assert!(
                got.contains("hello") && got.contains("world") && got.contains("with'apostrophe"),
                "restored data did not match. got: {:?}",
                got
            );

            // Every database in the cluster must have come back.
            let db_list_sql = "SELECT datname FROM pg_database \
                               WHERE datistemplate = false AND datname <> 'postgres' \
                               ORDER BY datname";
            let db_list_cmd = format!(
                "export PGPASSWORD={pw}; psql -v ON_ERROR_STOP=1 -U {u} -d postgres -tAc {sql}",
                pw = shell_escape(password),
                u = shell_escape(user),
                sql = shell_escape(db_list_sql),
            );
            let db_list_out =
                run_exec_capture(&docker, &new_container, &db_list_cmd, Some(password)).await?;
            let restored_dbs: std::collections::HashSet<String> = db_list_out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            for dbname in all_dbs {
                if !restored_dbs.contains(dbname) {
                    return Err(format!(
                        "database {dbname:?} missing after restore; restored databases = {restored_dbs:?}"
                    ));
                }
            }

            // Each database must have exactly as many tables as before the dump.
            for (dbname, before) in before_counts {
                let after = count_tables(&new_container, dbname).await?;
                if before <= 0 {
                    return Err(format!(
                        "precondition failed: {dbname:?} had no seeded tables before the dump"
                    ));
                }
                if before != after {
                    return Err(format!(
                        "table count for database {dbname:?} changed across the upgrade: \
                         before={before} after={after}"
                    ));
                }
            }

            // Run ANALYZE (same shape as phase_analyze) to confirm planner
            // stats refresh works on the restored DB.
            let analyze_cmd = format!(
                "export PGPASSWORD={pw}; psql -U {u} -d {d} -c 'ANALYZE;'",
                pw = shell_escape(password),
                u = shell_escape(user),
                d = shell_escape(db),
            );
            run_exec_ok(&docker, &new_container, &analyze_cmd, None).await?;

            Ok::<(), String>(())
        }
        .await;

        // Always clean up.
        cleanup(
            Arc::clone(&docker),
            old_container,
            new_container,
            old_data_volume,
            new_data_volume,
            dump_volume,
            network_name,
        )
        .await;

        if let Err(e) = result {
            panic!("integration test failed: {}", e);
        }
    }
}
