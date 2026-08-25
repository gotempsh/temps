//! Applies a published release to the running install, on request from the API.
//!
//! This is the implementation behind `temps_core::SelfUpdater`; the HTTP surface
//! lives in temps-config (`GET/POST /settings/update`). It reuses the download,
//! checksum and atomic-swap machinery of `temps upgrade` — the only genuinely
//! new problems here are (a) deciding honestly whether the process will come
//! back after it exits, and (b) reporting the outcome of an operation that by
//! definition kills the process that started it.
//!
//! **How the restart works.** Nothing in temps restarts temps. Where a
//! supervisor is detected (systemd `Restart=always`, launchd `KeepAlive`) the
//! binary is swapped and the process exits, and the supervisor brings it back
//! on the new one. Where no supervisor is detected the binary is still
//! installed — the operator asked for the update and gets it — but the process
//! deliberately keeps running the OLD version instead of exiting into an
//! outage nothing would recover from. The capability says which of the two
//! will happen BEFORE the click, and the result says the update is not live
//! until temps is restarted. See `detect_supervisor` and `SelfUpdateRestartMode`.
//!
//! **How the outcome is reported.** The attempt is written to
//! `<data_dir>/self-update.json` as `pending` immediately before exiting, and
//! resolved on the next boot by comparing the running version against the
//! target (`reconcile_journal`). So the console can say "updated to v0.2.0" or
//! "came back on the old version" instead of the operator staring at a
//! reconnecting spinner with no idea what happened.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::Utc;
use temps_core::{
    AvailableUpdate, ReleaseCheckResult, SelfUpdateAttempt, SelfUpdateBlocker,
    SelfUpdateCapability, SelfUpdateError, SelfUpdatePhase, SelfUpdatePolicy,
    SelfUpdateRestartMode, SelfUpdateStatus, SelfUpdater, StartedSelfUpdate, SupervisorKind,
    UpdateStatusSlot, SELF_UPDATE_JOURNAL_FILE,
};
use tracing::{error, info, warn};

use crate::commands::upgrade::{
    check_write_permission, create_upgrade_temp_file, current_version_tag, download_asset_text,
    download_asset_to_file, extract_binary_from_tarball_file, fetch_latest_release_in_channel,
    fetch_specific_release, finalize_staged_binary, is_newer_version, platform_target,
    verify_computed_checksum, GitHubRelease, UpgradeChannel,
};

/// Grace period between accepting the update and exiting the process. Long
/// enough for the 202 response and one status poll to reach the console, so the
/// UI can show "restarting" instead of an unexplained connection drop.
const RESTART_GRACE: std::time::Duration = std::time::Duration::from_millis(1_500);

/// What the console should budget for the server to come back: the systemd unit
/// installed by `deploy.sh` uses `RestartSec=5`, plus boot (migrations, plugin
/// init). Advisory only — it drives the polling timeout, nothing else.
const ESTIMATED_RESTART_SECS: u64 = 45;

/// Bound on an operator-triggered check so a hung release API cannot leave the
/// request (and the spinner behind it) waiting indefinitely.
const CHECK_NOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Default)]
struct UpdaterState {
    phase: SelfUpdatePhase,
    phase_error: Option<String>,
    last_attempt: Option<SelfUpdateAttempt>,
    /// Live migration progress — populated while `phase == Migrating`.
    migrations_applied: Option<u32>,
    migrations_total: Option<u32>,
    current_migration_name: Option<String>,
}

/// Replaces the on-disk binary and exits so the supervisor restarts it.
pub struct BinarySelfUpdater {
    /// Binary that gets replaced — the running executable, symlinks resolved.
    binary_path: PathBuf,
    /// Directory holding the update journal.
    data_dir: PathBuf,
    /// `temps serve --disable-self-update`. A hard, API-proof off switch: unlike
    /// the settings toggle, nothing reachable over HTTP can clear it.
    disabled_by_flag: bool,
    supervisor: SupervisorKind,
    /// Split-topology note supplied by the caller, merged into `caveats()`.
    topology_caveat: Option<String>,
    /// Shared notice slot the background notifier also writes. An on-demand
    /// check republishes it so the banner and the version page never disagree.
    update_status: Arc<UpdateStatusSlot>,
    state: Arc<RwLock<UpdaterState>>,
    /// Database URL passed explicitly to the migrate child process so it works
    /// even when the parent was started with `--database-url` (not the env var).
    database_url: String,
}

impl BinarySelfUpdater {
    /// Build the updater and resolve any attempt left `pending` by the previous
    /// process. Never fails: a bad journal or an unresolvable binary path
    /// degrades to "self-update unavailable", which the capability endpoint
    /// reports with a reason.
    ///
    /// `database_url` is set explicitly on the migrate child process rather than
    /// relying on environment inheritance, because the parent may have received
    /// it as a `--database-url` flag (not in env) and a naively-spawned child
    /// would then hard-fail clap's required-arg check.
    pub fn new(
        data_dir: PathBuf,
        disabled_by_flag: bool,
        topology_caveat: Option<String>,
        update_status: Arc<UpdateStatusSlot>,
        database_url: String,
    ) -> Self {
        let binary_path = std::env::current_exe()
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .unwrap_or_else(|e| {
                warn!("Could not determine the running binary path: {}", e);
                PathBuf::new()
            });

        let supervisor = detect_supervisor();
        info!(
            supervisor = supervisor.as_str(),
            binary = %binary_path.display(),
            disabled_by_flag,
            "Self-update capability initialized"
        );

        let updater = Self {
            binary_path,
            data_dir,
            disabled_by_flag,
            supervisor,
            topology_caveat,
            update_status,
            state: Arc::new(RwLock::new(UpdaterState::default())),
            database_url,
        };

        let last_attempt = updater.reconcile_journal();
        if let Ok(mut state) = updater.state.write() {
            state.last_attempt = last_attempt;
        }
        updater
    }

    fn journal_path(&self) -> PathBuf {
        self.data_dir.join(SELF_UPDATE_JOURNAL_FILE)
    }

    /// Resolve an attempt the previous process left `pending`.
    ///
    /// The process that started the update cannot record its own outcome — it
    /// exits mid-flight by design. So its successor decides: if we are running
    /// the version the attempt targeted it succeeded, otherwise the swap did not
    /// take effect and the operator needs to know that (and where the previous
    /// binary was kept).
    fn reconcile_journal(&self) -> Option<SelfUpdateAttempt> {
        let path = self.journal_path();
        let mut attempt = match read_journal(&path) {
            Ok(attempt) => attempt?,
            Err(e) => {
                warn!(
                    "Ignoring unreadable self-update journal at {}: {}",
                    path.display(),
                    e
                );
                return None;
            }
        };

        // `InstalledPendingRestart` is resolvable too: the operator may be
        // restarting right now, which is exactly this boot.
        if !matches!(
            attempt.status,
            SelfUpdateStatus::Pending | SelfUpdateStatus::InstalledPendingRestart
        ) {
            return Some(attempt);
        }

        let running = current_version_tag();
        let awaiting_manual_restart = attempt.status == SelfUpdateStatus::InstalledPendingRestart;
        attempt.finished_at = Some(Utc::now());
        if attempt.to_version.as_deref() == Some(running.as_str()) {
            attempt.status = SelfUpdateStatus::Succeeded;
            info!(
                from = %attempt.from_version,
                to = %running,
                "Self-update completed: restarted on the new version"
            );
        } else if awaiting_manual_restart {
            // Still on the old binary, but nothing ever promised otherwise:
            // this install is waiting on a restart the operator hasn't done.
            // Calling that a failure would hide a pending upgrade, so the
            // attempt stays exactly where it is.
            attempt.finished_at = None;
            info!(
                installed = ?attempt.to_version,
                running = %running,
                "A self-update is installed and waiting for a restart"
            );
        } else {
            attempt.status = SelfUpdateStatus::Failed;
            let target = attempt
                .to_version
                .clone()
                .unwrap_or_else(|| "?".to_string());
            attempt.error = Some(format!(
                "Restarted on {running} instead of {target}. The binary swap did not take effect \
                 — the supervisor may have relaunched an older binary from a different path, or \
                 the new binary failed to start and was rolled back by the supervisor.{}",
                match &attempt.previous_binary_path {
                    Some(p) => format!(" The replaced binary was kept at {p}."),
                    None => String::new(),
                }
            ));
            error!(
                from = %attempt.from_version,
                target = %target,
                running = %running,
                "Self-update did not take effect"
            );
        }

        if let Err(e) = write_journal(&path, &attempt) {
            warn!("Could not persist resolved self-update journal: {}", e);
        }
        Some(attempt)
    }

