// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Contract for applying a release update to the running binary from the API.
//!
//! Lives in temps-core because the two crates involved must not depend on each
//! other: the implementation belongs to temps-cli (it owns the binary path, the
//! release downloader and the process lifecycle), while the HTTP surface
//! belongs to temps-config (`POST /settings/update`). The console registers the
//! implementation as a service; ConfigPlugin picks it up if present.
//!
//! **Why a capability instead of "just do it":** replacing the binary is only
//! half the job — the process then has to come back on the new one. That is
//! only true when something supervises it (systemd `Restart=always`, launchd
//! `KeepAlive`). Inside a container the binary lives in the image, so a swap is
//! discarded on the next container recreate and a restart returns to the OLD
//! version; run from a shell, exiting is simply permanent downtime. So the
//! capability is reported honestly up front, with the reason and the manual
//! command to run instead, rather than discovering it after the process is gone.

use crate::AvailableUpdate;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// File under the data dir recording the in-flight/last update attempt. Read
/// back on the next boot to report the outcome of an update that, by
/// definition, killed the process that started it.
pub const SELF_UPDATE_JOURNAL_FILE: &str = "self-update.json";

/// What (if anything) will restart the process after it exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorKind {
    /// Started by systemd (detected via `INVOCATION_ID`). The unit installed by
    /// `deploy.sh` carries `Restart=always`, so exiting restarts on the new binary.
    Systemd,
    /// Started by launchd (detected via `XPC_SERVICE_NAME`), the macOS install path.
    Launchd,
    /// Running inside a container. The binary comes from the image, so a
    /// self-update cannot survive a container recreate — never updatable.
    Container,
    /// No supervisor found: a foreground/manual `temps serve`. Exiting is downtime.
    None,
}

impl SupervisorKind {
    /// How the new binary gets picked up under this supervisor.
    ///
    /// Only systemd and launchd are known to relaunch the process, so only they
    /// get an automatic restart. Everything else installs and stays put: the
    /// operator restarts on their own schedule, which is strictly better than
    /// either refusing to install or exiting into an outage.
    pub fn restart_mode(self) -> SelfUpdateRestartMode {
        match self {
            Self::Systemd | Self::Launchd => SelfUpdateRestartMode::Automatic,
            Self::Container | Self::None => SelfUpdateRestartMode::Manual,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
            Self::Container => "container",
            Self::None => "none",
        }
    }
}

/// Why a one-click update is unavailable. Exactly one is reported — the most
/// fundamental blocker wins, so the operator fixes the real problem first
/// rather than clearing one only to hit the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdateBlocker {
    /// The operator started the server with `--disable-self-update`. Deliberate
    /// and NOT overridable from the API — see the module docs on the two levels.
    DisabledByFlag,
    /// Turned off in Settings (`self_update.enabled = false`). An admin can turn
    /// it back on in the UI.
    DisabledBySetting,
    /// This process does not manage the temps binary at all (e.g. the settings
    /// API running inside the standalone proxy), so there is nothing to replace.
    NotSupported,
    /// The binary (or its directory) is not writable by the server user.
    BinaryNotWritable,
    /// No release assets are published for this OS/arch.
    UnsupportedPlatform,
    /// Another update attempt is already running.
    InProgress,
}

/// What happens to the running process once the new binary is in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdateRestartMode {
    /// A supervisor will relaunch temps, so the update finishes by exiting.
    /// Costs a few seconds of downtime and completes without further action.
    Automatic,
    /// Nothing would relaunch temps, so it installs the binary and keeps
    /// running the OLD version until the operator restarts it themselves.
    /// No downtime, but the update is not live until they do.
    Manual,
}

impl SelfUpdateRestartMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
        }
    }
}

/// Where an in-flight update has got to. Polled by the console so a long
/// download shows progress instead of an indefinite spinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdatePhase {
    /// Nothing running.
    #[default]
    Idle,
    /// Resolving the target release from the release API.
    Resolving,
    /// Downloading the release tarball.
    Downloading,
    /// Checking the published SHA-256 and running `--version` on the new binary.
    Verifying,
    /// Swapping the binary on disk (previous one kept as a `.bak` sibling).
    Installing,
    /// Binary swapped; the new binary is running database migrations before
    /// temps restarts on it. Progress is available in `migrations_applied`,
    /// `migrations_total`, and `current_migration_name` on the capability.
    Migrating,
    /// Migrations done; the process is shutting down so the supervisor restarts it.
    Restarting,
    /// Binary swapped and migrations applied, but nothing will restart temps
    /// automatically — it is still serving the old version until the operator
    /// restarts it.
    PendingRestart,
    /// The attempt failed. `Failed` before the swap means the running binary was
    /// left untouched. `Failed` after the swap (during migrations) means the new
    /// binary is on disk but the schema may be partially migrated — see `error`
    /// for recovery instructions including the `.bak` path.
    Failed,
}