    /// The command an operator runs by hand when the API path is unavailable.
    /// Always answerable — there is no state in which the manual path is gone.
    fn manual_command(&self, blocker: Option<SelfUpdateBlocker>) -> String {
        match blocker {
            Some(SelfUpdateBlocker::BinaryNotWritable) => "sudo temps upgrade".to_string(),
            _ if self.supervisor == SupervisorKind::Container => {
                "docker compose pull && docker compose up -d".to_string()
            }
            _ => "temps upgrade".to_string(),
        }
    }

    /// Everything true about this install that the operator should know before
    /// clicking, but that does not stop the update. Composed rather than
    /// single-valued because a split-topology console in a container has two
    /// separate things worth saying.
    fn caveats(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        match self.supervisor {
            SupervisorKind::Container => parts.push(
                "This server runs in a container. The new binary is written into the container's \
                 filesystem and survives a restart, but recreating the container (for example \
                 `docker compose up -d` after an image change) reverts it to the image's version \
                 — update your image tag for a durable upgrade."
                    .to_string(),
            ),
            SupervisorKind::None => parts.push(
                "No process supervisor was detected, so temps cannot restart itself. The new \
                 binary will be installed and temps will keep serving the current version until \
                 you restart it."
                    .to_string(),
            ),
            SupervisorKind::Systemd | SupervisorKind::Launchd => {}
        }
        if let Some(topology) = &self.topology_caveat {
            parts.push(topology.clone());
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// First blocker that applies, or `None` when an update can run.
    ///
    /// Order is deliberate: an explicit operator decision (`--disable-self-update`,
    /// then the settings toggle) is reported before any environmental problem,
    /// because "you turned this off" is the honest answer even on a host that
    /// also happens to be unsupervised.
    fn find_blocker(&self, policy: &SelfUpdatePolicy) -> Option<(SelfUpdateBlocker, String)> {
        if self.disabled_by_flag {
            return Some((
                SelfUpdateBlocker::DisabledByFlag,
                "The server was started with --disable-self-update. Restart it without that flag \
                 to allow updates from the console; this cannot be re-enabled from the UI."
                    .to_string(),
            ));
        }
        if !policy.enabled {
            return Some((
                SelfUpdateBlocker::DisabledBySetting,
                "One-click updates are turned off in Settings → Platform. An admin can re-enable \
                 them there."
                    .to_string(),
            ));
        }
        // NOTE: a missing supervisor is deliberately NOT a blocker. Installing
        // the binary is still exactly what the operator asked for; only the
        // restart is out of reach, and `restart_mode` says so up front.
        if let Err(e) = platform_target() {
            return Some((SelfUpdateBlocker::UnsupportedPlatform, e.to_string()));
        }
        if self.binary_path.as_os_str().is_empty() {
            return Some((
                SelfUpdateBlocker::BinaryNotWritable,
                "The path of the running binary could not be determined, so it cannot be replaced."
                    .to_string(),
            ));
        }
        if let Err(e) = check_write_permission(&self.binary_path) {
            return Some((
                SelfUpdateBlocker::BinaryNotWritable,
                format!(
                    "The server user cannot replace {}: {}",
                    self.binary_path.display(),
                    e
                ),
            ));
        }
        // Checked last: an in-flight attempt is transient, and reporting it
        // ahead of a permanent blocker would hide the real problem.
        let phase = self
            .state
            .read()
            .map(|s| s.phase)
            .unwrap_or(SelfUpdatePhase::Idle);
        if phase.is_active() {
            return Some((
                SelfUpdateBlocker::InProgress,
                format!("An update is already running ({}).", phase.as_str()),
            ));
        }
        None
    }

    /// Test-only: production code moves the phase through `try_claim` and the
    /// job itself, which owns the transitions after the claim succeeds.
    #[cfg(test)]
    fn set_phase(&self, phase: SelfUpdatePhase, phase_error: Option<String>) {
        if let Ok(mut state) = self.state.write() {
            state.phase = phase;
            state.phase_error = phase_error;
        }
    }

    /// Take exclusive ownership of the updater, or report who already has it.
    ///
    /// Tests and sets the phase in ONE lock acquisition. The equivalent check
    /// inside `find_blocker` is advisory only: two concurrent callers can both
    /// observe `Idle` there and both proceed, which would put two jobs on the
    /// same binary, backup and temp files — capable of leaving a truncated
    /// executable behind. This is the check that actually excludes them.
    fn try_claim(&self) -> Result<(), SelfUpdateError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.phase.is_active() {
            return Err(SelfUpdateError::AlreadyRunning {
                phase: state.phase.as_str(),
            });
        }
        state.phase = SelfUpdatePhase::Resolving;
        state.phase_error = None;
        Ok(())
    }
}

/// Resolve the channel to track: the operator's explicit setting, else the one
/// implied by the running version tag. An unparsable setting falls back to
/// inference rather than erroring, so a bad value can never wedge the checker.
pub(crate) fn resolve_channel(
    configured: Option<&str>,
    current_version: &str,
) -> (UpgradeChannel, bool) {
    match configured.and_then(UpgradeChannel::from_setting) {
        Some(channel) => (channel, true),
        None => (
            UpgradeChannel::for_installed_version(current_version),
            false,
        ),
    }
}

#[async_trait::async_trait]
impl SelfUpdater for BinarySelfUpdater {
    fn capability(&self, policy: &SelfUpdatePolicy) -> SelfUpdateCapability {
        let blocker = self.find_blocker(policy);
        let current_version = current_version_tag();
        // The capability also backs the version page, so it reports the
        // effective channel even when no update is pending.
        let (channel, pinned) = resolve_channel(policy.channel.as_deref(), &current_version);
        let (
            phase,
            phase_error,
            last_attempt,
            migrations_applied,
            migrations_total,
            current_migration_name,
        ) = match self.state.read() {
            Ok(state) => (
                state.phase,
                state.phase_error.clone(),
                state.last_attempt.clone(),
                state.migrations_applied,
                state.migrations_total,
                state.current_migration_name.clone(),
            ),
            Err(poisoned) => {
                let state = poisoned.into_inner();
                (
                    state.phase,
                    state.phase_error.clone(),
                    state.last_attempt.clone(),
                    state.migrations_applied,
                    state.migrations_total,
                    state.current_migration_name.clone(),
                )
            }
        };

        SelfUpdateCapability {
            can_apply: blocker.is_none(),
            blocker: blocker.as_ref().map(|(b, _)| *b),
            reason: blocker.as_ref().map(|(_, r)| r.clone()),
            manual_command: self.manual_command(blocker.as_ref().map(|(b, _)| *b)),
            current_version: current_version.clone(),
            channel: channel.as_str().to_string(),
            channel_is_pinned: pinned,
            supervisor: self.supervisor,
            restart_mode: self.supervisor.restart_mode(),
            binary_path: self.binary_path.display().to_string(),
            caveat: self.caveats(),
            phase,
            phase_error,
            last_attempt,
            migrations_applied,
            migrations_total,
            current_migration_name,
        }
    }

    fn start(
        &self,
        target_version: Option<String>,
        triggered_by_user_id: Option<i32>,
        policy: &SelfUpdatePolicy,
    ) -> Result<StartedSelfUpdate, SelfUpdateError> {
        if let Some((blocker, reason)) = self.find_blocker(policy) {
            if blocker == SelfUpdateBlocker::InProgress {
                let phase = self
                    .state
                    .read()
                    .map(|s| s.phase)
                    .unwrap_or(SelfUpdatePhase::Idle);
                return Err(SelfUpdateError::AlreadyRunning {
                    phase: phase.as_str(),
                });
            }
            return Err(SelfUpdateError::Unavailable { blocker, reason });
        }

        // Validate the pin BEFORE claiming the updater, so a typo fails the
        // request outright rather than occupying the updater and surfacing as
        // a background failure the caller has to go hunting for. This is also
        // the security boundary for the tag — see `normalize_release_tag`.
        let target_version = match target_version {
            Some(version) => Some(
                crate::commands::upgrade::normalize_release_tag(&version).map_err(|e| {
                    SelfUpdateError::InvalidVersion {
                        reason: e.to_string(),
                    }
                })?,
            ),
            None => None,
        };

        self.try_claim()?;

        let current_version = current_version_tag();

        let restart_mode = self.supervisor.restart_mode();
        let (channel, _) = resolve_channel(policy.channel.as_deref(), &current_version);
        let job = UpdateJob {
            binary_path: self.binary_path.clone(),
            journal_path: self.journal_path(),
            from_version: current_version.clone(),
            target_version,
            triggered_by_user_id,
            restart_mode,
            channel,
            state: self.state.clone(),
            database_url: self.database_url.clone(),
        };
        tokio::spawn(job.run());

        info!(
            user_id = ?triggered_by_user_id,
            from = %current_version,
            restart_mode = restart_mode.as_str(),
            "Self-update accepted; downloading in the background"
        );

        Ok(StartedSelfUpdate {
            current_version,
            // Nothing goes down in manual mode, so the caller has nothing to
            // wait out.
            estimated_restart_secs: match restart_mode {
                SelfUpdateRestartMode::Automatic => ESTIMATED_RESTART_SECS,
                SelfUpdateRestartMode::Manual => 0,
            },
            restart_mode,
        })
    }

    async fn check_now(&self, channel: Option<String>) -> Result<ReleaseCheckResult, String> {
        let current_version = current_version_tag();
        let (channel, _) = resolve_channel(channel.as_deref(), &current_version);

        let release =
            tokio::time::timeout(CHECK_NOW_TIMEOUT, fetch_latest_release_in_channel(channel))
                .await
                .map_err(|_| {
                    format!(
                        "Checking the {} channel timed out after {}s.",
                        channel.as_str(),
                        CHECK_NOW_TIMEOUT.as_secs()
                    )
                })?
                .map_err(|e| format!("Could not check the {} channel: {e}", channel.as_str()))?;

        let update_available = is_newer_version(&release.tag_name, &current_version);
        if update_available {
            self.update_status.set(AvailableUpdate {
                current_version: current_version.clone(),
                latest_version: release.tag_name.clone(),
                channel: channel.as_str().to_string(),
                release_url: release.html_url.clone(),
                checked_at: Utc::now(),
            });
        } else {
            // Clear rather than leave the previous notice: after switching from
            // nightly to stable the newest stable is OLDER, so it can never
            // overwrite the stale nightly notice and the banner would lie.
            self.update_status.clear();
        }

        info!(
            channel = channel.as_str(),
            current = %current_version,
            latest = %release.tag_name,
            update_available,
            "Release check requested from the console"
        );

        Ok(ReleaseCheckResult {
            channel: channel.as_str().to_string(),
            current_version,
            latest_version: Some(release.tag_name),
            release_url: Some(release.html_url),
            update_available,
        })
    }

    fn available_update(&self) -> Option<AvailableUpdate> {
        self.update_status.get()
    }
}

/// The background half of an update: everything between "accepted" and "the
/// process is gone". Owns clones rather than borrowing the updater so it can
/// outlive the request that started it.
struct UpdateJob {
    binary_path: PathBuf,
    journal_path: PathBuf,
    from_version: String,
    target_version: Option<String>,
    triggered_by_user_id: Option<i32>,
    restart_mode: SelfUpdateRestartMode,
    /// Channel the "newest release" lookup uses when no version is pinned.
    channel: UpgradeChannel,
    state: Arc<RwLock<UpdaterState>>,
    /// Passed explicitly to the `temps migrate` child so the DB URL is
    /// guaranteed to be in its environment regardless of how the parent
    /// received it (flag vs env var).
    database_url: String,
}

/// Failure discriminator for `UpdateJob::execute`.
///
/// The two variants have completely different semantics: a pre-swap failure
/// means the running binary was never touched, while a post-swap failure means
/// the new binary is on disk and the DB may be partially migrated — the error
/// message and journal entry must reflect that honestly.
#[derive(Debug)]
enum ExecuteError {
    /// Failed before the binary was replaced. The running binary is untouched.
    PreSwap(anyhow::Error),
    /// Binary was replaced with `to_version`, but `temps migrate` exited
    /// non-zero or could not be launched. The schema may be partially migrated.
    PostSwapMigration {
        to_version: String,
        backup_path: Option<String>,
        error: String,
        /// How many migrations completed before the failure, if known.
        migrations_applied: Option<u32>,
        /// Total migrations that were planned, if known.
        migrations_total: Option<u32>,
    },
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreSwap(e) => write!(f, "{e}"),
            Self::PostSwapMigration {
                to_version,
                backup_path,
                error,
                ..
            } => {
                write!(
                    f,
                    "The binary was replaced with {to_version} but database migrations failed: \
                     {error}. The schema may be partially migrated. {}Inspect the database and \
                     re-run `temps migrate` to complete the upgrade before restarting.",
                    backup_path
                        .as_ref()
                        .map(|p| format!(
                            "The previous binary was kept at {p} — restore it with \
                             `mv {p} <binary_path>` if needed. "
                        ))
                        .unwrap_or_default()
                )
            }
        }
    }
}

/// A single NDJSON progress line emitted by `temps migrate --progress-format=json`.
///
/// Field names must match what `print_progress_json` in `migrate.rs` emits exactly.
#[derive(serde::Deserialize)]
struct MigrateProgressLine {
    event: String,
    #[serde(default)]
    index: u32,
    #[serde(default)]
    total: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    success: bool,
    /// Present on `complete` events — total migrations applied in the run.
    #[serde(default)]
    migrations_applied: u32,
    /// Present on `finished` events where `success == false` — the migration
    /// runner's own error text (e.g. the underlying DB error). Without this,
    /// a migrate failure is reported as only "migrate exited with code N",
    /// leaving the operator to go hunting through logs for what actually
    /// broke.
    #[serde(default)]
    error: Option<String>,
}

impl UpdateJob {
    fn set_phase(&self, phase: SelfUpdatePhase, phase_error: Option<String>) {
        if let Ok(mut state) = self.state.write() {
            state.phase = phase;
            state.phase_error = phase_error;
        }
    }