impl SelfUpdatePhase {
    /// Is an attempt currently occupying the updater?
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Resolving
                | Self::Downloading
                | Self::Verifying
                | Self::Installing
                | Self::Migrating
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Resolving => "resolving",
            Self::Downloading => "downloading",
            Self::Verifying => "verifying",
            Self::Installing => "installing",
            Self::Migrating => "migrating",
            Self::Restarting => "restarting",
            Self::PendingRestart => "pending_restart",
            Self::Failed => "failed",
        }
    }
}

/// Outcome of an update attempt, as persisted in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdateStatus {
    /// Binary swapped, process exiting — written just before shutdown and
    /// resolved on the next boot by comparing the running version to the target.
    Pending,
    /// The process came back on the target version.
    Succeeded,
    /// The new binary is on disk, but temps is still running the old one
    /// because nothing restarts it automatically. Resolves to `Succeeded` on
    /// the next boot; stays here (never fails) until the operator restarts.
    InstalledPendingRestart,
    /// The attempt failed before the swap, or the process came back on the old
    /// version despite a completed swap.
    Failed,
}

/// A single update attempt. Persisted to `<data_dir>/self-update.json` so the
/// result survives the restart it causes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SelfUpdateAttempt {
    /// Version the attempt started from.
    pub from_version: String,
    /// Version the attempt targeted. `None` if it failed before resolving one.
    pub to_version: Option<String>,
    pub status: SelfUpdateStatus,
    #[schema(value_type = String, format = DateTime, example = "2026-08-06T09:12:31Z")]
    pub started_at: DateTime<Utc>,
    /// When the outcome was decided. `None` while still `Pending`.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
    /// User who clicked the button. `None` for attempts started by the CLI.
    pub triggered_by_user_id: Option<i32>,
    /// Operator-facing failure reason. Always set when `status` is `Failed`.
    pub error: Option<String>,
    /// Where the replaced binary was kept, so a bad release can be reverted by
    /// hand (`mv <path> <binary>`). Set once the swap completes.
    pub previous_binary_path: Option<String>,
    /// Number of database migrations that were successfully applied during
    /// this attempt. Set at completion (success or migration failure). `None`
    /// if migrations were never reached (pre-swap failure).
    #[serde(default)]
    pub migrations_applied: Option<u32>,
    /// Total number of database migrations that were planned. Set at the same
    /// time as `migrations_applied`. `None` if migrations were never reached.
    #[serde(default)]
    pub migrations_total: Option<u32>,
}

/// Everything the console needs to render the update control honestly: whether
/// it can run, why not, what to do instead, and how the last attempt went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SelfUpdateCapability {
    /// True only when a click would actually download, swap and come back up.
    pub can_apply: bool,
    /// Set iff `can_apply` is false.
    pub blocker: Option<SelfUpdateBlocker>,
    /// Operator-facing explanation of `blocker`, naming the specific thing that
    /// is wrong (path, supervisor, flag) rather than a generic refusal.
    pub reason: Option<String>,
    /// Command to run by hand instead. Always present — the manual path works
    /// even when the API path is blocked, so the operator is never stuck.
    pub manual_command: String,
    /// Version tag of the running binary, e.g. `v0.1.0-beta.55`. Always
    /// present — the version page needs it whether or not an update exists.
    pub current_version: String,
    /// Channel this install actually tracks, after applying the configured
    /// override or falling back to inference from `current_version`.
    pub channel: String,
    /// Whether `channel` came from settings (`true`) or was inferred from the
    /// running version tag (`false`). The UI shows inference as the default
    /// rather than as an explicit choice the operator made.
    pub channel_is_pinned: bool,
    pub supervisor: SupervisorKind,
    /// Whether applying an update also restarts temps, or only installs the
    /// binary and leaves the restart to the operator.
    pub restart_mode: SelfUpdateRestartMode,
    /// Absolute path of the binary that would be replaced.
    pub binary_path: String,
    /// Something true about this topology that the operator must know *before*
    /// clicking, even though it does not block the update — currently the
    /// split-topology case, where restarting the console leaves the separate
    /// proxy process on the old binary. Shown alongside the confirmation.
    pub caveat: Option<String>,
    /// Phase of the in-flight attempt (`idle` when none).
    pub phase: SelfUpdatePhase,
    /// Failure detail for `phase == failed`, before it is cleared by a retry.
    pub phase_error: Option<String>,
    /// The most recent attempt, including one resolved on this boot.
    pub last_attempt: Option<SelfUpdateAttempt>,
    /// Number of migrations applied so far. `Some` while `phase == migrating`.
    pub migrations_applied: Option<u32>,
    /// Total migrations to be applied. `Some` once the migrate child has
    /// reported its first `started` event.
    pub migrations_total: Option<u32>,
    /// Name of the migration currently running. `Some` while `phase == migrating`
    /// and a migration step is in flight.
    pub current_migration_name: Option<String>,
}