    async fn run(self) {
        let started_at = Utc::now();
        match self.execute(started_at).await {
            Ok(target) => match self.restart_mode {
                SelfUpdateRestartMode::Automatic => {
                    // The swap is done, migrations applied, and the journal
                    // says `pending`. Exiting is now the whole job: the
                    // supervisor brings us back on the new binary and the next
                    // boot resolves the journal.
                    self.set_phase(SelfUpdatePhase::Restarting, None);
                    warn!(
                        from = %self.from_version,
                        to = %target,
                        "Binary replaced and migrations applied — exiting so the supervisor \
                         restarts temps on the new version"
                    );
                    tokio::time::sleep(RESTART_GRACE).await;
                    std::process::exit(0);
                }
                SelfUpdateRestartMode::Manual => {
                    // Installed and migrated, but exiting here would take the
                    // server down with nothing to bring it back. Keep serving
                    // the old binary and make the outstanding restart obvious.
                    self.set_phase(SelfUpdatePhase::PendingRestart, None);
                    warn!(
                        from = %self.from_version,
                        to = %target,
                        "Binary replaced and migrations applied, but no supervisor was detected \
                         — temps is STILL RUNNING {} and will pick up {} when you restart it",
                        self.from_version,
                        target
                    );
                }
            },
            Err(e) => {
                // `Display` is the single source of truth for the operator
                // -facing message in both variants — building it separately
                // here would drift from `ExecuteError`'s own wording.
                let message = e.to_string();
                match e {
                    ExecuteError::PreSwap(_) => {
                        error!(
                            from = %self.from_version,
                            error = %message,
                            "Self-update failed; the running binary was left untouched"
                        );
                        // Record the failure so it survives to the UI even if
                        // the operator's tab was closed when it happened.
                        let attempt = SelfUpdateAttempt {
                            from_version: self.from_version.clone(),
                            to_version: None,
                            status: SelfUpdateStatus::Failed,
                            started_at,
                            finished_at: Some(Utc::now()),
                            triggered_by_user_id: self.triggered_by_user_id,
                            error: Some(message.clone()),
                            previous_binary_path: None,
                            migrations_applied: None,
                            migrations_total: None,
                        };
                        if let Err(e) = write_journal(&self.journal_path, &attempt) {
                            warn!("Could not persist failed self-update attempt: {}", e);
                        }
                        if let Ok(mut state) = self.state.write() {
                            state.phase = SelfUpdatePhase::Failed;
                            state.phase_error = Some(message);
                            state.last_attempt = Some(attempt);
                        }
                    }
                    ExecuteError::PostSwapMigration {
                        to_version,
                        backup_path,
                        migrations_applied,
                        migrations_total,
                        ..
                    } => {
                        // The binary IS the new version on disk. `message`
                        // (built above from `Display`) never says "the
                        // running binary was left untouched" for this variant
                        // — it was replaced. The operator needs to know the
                        // exact state so they can decide whether to run
                        // `temps migrate` themselves before restarting.
                        error!(
                            from = %self.from_version,
                            to = %to_version,
                            error = %message,
                            "Binary swapped but migrations failed; schema may be partially migrated"
                        );
                        let attempt = SelfUpdateAttempt {
                            from_version: self.from_version.clone(),
                            to_version: Some(to_version),
                            status: SelfUpdateStatus::Failed,
                            started_at,
                            finished_at: Some(Utc::now()),
                            triggered_by_user_id: self.triggered_by_user_id,
                            error: Some(message.clone()),
                            previous_binary_path: backup_path,
                            migrations_applied,
                            migrations_total,
                        };
                        if let Err(e) = write_journal(&self.journal_path, &attempt) {
                            warn!("Could not persist failed self-update attempt: {}", e);
                        }
                        if let Ok(mut state) = self.state.write() {
                            // Clear live migration progress — it's now in the journal.
                            state.migrations_applied = None;
                            state.migrations_total = None;
                            state.current_migration_name = None;
                            state.phase = SelfUpdatePhase::Failed;
                            state.phase_error = Some(message);
                            state.last_attempt = Some(attempt);
                        }
                    }
                }
            }
        }
    }

    /// Download, verify, swap the binary and run database migrations.
    ///
    /// Everything up to and including `finalize_staged_binary` is the
    /// "pre-swap" zone: a failure there leaves the running binary untouched
    /// and is returned as `ExecuteError::PreSwap`. Once the binary is on
    /// disk, any failure is `ExecuteError::PostSwapMigration` — the operator
    /// is told exactly what state the install is in and what to do next.
    async fn execute(&self, started_at: chrono::DateTime<Utc>) -> Result<String, ExecuteError> {
        let (to_version, backup_path) = self
            .download_verify_swap()
            .await
            .map_err(ExecuteError::PreSwap)?;

        let backup_str = backup_path.map(|p| p.display().to_string());

        // Run the new binary's migrator so the schema is up to date before
        // anything restarts. This MUST use the new binary (not in-process) so
        // it applies migrations compiled into the new version, not the old one.
        let (migrations_applied, migrations_total) =
            self.run_migration_step(&to_version, &backup_str).await?;

        // Written BEFORE the exit: once the process is gone there is no second
        // chance to record what it was doing.
        let attempt = SelfUpdateAttempt {
            from_version: self.from_version.clone(),
            to_version: Some(to_version.clone()),
            // `Pending` means "we are about to exit, resolve this on the next
            // boot". In manual mode nothing exits, so the attempt is already
            // as finished as it can get until the operator restarts.
            status: match self.restart_mode {
                SelfUpdateRestartMode::Automatic => SelfUpdateStatus::Pending,
                SelfUpdateRestartMode::Manual => SelfUpdateStatus::InstalledPendingRestart,
            },
            started_at,
            finished_at: match self.restart_mode {
                SelfUpdateRestartMode::Automatic => None,
                SelfUpdateRestartMode::Manual => Some(Utc::now()),
            },
            triggered_by_user_id: self.triggered_by_user_id,
            error: None,
            previous_binary_path: backup_str,
            migrations_applied: Some(migrations_applied),
            migrations_total: Some(migrations_total),
        };
        write_journal(&self.journal_path, &attempt).map_err(ExecuteError::PreSwap)?;
        if let Ok(mut state) = self.state.write() {
            // Clear live migration progress now that it's recorded in the journal.
            state.migrations_applied = None;
            state.migrations_total = None;
            state.current_migration_name = None;
            state.last_attempt = Some(attempt);
        }

        Ok(to_version)
    }

    /// Everything from "resolve release" through "replace binary on disk".
    ///
    /// A failure at any point here leaves the running binary untouched and is
    /// safe to propagate as `PreSwap`. Returns `(to_version, backup_path)`.
    async fn download_verify_swap(&self) -> anyhow::Result<(String, Option<PathBuf>)> {
        let target = platform_target()?;

        let release = self.resolve_release().await?;
        let to_version = release.tag_name.clone();
        if to_version == self.from_version {
            return Err(anyhow::anyhow!(
                "Already running {} — nothing to update",
                self.from_version
            ));
        }

        let tarball_name = format!("temps-{}.tar.gz", target);
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == tarball_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Release {} publishes no asset for this platform ({}). Available: {}",
                    to_version,
                    target,
                    release
                        .assets
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;

        info!(
            to = %to_version,
            size_mb = format!("{:.1}", asset.size as f64 / 1_048_576.0),
            "Downloading release asset"
        );

        // Streamed through disk, exactly like `temps upgrade`: this runs
        // INSIDE the already-serving `temps serve` process, so buffering the
        // ~110 MB tarball and ~270 MB extracted binary in memory here is the
        // same swap-thrashing risk on a small host — arguably worse, since
        // the server is live and handling requests while it happens.
        let parent = self
            .binary_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine the binary's directory"))?
            .to_path_buf();

        self.set_phase(SelfUpdatePhase::Downloading, None);
        let mut download_file = create_upgrade_temp_file(&parent, ".temps-selfupdate-dl.")?;
        let download_path = download_file.path().to_path_buf();
        let computed = download_asset_to_file(
            &asset.browser_download_url,
            download_file.as_file_mut(),
            &download_path,
        )
        .await?;

        self.set_phase(SelfUpdatePhase::Verifying, None);
        // Fail closed on a missing checksum. `temps upgrade` merely warns
        // because a human is watching the terminal and can judge; a background
        // job triggered from a browser has nobody to make that call, so it must
        // never install bytes it could not verify.
        let checksum_asset = release
            .assets
            .iter()
            .find(|a| a.name == format!("{}.sha256", tarball_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Release {} publishes no SHA-256 for {}, so the download cannot be verified. \
                     Upgrade from the command line if you want to install it anyway.",
                    to_version,
                    tarball_name
                )
            })?;
        let expected = download_asset_text(&checksum_asset.browser_download_url).await?;
        verify_computed_checksum(&computed, &expected)?;

        let mut staged_file = create_upgrade_temp_file(&parent, ".temps-selfupdate-bin.")?;
        let staged_path = staged_file.path().to_path_buf();
        extract_binary_from_tarball_file(
            download_file.as_file_mut(),
            &download_path,
            staged_file.as_file_mut(),
            &staged_path,
        )?;

        // Preflight against the staged file itself rather than writing a
        // second full copy just to exec it.
        preflight_staged_binary(&staged_path, staged_file.as_file())?;

        self.set_phase(SelfUpdatePhase::Installing, None);
        // Keep the outgoing binary next to the new one so a release that boots
        // but misbehaves can be reverted with a single `mv`, without network.
        let backup_path = backup_current_binary(&self.binary_path)?;
        finalize_staged_binary(&self.binary_path, staged_file)?;

        Ok((to_version, backup_path))
    }

    /// Spawn `<new_binary> migrate --yes --progress-format=json`, stream its
    /// NDJSON stdout into the shared state for live polling, and return the
    /// applied/total migration counts on success.
    ///
    /// On any failure (launch error, non-zero exit) this returns
    /// `ExecuteError::PostSwapMigration` — the swap has already happened so the
    /// caller must NOT describe the error as "the running binary was untouched".
    async fn run_migration_step(
        &self,
        to_version: &str,
        backup_path: &Option<String>,
    ) -> Result<(u32, u32), ExecuteError> {
        use tokio::io::AsyncBufReadExt;

        self.set_phase(SelfUpdatePhase::Migrating, None);
        info!(
            to = %to_version,
            binary = %self.binary_path.display(),
            "Running database migrations with the new binary"
        );

        let post_swap_err = |error: String| ExecuteError::PostSwapMigration {
            to_version: to_version.to_string(),
            backup_path: backup_path.clone(),
            error,
            // Counts captured from state at the moment of failure.
            migrations_applied: self.state.read().ok().and_then(|s| s.migrations_applied),
            migrations_total: self.state.read().ok().and_then(|s| s.migrations_total),
        };

        let mut child = tokio::process::Command::new(&self.binary_path)
            .arg("migrate")
            .arg("--yes")
            .arg("--progress-format=json")
            // Explicit env set: if the parent received the DB URL as a
            // `--database-url` flag it is not in the environment, so a naive
            // child spawn would fail clap's required-arg check.
            .env("TEMPS_DATABASE_URL", &self.database_url)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| post_swap_err(format!("Failed to launch migrate subprocess: {e}")))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            post_swap_err("migrate subprocess stdout was not captured".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            post_swap_err("migrate subprocess stderr was not captured".to_string())
        })?;

        // Drain stderr concurrently with stdout. If nothing reads it, a child
        // that writes more than the OS pipe buffer (64KB on Linux) to stderr
        // blocks on the write while we block reading stdout — a deadlock ends
        // an update that would otherwise have succeeded.
        //
        // Piped rather than inherited so this can be logged deliberately: the
        // child has `TEMPS_DATABASE_URL` in its environment, and a
        // connection-failure message from sqlx/libpq can embed the full DSN
        // (including the password). Piping does NOT by itself keep the
        // credential out of the parent's own log stream — `warn!` below IS
        // the parent's log stream, same as `Stdio::inherit()` would have
        // been, just structured instead of raw. `redact_dsn` is what
        // actually prevents the leak, by stripping any `scheme://user:pass@`
        // prefix before the line is logged.
        let stderr_task = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    warn!(
                        target: "temps_cli::self_update::migrate_stderr",
                        "{}",
                        redact_dsn(&line)
                    );
                }
            }
        });

        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut applied: u32 = 0;
        let mut total: u32 = 0;
        // The migration that failed and why, if any `finished` event reports
        // `success: false` — carried into the final error so the operator
        // sees the actual DB failure instead of just an exit code.
        let mut last_failure: Option<(String, String)> = None;

        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(event) = serde_json::from_str::<MigrateProgressLine>(&line) {
                match event.event.as_str() {
                    "started" => {
                        total = event.total;
                        if let Ok(mut state) = self.state.write() {
                            state.migrations_total = Some(total);
                            state.current_migration_name = Some(event.name.clone());
                        }
                        info!(
                            index = event.index,
                            total = event.total,
                            name = %event.name,
                            "Applying migration"
                        );
                    }
                    "finished" => {
                        if event.success {
                            applied += 1;
                        } else {
                            last_failure = Some((
                                event.name.clone(),
                                event
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| "no error detail reported".to_string()),
                            ));
                        }
                        if let Ok(mut state) = self.state.write() {
                            state.migrations_applied = Some(applied);
                            state.current_migration_name = None;
                        }
                        info!(
                            index = event.index,
                            total = event.total,
                            name = %event.name,
                            success = event.success,
                            error = event.error.as_deref().unwrap_or(""),
                            "Migration step finished"
                        );
                    }
                    "complete" => {
                        // Authoritative count from the child — use it if
                        // higher than our per-event counter (defensive).
                        if event.migrations_applied > applied {
                            applied = event.migrations_applied;
                            if let Ok(mut state) = self.state.write() {
                                state.migrations_applied = Some(applied);
                            }
                        }
                    }
                    "up_to_date" => {
                        // No migrations needed — counts stay at zero.
                        info!("Database already up to date; no migrations to apply");
                    }
                    _ => {}
                }
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|e| post_swap_err(format!("Failed to wait for migrate subprocess: {e}")))?;
        // Exiting closes the child's stderr, so this drains any remaining
        // buffered lines and then returns; best-effort, a panicked drain task
        // must not fail an otherwise-successful migration.
        let _ = stderr_task.await;

        if !status.success() {
            let code = status.code().unwrap_or(-1);
            let error = match &last_failure {
                Some((name, detail)) => {
                    format!("migration '{name}' failed: {detail} (migrate exited with code {code})")
                }
                None => format!("migrate exited with code {code}"),
            };
            return Err(ExecuteError::PostSwapMigration {
                to_version: to_version.to_string(),
                backup_path: backup_path.clone(),
                error,
                migrations_applied: Some(applied),
                migrations_total: Some(total),
            });
        }

        info!(
            applied = applied,
            total = total,
            "Database migrations completed successfully"
        );
        Ok((applied, total))
    }

    /// The release to install: an explicit pin, or the newest one on the channel
    /// this install already tracks (inferred from the running version tag, the
    /// same rule the startup notifier uses).
    async fn resolve_release(&self) -> anyhow::Result<GitHubRelease> {
        match &self.target_version {
            Some(version) => fetch_specific_release(version).await,
            None => fetch_latest_release_in_channel(self.channel).await,
        }
    }
}