/// Accepted-update receipt returned by `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StartedSelfUpdate {
    /// Version the server is running right now.
    pub current_version: String,
    /// How long the console should expect to wait before the server answers
    /// again, so it can poll with a sensible timeout instead of guessing.
    /// Meaningless when `restart_mode` is `manual` — nothing goes down.
    pub estimated_restart_secs: u64,
    /// Echoed from the capability so the caller knows whether to expect a
    /// restart at all, without racing a second capability fetch.
    pub restart_mode: SelfUpdateRestartMode,
}

/// Outcome of an operator-triggered release check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseCheckResult {
    /// Channel that was queried.
    pub channel: String,
    /// Version tag of the running binary.
    pub current_version: String,
    /// Newest release published on that channel, if any could be resolved.
    pub latest_version: Option<String>,
    /// Release-notes page for `latest_version`.
    pub release_url: Option<String>,
    /// True when `latest_version` is strictly newer than what is running.
    /// False on a channel whose newest release is older — which is normal and
    /// expected right after switching a nightly box onto stable.
    pub update_available: bool,
}

/// The database-backed half of the update policy.
///
/// Passed in by the caller rather than read here: these live in the settings
/// table, which this trait deliberately knows nothing about. Bundled into one
/// struct so adding policy later doesn't churn every call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfUpdatePolicy {
    /// `self_update.enabled` — the console's soft off switch.
    pub enabled: bool,
    /// `self_update.channel` — `None` means infer from the running version.
    pub channel: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateError {
    #[error("Self-update is unavailable on this install: {reason}")]
    Unavailable {
        blocker: SelfUpdateBlocker,
        reason: String,
    },
    #[error("An update is already running (phase: {phase})")]
    AlreadyRunning { phase: &'static str },
    /// The pinned version is not a release tag this install will accept.
    /// Rejected before any work starts so the caller gets a straight answer
    /// instead of a background failure they have to go looking for.
    #[error("{reason}")]
    InvalidVersion { reason: String },
}

impl SelfUpdateError {
    /// The blocker behind this error, for mapping to a response body.
    pub fn blocker(&self) -> Option<SelfUpdateBlocker> {
        match self {
            Self::Unavailable { blocker, .. } => Some(*blocker),
            Self::AlreadyRunning { .. } => Some(SelfUpdateBlocker::InProgress),
            // Not a state of the install — a bad argument.
            Self::InvalidVersion { .. } => None,
        }
    }
}

/// Applies a published release to the running install.
///
/// Registered as a service by `temps serve` and consumed by the settings API.
/// Absent in hosts that cannot restart themselves meaningfully (e.g. the
/// standalone proxy), in which case the endpoint reports "not supported here".
#[async_trait::async_trait]
pub trait SelfUpdater: Send + Sync {
    /// Report whether an update can be applied right now, plus the version and
    /// channel information the console's version page renders.
    fn capability(&self, policy: &SelfUpdatePolicy) -> SelfUpdateCapability;

    /// Begin an update. Returns as soon as the attempt is accepted — the
    /// download and swap continue in the background, observable through
    /// `capability().phase`. Under `Automatic` restart mode the process then
    /// exits and the outcome is read from the journal on the next boot; under
    /// `Manual` it finishes at `PendingRestart` with temps still serving the
    /// old binary.
    ///
    /// `target_version` pins a specific tag; `None` takes the newest release on
    /// this install's channel.
    fn start(
        &self,
        target_version: Option<String>,
        triggered_by_user_id: Option<i32>,
        policy: &SelfUpdatePolicy,
    ) -> Result<StartedSelfUpdate, SelfUpdateError>;

    /// Query the release API right now instead of waiting for the background
    /// notifier's next pass, and republish the shared update slot from the
    /// result (including clearing it when nothing newer exists).
    ///
    /// `channel` is the configured override; `None` infers from the running
    /// version. Errors are returned as operator-facing strings — a failed
    /// check is a network problem to show, not a domain error to model.
    async fn check_now(&self, channel: Option<String>) -> Result<ReleaseCheckResult, String>;

    /// The update the last successful check found, if it is still current.
    fn available_update(&self) -> Option<AvailableUpdate>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_only_real_supervisors_restart_automatically() {
        assert_eq!(
            SupervisorKind::Systemd.restart_mode(),
            SelfUpdateRestartMode::Automatic
        );
        assert_eq!(
            SupervisorKind::Launchd.restart_mode(),
            SelfUpdateRestartMode::Automatic
        );
        // Neither of these is known to relaunch temps, so the update must
        // install and stop rather than exit into an outage.
        assert_eq!(
            SupervisorKind::Container.restart_mode(),
            SelfUpdateRestartMode::Manual
        );
        assert_eq!(
            SupervisorKind::None.restart_mode(),
            SelfUpdateRestartMode::Manual
        );
    }

    #[test]
    fn test_only_pre_restart_phases_are_active() {
        for phase in [
            SelfUpdatePhase::Resolving,
            SelfUpdatePhase::Downloading,
            SelfUpdatePhase::Verifying,
            SelfUpdatePhase::Installing,
            // Migrating is active: a second trigger while migrations are running
            // would race the same binary and may corrupt the schema.
            SelfUpdatePhase::Migrating,
        ] {
            assert!(phase.is_active(), "{phase:?} should occupy the updater");
        }
        // Restarting is deliberately NOT active: the swap is done and the
        // process is on its way out, so a second attempt has nothing to race.
        assert!(!SelfUpdatePhase::Restarting.is_active());
        // Likewise once installed — the binary is in place and the updater is
        // free; only the operator's restart is outstanding.
        assert!(!SelfUpdatePhase::PendingRestart.is_active());
        assert!(!SelfUpdatePhase::Idle.is_active());
        assert!(!SelfUpdatePhase::Failed.is_active());
    }

    #[test]
    fn test_already_running_maps_to_in_progress_blocker() {
        let err = SelfUpdateError::AlreadyRunning {
            phase: "downloading",
        };
        assert_eq!(err.blocker(), Some(SelfUpdateBlocker::InProgress));
    }

    #[test]
    fn test_invalid_version_is_not_an_install_blocker() {
        // A bad argument says nothing about the install's capability, so it
        // must not be reported as one (it maps to 400, not 409).
        let err = SelfUpdateError::InvalidVersion {
            reason: "bad tag".to_string(),
        };
        assert_eq!(err.blocker(), None);
    }

    #[test]
    fn test_attempt_roundtrips_through_json() {
        // The journal is written by one process and read by its successor, so
        // the on-disk shape must survive a serialize/deserialize round trip.
        let attempt = SelfUpdateAttempt {
            from_version: "v0.1.0".to_string(),
            to_version: Some("v0.2.0".to_string()),
            status: SelfUpdateStatus::Pending,
            started_at: Utc::now(),
            finished_at: None,
            triggered_by_user_id: Some(7),
            error: None,
            previous_binary_path: Some("/usr/local/bin/temps.bak".to_string()),
            migrations_applied: Some(3),
            migrations_total: Some(5),
        };
        let json = serde_json::to_string(&attempt).expect("serialize");
        let parsed: SelfUpdateAttempt = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, attempt);
    }

    #[test]
    fn test_attempt_with_no_migration_fields_roundtrips_through_json() {
        // Existing journals on disk written before the migration fields were
        // added must still deserialize correctly (missing fields → None).
        let json = r#"{
            "from_version": "v0.1.0",
            "to_version": "v0.2.0",
            "status": "succeeded",
            "started_at": "2026-08-11T10:00:00Z",
            "finished_at": "2026-08-11T10:01:00Z",
            "triggered_by_user_id": null,
            "error": null,
            "previous_binary_path": null
        }"#;
        let parsed: SelfUpdateAttempt = serde_json::from_str(json).expect("deserialize");
        assert_eq!(parsed.migrations_applied, None);
        assert_eq!(parsed.migrations_total, None);
    }
}