/// Strip `scheme://user:pass@` credentials from a line before it is logged.
///
/// Defense in depth for the `migrate` child's stderr: that child has
/// `TEMPS_DATABASE_URL` in its environment, and a connection-failure message
/// from sqlx/libpq is not guaranteed never to embed the DSN (including the
/// password) in its `Display` output. Piping stderr into `warn!` instead of
/// inheriting it only changes WHERE the line goes, not whether it contains a
/// credential — this is what actually keeps one out of the log.
fn redact_dsn(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains("://") {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    let mut redacted_any = false;
    while let Some(scheme_pos) = rest.find("://") {
        let (before, after_marker) = rest.split_at(scheme_pos + 3);
        result.push_str(before);
        // The userinfo component, if present, ends at the next '/' or
        // whitespace — scan only that span for a trailing '@'.
        let boundary = after_marker
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(after_marker.len());
        let candidate = &after_marker[..boundary];
        if let Some(at_pos) = candidate.rfind('@') {
            result.push_str("***@");
            redacted_any = true;
            rest = &after_marker[at_pos + 1..];
        } else {
            result.push_str(candidate);
            rest = &after_marker[boundary..];
        }
    }
    result.push_str(rest);
    if redacted_any {
        std::borrow::Cow::Owned(result)
    } else {
        std::borrow::Cow::Borrowed(line)
    }
}

/// Is this process running inside a container? Checked before anything else,
/// because a swapped binary there is silently discarded on the next recreate.
fn in_container() -> bool {
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return true;
    }
    // Fallback for runtimes that create neither marker file.
    std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
        cgroup.contains("/docker/")
            || cgroup.contains("/docker-")
            || cgroup.contains("containerd")
            || cgroup.contains("kubepods")
            || cgroup.contains("/lxc/")
    })
}

/// Identify what will restart this process after it exits.
///
/// Both signals are set by the supervisor itself in the child's environment,
/// so they cannot be faked by a wrong assumption about how temps was launched:
/// systemd exports `INVOCATION_ID`, launchd exports `XPC_SERVICE_NAME`.
fn detect_supervisor() -> SupervisorKind {
    if in_container() {
        return SupervisorKind::Container;
    }
    if std::env::var_os("INVOCATION_ID").is_some() && running_under_systemd_service() {
        return SupervisorKind::Systemd;
    }
    // launchd sets this for managed jobs; processes started from a terminal
    // inherit the literal "0", which means "not a launchd service".
    if std::env::var("XPC_SERVICE_NAME").is_ok_and(|v| v != "0") {
        return SupervisorKind::Launchd;
    }
    SupervisorKind::None
}

/// Corroborate `INVOCATION_ID` against the cgroup this process actually lives in.
///
/// `INVOCATION_ID` is inherited, so a shell started inside a unit — or anything
/// launched via `systemd-run` — carries it without being a restartable service.
/// Believing it there would make temps exit expecting a restart that never
/// comes, i.e. turn an update into an outage. A real service lives in a
/// `*.service` cgroup, so require that too. If the cgroup cannot be read (not
/// Linux, or an unusual mount), fall back to trusting the env var rather than
/// downgrading a legitimate systemd install to manual restarts.
fn running_under_systemd_service() -> bool {
    match std::fs::read_to_string("/proc/self/cgroup") {
        Ok(cgroup) => cgroup.lines().any(|line| {
            line.rsplit('/')
                .next()
                .is_some_and(|unit| unit.ends_with(".service"))
        }),
        Err(_) => true,
    }
}

/// Run the staged binary's `--version` before trusting it.
///
/// Catches the failures a checksum cannot: a build for the wrong libc, a
/// missing shared library, a corrupted extraction. Cheap insurance — without
/// it, a binary that cannot exec turns a one-click update into an outage that
/// only a console on the host can fix.
///
/// Runs directly against the already-staged extraction file (the same one
/// `finalize_staged_binary` will persist) instead of writing a second full
/// copy just to exec it — that second copy is exactly the kind of extra
/// in-memory/on-disk buffering the streaming rewrite of this flow removed
/// everywhere else.
fn preflight_staged_binary(staged_path: &Path, staged_file: &std::fs::File) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged_file
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|e| anyhow::anyhow!("Failed to make the staged binary executable: {}", e))?;
    }

    let output = std::process::Command::new(staged_path)
        .arg("--version")
        .output();

    let output = output.map_err(|e| {
        anyhow::anyhow!(
            "The downloaded binary could not be executed on this host: {}. \
             The running version was left untouched.",
            e
        )
    })?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "The downloaded binary exited with {} when asked for its version: {}. \
             The running version was left untouched.",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Copy the current binary aside as `<binary>.bak`, returning where it went.
///
/// A copy (not a rename) so the target is never momentarily absent — if the
/// process dies between the two steps, the install is still intact. Best
/// effort: failing to keep a backup must not block an otherwise valid update,
/// since the release remains downloadable.
fn backup_current_binary(binary_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let backup_path = binary_path.with_extension("bak");
    match std::fs::copy(binary_path, &backup_path) {
        Ok(_) => {
            info!(
                "Previous binary kept at {} — restore it with `mv {} {}` if the new version misbehaves",
                backup_path.display(),
                backup_path.display(),
                binary_path.display()
            );
            Ok(Some(backup_path))
        }
        Err(e) => {
            warn!(
                "Could not keep a backup of the current binary at {}: {}",
                backup_path.display(),
                e
            );
            Ok(None)
        }
    }
}

fn read_journal(path: &Path) -> anyhow::Result<Option<SelfUpdateAttempt>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
    let attempt = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))?;
    Ok(Some(attempt))
}

fn write_journal(path: &Path, attempt: &SelfUpdateAttempt) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create {}: {}", parent.display(), e))?;
    }
    let json = serde_json::to_string_pretty(attempt)
        .map_err(|e| anyhow::anyhow!("Failed to serialize the update journal: {}", e))?;
    std::fs::write(path, json)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_dsn_strips_credentials() {
        let line = "error connecting to postgresql://app:hunter2@db:5432/temps: connection refused";
        let redacted = redact_dsn(line);
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(!redacted.contains("app:"), "{redacted}");
        assert!(redacted.contains("***@db:5432/temps"), "{redacted}");
    }

    #[test]
    fn test_redact_dsn_leaves_credential_free_lines_untouched() {
        let line = "FATAL: password authentication failed for user \"app\"";
        assert_eq!(redact_dsn(line), line);
    }

    #[test]
    fn test_redact_dsn_leaves_url_without_userinfo_untouched() {
        let line = "fetching https://example.com/releases/latest";
        assert_eq!(redact_dsn(line), line);
    }

    #[test]
    fn test_redact_dsn_handles_multiple_urls_in_one_line() {
        let line = "postgres://a:b@host1/db and postgres://c:d@host2/db";
        let redacted = redact_dsn(line);
        assert!(!redacted.contains("a:b"), "{redacted}");
        assert!(!redacted.contains("c:d"), "{redacted}");
        assert_eq!(
            redacted,
            "postgres://***@host1/db and postgres://***@host2/db"
        );
    }

    fn updater(
        dir: &Path,
        disabled_by_flag: bool,
        supervisor: SupervisorKind,
    ) -> BinarySelfUpdater {
        BinarySelfUpdater {
            // A path inside the temp dir: present and writable, so the write
            // check passes and the test exercises the blocker being asserted.
            binary_path: dir.join("temps"),
            data_dir: dir.to_path_buf(),
            disabled_by_flag,
            supervisor,
            topology_caveat: None,
            update_status: Arc::new(UpdateStatusSlot::new()),
            state: Arc::new(RwLock::new(UpdaterState::default())),
            // Tests don't exercise the migrate subprocess, so the URL value
            // doesn't matter — it only needs to be present in the struct.
            database_url: String::new(),
        }
    }

    /// Settings-backed policy for tests: enabled/disabled, channel inferred.
    fn policy(enabled: bool) -> SelfUpdatePolicy {
        SelfUpdatePolicy {
            enabled,
            channel: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("temps-self-update-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("temps"), b"#!/bin/sh\n").expect("write fake binary");
        dir
    }

    #[test]
    fn test_flag_blocks_even_when_everything_else_is_fine() {
        let dir = temp_dir("flag");
        let cap = updater(&dir, true, SupervisorKind::Systemd).capability(&policy(true));
        assert!(!cap.can_apply);
        assert_eq!(cap.blocker, Some(SelfUpdateBlocker::DisabledByFlag));
        // The reason must say the flag cannot be cleared from the UI, or an
        // admin will hunt for a toggle that does not exist.
        assert!(cap.reason.unwrap().contains("--disable-self-update"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_flag_wins_over_settings_toggle() {
        // Both off: the flag is the one the operator cannot undo in the UI, so
        // it must be the reported blocker.
        let dir = temp_dir("flag-wins");
        let cap = updater(&dir, true, SupervisorKind::Systemd).capability(&policy(false));
        assert_eq!(cap.blocker, Some(SelfUpdateBlocker::DisabledByFlag));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_settings_toggle_blocks() {
        let dir = temp_dir("setting");
        let cap = updater(&dir, false, SupervisorKind::Systemd).capability(&policy(false));
        assert!(!cap.can_apply);
        assert_eq!(cap.blocker, Some(SelfUpdateBlocker::DisabledBySetting));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_container_installs_with_a_caveat_instead_of_being_refused() {
        let dir = temp_dir("container");
        let cap = updater(&dir, false, SupervisorKind::Container).capability(&policy(true));
        // The operator asked for the binary to be updated; a container is a
        // reason to warn, not to refuse.
        assert!(cap.can_apply, "blocked by {:?}", cap.blocker);
        assert_eq!(cap.restart_mode, SelfUpdateRestartMode::Manual);
        // ...but they must be told the swap does not survive a recreate.
        let caveat = cap.caveat.expect("container needs a caveat");
        assert!(
            caveat.contains("recreating") || caveat.contains("recreate"),
            "{caveat}"
        );
        // The durable fix points at the image, not `temps upgrade`.
        assert!(cap.manual_command.contains("docker"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unsupervised_installs_without_restarting() {
        let dir = temp_dir("unsupervised");
        let cap = updater(&dir, false, SupervisorKind::None).capability(&policy(true));
        // Installing still works with no supervisor — only the restart is out
        // of reach, and that is stated rather than used as a refusal.
        assert!(cap.can_apply, "blocked by {:?}", cap.blocker);
        assert_eq!(cap.blocker, None);
        assert_eq!(cap.restart_mode, SelfUpdateRestartMode::Manual);
        let caveat = cap.caveat.expect("manual restart needs a caveat");
        assert!(caveat.contains("restart it"), "{caveat}");
        assert_eq!(cap.manual_command, "temps upgrade");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_supervised_install_restarts_automatically_and_has_no_caveat() {
        let dir = temp_dir("supervised-mode");
        let cap = updater(&dir, false, SupervisorKind::Systemd).capability(&policy(true));
        assert_eq!(cap.restart_mode, SelfUpdateRestartMode::Automatic);
        assert_eq!(cap.caveat, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_split_topology_caveat_is_kept_alongside_the_restart_caveat() {
        let dir = temp_dir("caveats");
        let mut u = updater(&dir, false, SupervisorKind::None);
        u.topology_caveat = Some("Console only; restart the proxy too.".to_string());
        let caveat = u.capability(&policy(true)).caveat.expect("both caveats");
        assert!(caveat.contains("restart it"), "{caveat}");
        assert!(caveat.contains("restart the proxy too"), "{caveat}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_supervised_install_can_apply() {
        let dir = temp_dir("ok");
        let cap = updater(&dir, false, SupervisorKind::Systemd).capability(&policy(true));
        assert!(
            cap.can_apply,
            "blocked by {:?}: {:?}",
            cap.blocker, cap.reason
        );
        assert_eq!(cap.blocker, None);
        assert_eq!(cap.phase, SelfUpdatePhase::Idle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_start_refused_when_disabled_reports_blocker() {
        let dir = temp_dir("start-refused");
        let err = updater(&dir, false, SupervisorKind::Systemd)
            .start(None, Some(1), &policy(false))
            .expect_err("must refuse when disabled in settings");
        assert_eq!(err.blocker(), Some(SelfUpdateBlocker::DisabledBySetting));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_start_rejects_a_bad_version_without_claiming_the_updater() {
        // A typo must fail the request outright and leave the updater free,
        // not occupy it and surface later as a background failure.
        let dir = temp_dir("bad-version");
        let updater = updater(&dir, false, SupervisorKind::Systemd);
        let err = updater
            .start(
                Some("v/../../../../../owner/repo/releases/latest".to_string()),
                Some(1),
                &policy(true),
            )
            .expect_err("must reject a traversal-shaped version");
        assert!(matches!(err, SelfUpdateError::InvalidVersion { .. }));
        assert_eq!(
            err.blocker(),
            None,
            "a bad argument is not an install blocker"
        );
        // Still idle, so a corrected retry works immediately.
        assert_eq!(
            updater.capability(&policy(true)).phase,
            SelfUpdatePhase::Idle
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_start_refused_while_another_attempt_runs() {
        let dir = temp_dir("start-busy");
        let updater = updater(&dir, false, SupervisorKind::Systemd);
        updater.set_phase(SelfUpdatePhase::Downloading, None);
        let err = updater
            .start(None, Some(1), &policy(true))
            .expect_err("must refuse a concurrent attempt");
        assert!(matches!(err, SelfUpdateError::AlreadyRunning { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_concurrent_starts_claim_the_updater_exactly_once() {
        // Two callers racing on the same updater must not both win: they
        // would share the binary, the .bak and the temp files, and can leave a
        // truncated executable behind. The claim is a test-and-set under one
        // lock, so exactly one wins however they interleave.
        let dir = temp_dir("concurrent-claim");
        let updater = Arc::new(updater(&dir, false, SupervisorKind::Systemd));

        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rejected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let updater = updater.clone();
                let accepted = accepted.clone();
                let rejected = rejected.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    match updater.try_claim() {
                        Ok(()) => accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        Err(SelfUpdateError::AlreadyRunning { .. }) => {
                            rejected.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        }
                        Err(e) => panic!("unexpected error: {e}"),
                    };
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread");
        }

        assert_eq!(
            accepted.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one caller may claim the updater"
        );
        assert_eq!(rejected.load(std::sync::atomic::Ordering::SeqCst), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_failed_phase_does_not_block_a_retry() {
        // A previous failure must not wedge the updater — the operator has to
        // be able to try again after fixing whatever broke.
        let dir = temp_dir("retry");
        let updater = updater(&dir, false, SupervisorKind::Systemd);
        updater.set_phase(SelfUpdatePhase::Failed, Some("network down".to_string()));
        let cap = updater.capability(&policy(true));
        assert!(cap.can_apply);
        assert_eq!(cap.phase, SelfUpdatePhase::Failed);
        assert_eq!(cap.phase_error.as_deref(), Some("network down"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pending_journal_resolves_to_succeeded_on_matching_version() {
        let dir = temp_dir("journal-ok");
        let running = current_version_tag();
        let attempt = SelfUpdateAttempt {
            from_version: "v0.0.1".to_string(),
            to_version: Some(running.clone()),
            status: SelfUpdateStatus::Pending,
            started_at: Utc::now(),
            finished_at: None,
            triggered_by_user_id: Some(3),
            error: None,
            previous_binary_path: None,
            migrations_applied: None,
            migrations_total: None,
        };
        let path = dir.join(SELF_UPDATE_JOURNAL_FILE);
        write_journal(&path, &attempt).expect("write journal");

        let resolved = updater(&dir, false, SupervisorKind::Systemd)
            .reconcile_journal()
            .expect("journal entry");
        assert_eq!(resolved.status, SelfUpdateStatus::Succeeded);
        assert!(resolved.finished_at.is_some());
        // Resolution is persisted, so a later boot doesn't redo it.
        let reread = read_journal(&path).expect("read").expect("entry");
        assert_eq!(reread.status, SelfUpdateStatus::Succeeded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_pending_journal_resolves_to_failed_on_version_mismatch() {
        let dir = temp_dir("journal-fail");
        let attempt = SelfUpdateAttempt {
            from_version: current_version_tag(),
            // A version we are demonstrably not running.
            to_version: Some("v999.0.0".to_string()),
            status: SelfUpdateStatus::Pending,
            started_at: Utc::now(),
            finished_at: None,
            triggered_by_user_id: None,
            error: None,
            previous_binary_path: Some("/usr/local/bin/temps.bak".to_string()),
            migrations_applied: None,
            migrations_total: None,
        };
        write_journal(&dir.join(SELF_UPDATE_JOURNAL_FILE), &attempt).expect("write journal");

        let resolved = updater(&dir, false, SupervisorKind::Systemd)
            .reconcile_journal()
            .expect("journal entry");
        assert_eq!(resolved.status, SelfUpdateStatus::Failed);
        let error = resolved.error.expect("failure must explain itself");
        assert!(error.contains("v999.0.0"), "{error}");
        // The operator needs to be told where the old binary went.
        assert!(error.contains("/usr/local/bin/temps.bak"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolved_journal_is_left_alone() {
        // Already-decided attempts must not be re-resolved on every boot,
        // which would rewrite a success into a failure once the NEXT version
        // is installed.
        let dir = temp_dir("journal-stable");
        let attempt = SelfUpdateAttempt {
            from_version: "v0.0.1".to_string(),
            to_version: Some("v0.0.2".to_string()),
            status: SelfUpdateStatus::Succeeded,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            triggered_by_user_id: None,
            error: None,
            previous_binary_path: None,
            migrations_applied: Some(2),
            migrations_total: Some(2),
        };
        write_journal(&dir.join(SELF_UPDATE_JOURNAL_FILE), &attempt).expect("write journal");
        let resolved = updater(&dir, false, SupervisorKind::Systemd)
            .reconcile_journal()
            .expect("journal entry");
        assert_eq!(resolved.status, SelfUpdateStatus::Succeeded);
        assert_eq!(resolved.to_version.as_deref(), Some("v0.0.2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_installed_pending_restart_survives_a_restart_on_the_old_version() {
        // The operator installed an update but has not restarted yet. Booting
        // the OLD binary must not turn that into a failure — the new binary is
        // still sitting there waiting.
        let dir = temp_dir("journal-pending-restart");
        let attempt = SelfUpdateAttempt {
            from_version: current_version_tag(),
            to_version: Some("v999.0.0".to_string()),
            status: SelfUpdateStatus::InstalledPendingRestart,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            triggered_by_user_id: Some(1),
            error: None,
            previous_binary_path: None,
            migrations_applied: Some(0),
            migrations_total: Some(0),
        };
        write_journal(&dir.join(SELF_UPDATE_JOURNAL_FILE), &attempt).expect("write journal");

        let resolved = updater(&dir, false, SupervisorKind::None)
            .reconcile_journal()
            .expect("journal entry");
        assert_eq!(resolved.status, SelfUpdateStatus::InstalledPendingRestart);
        assert_eq!(resolved.error, None, "waiting is not a failure");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_installed_pending_restart_resolves_once_the_new_version_boots() {
        let dir = temp_dir("journal-pending-done");
        let running = current_version_tag();
        let attempt = SelfUpdateAttempt {
            from_version: "v0.0.1".to_string(),
            to_version: Some(running.clone()),
            status: SelfUpdateStatus::InstalledPendingRestart,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            triggered_by_user_id: Some(1),
            error: None,
            previous_binary_path: None,
            migrations_applied: Some(1),
            migrations_total: Some(1),
        };
        write_journal(&dir.join(SELF_UPDATE_JOURNAL_FILE), &attempt).expect("write journal");

        let resolved = updater(&dir, false, SupervisorKind::None)
            .reconcile_journal()
            .expect("journal entry");
        assert_eq!(resolved.status, SelfUpdateStatus::Succeeded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_missing_journal_is_not_an_error() {
        let dir = temp_dir("journal-absent");
        assert!(updater(&dir, false, SupervisorKind::Systemd)
            .reconcile_journal()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_corrupt_journal_degrades_instead_of_failing_startup() {
        let dir = temp_dir("journal-corrupt");
        std::fs::write(dir.join(SELF_UPDATE_JOURNAL_FILE), "{not json").expect("write");
        assert!(updater(&dir, false, SupervisorKind::Systemd)
            .reconcile_journal()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_preflight_rejects_a_binary_that_cannot_run() {
        let dir = temp_dir("preflight");
        let staged_path = dir.join("staged");
        std::fs::write(&staged_path, b"definitely not an executable").unwrap();
        let staged_file = std::fs::File::open(&staged_path).unwrap();

        // Not a valid executable — exec must fail, and the error must say the
        // running version is untouched.
        let err = preflight_staged_binary(&staged_path, &staged_file).expect_err("must reject");
        assert!(err.to_string().contains("left untouched"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_backup_keeps_the_original_in_place() {
        let dir = temp_dir("backup");
        let binary = dir.join("temps");
        let backup = backup_current_binary(&binary)
            .expect("backup")
            .expect("path");
        assert!(binary.exists(), "the original must never be moved away");
        assert!(backup.exists());
        assert_eq!(
            std::fs::read(&binary).unwrap(),
            std::fs::read(&backup).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_post_swap_migration_error_carries_to_version_and_backup_path() {
        // This is the one failure mode that did not exist before migrations were
        // added to the update flow: a failure AFTER the binary was replaced.
        // The error must carry the `to_version` so the journal entry can record
        // what binary is now on disk (not None, which would be wrong), and the
        // `backup_path` so the operator knows how to recover.
        let err = ExecuteError::PostSwapMigration {
            to_version: "v0.2.0".to_string(),
            backup_path: Some("/usr/local/bin/temps.bak".to_string()),
            error: "migrate exited with code 1".to_string(),
            migrations_applied: Some(2),
            migrations_total: Some(5),
        };

        let msg = err.to_string();
        // Must name the version that is now on disk.
        assert!(msg.contains("v0.2.0"), "error message: {msg}");
        // Must tell the operator where the old binary is.
        assert!(msg.contains("temps.bak"), "error message: {msg}");
        // Must NOT claim the running binary was untouched.
        assert!(!msg.contains("left untouched"), "error message: {msg}");
        // Must tell the operator to run migrate themselves.
        assert!(
            msg.contains("temps migrate") || msg.contains("migrate"),
            "error message: {msg}"
        );
    }

    #[test]
    fn test_migrating_phase_blocks_a_concurrent_trigger() {
        // `Migrating` must be active (i.e. block a second trigger) so two
        // concurrent updates can't race the migrate step against the same schema.
        let dir = temp_dir("migrating-blocks");
        let u = updater(&dir, false, SupervisorKind::Systemd);
        u.set_phase(SelfUpdatePhase::Migrating, None);
        let err = u
            .start(None, Some(1), &policy(true))
            .expect_err("must refuse while migrating");
        assert!(matches!(err, SelfUpdateError::AlreadyRunning { .. }));
        let cap = u.capability(&policy(true));
        assert_eq!(cap.phase, SelfUpdatePhase::Migrating);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capability_reports_migration_progress_from_state() {
        // While migrating, the live progress fields should be visible in the
        // capability so the frontend can show "2 of 5 migrations applied".
        let dir = temp_dir("migration-progress");
        let u = updater(&dir, false, SupervisorKind::Systemd);
        {
            let mut state = u.state.write().expect("write lock");
            state.phase = SelfUpdatePhase::Migrating;
            state.migrations_applied = Some(2);
            state.migrations_total = Some(5);
            state.current_migration_name = Some("m20260811_add_column".to_string());
        }
        let cap = u.capability(&policy(true));
        assert_eq!(cap.phase, SelfUpdatePhase::Migrating);
        assert_eq!(cap.migrations_applied, Some(2));
        assert_eq!(cap.migrations_total, Some(5));
        assert_eq!(
            cap.current_migration_name.as_deref(),
            Some("m20260811_add_column")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
