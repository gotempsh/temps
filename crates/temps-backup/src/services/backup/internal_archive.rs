// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep the bundled control-plane database from retaining WAL while no
//! archive destination exists.
//!
//! The bundled Compose files start PostgreSQL with `archive_mode=on`, because
//! `archive_mode` only changes on a restart and Temps cannot restart the
//! database it runs on, and leave `archive_command` unset. PostgreSQL then has
//! archiving enabled with nothing to archive to, and in that state it keeps
//! every completed WAL segment in `pg_wal` and waits for a command, until the
//! disk fills.
//!
//! On startup Temps gives such a database a command that accepts each segment
//! without storing it. PostgreSQL then recycles WAL as it would with archiving
//! off, and clears any backlog already built up. No WAL archive exists to
//! receive those segments, and WAL written before a base backup is not needed
//! to restore it, so nothing restorable is lost. Control-plane backups
//! (`ControlPlaneEngine`) are logical dumps and need no WAL archive, so the
//! command stays until an operator sets their own. A WAL-G base backup
//! through [`BackupService::archive_then_push`] replaces it with WAL-G
//! archiving before it starts, and puts back what was there if it fails.
//!
//! Only a command nobody ever set is replaced. PostgreSQL's documented way to
//! pause archiving is to set `archive_command` to an empty string, and a
//! database where someone did that is left alone.

use super::*;
use sea_orm::ConnectionTrait;

/// `archive_command` for a bundled database with no archive destination yet:
/// it reports every segment archived and keeps none of them.
pub const NO_ARCHIVE_DESTINATION_COMMAND: &str = "/bin/true";

/// The WAL archiving settings the control-plane database is running with.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub(super) struct ArchiveSettings {
    pub archive_mode: String,
    pub archive_command: String,
    /// `None` on PostgreSQL releases before 15, which have no
    /// `archive_library`.
    pub archive_library: Option<String>,
    /// Where the running `archive_command` came from (`pg_settings.source`):
    /// `default` when nothing ever set it.
    pub archive_command_source: Option<String>,
    /// The configuration file that set it (`pg_settings.sourcefile`), when
    /// PostgreSQL shows it to the connected role.
    pub archive_command_sourcefile: Option<String>,
}

/// A change to `archive_command` through `ALTER SYSTEM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArchiveCommandChange {
    /// Write the value to `postgresql.auto.conf`.
    Set(String),
    /// Remove the `postgresql.auto.conf` entry, so the value from the
    /// operator's own configuration applies again.
    Reset,
}

impl ArchiveCommandChange {
    fn sql(&self) -> String {
        match self {
            Self::Set(command) => format!(
                "ALTER SYSTEM SET archive_command = '{}'",
                command.replace('\'', "''")
            ),
            Self::Reset => "ALTER SYSTEM RESET archive_command".to_string(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Set(command) => format!("setting archive_command to '{}'", command),
            Self::Reset => "resetting archive_command to its configuration-file value".to_string(),
        }
    }
}

impl ArchiveSettings {
    /// Whether PostgreSQL is keeping every completed WAL segment because
    /// archiving is on and neither a command nor a library receives them.
    pub(super) fn retains_wal_without_destination(&self) -> bool {
        self.archiving_on() && self.archive_command.trim().is_empty() && !self.has_archive_library()
    }

    /// Whether the archiver runs. `archive_mode` only changes on a restart,
    /// so with it off no `archive_command` ships any WAL.
    fn archiving_on(&self) -> bool {
        matches!(
            self.archive_mode.trim().to_ascii_lowercase().as_str(),
            "on" | "always"
        )
    }

    fn has_archive_library(&self) -> bool {
        self.archive_library
            .as_deref()
            .is_some_and(|library| !library.trim().is_empty())
    }

    /// Whether `archive_command` is still PostgreSQL's built-in empty
    /// default, rather than a value someone set, possibly to pause archiving.
    pub(super) fn archive_command_never_set(&self) -> bool {
        self.archive_command_source.as_deref() == Some("default")
    }

    /// Whether `archive_command` was written by `ALTER SYSTEM`, which is how
    /// Temps sets it. `None` when PostgreSQL hides the file from this role.
    fn set_by_alter_system(&self) -> Option<bool> {
        self.archive_command_sourcefile
            .as_deref()
            .map(|file| file.ends_with("postgresql.auto.conf"))
    }

    /// What the startup check concludes without changing anything, or `None`
    /// when the database retains WAL on a command nobody set, and Temps may
    /// give it the placeholder.
    fn release_blocked(&self) -> Option<InternalArchiveReconcile> {
        if !self.retains_wal_without_destination() {
            return Some(InternalArchiveReconcile::Unaffected);
        }
        if !self.archive_command_never_set() {
            return Some(InternalArchiveReconcile::LeftToOperator {
                reason: format!(
                    "archive_command was set to an empty value (setting source: {}), which \
                     pauses archiving on purpose",
                    self.archive_command_source.as_deref().unwrap_or("unknown")
                ),
            });
        }
        None
    }

    /// Why Temps must not take a WAL-G base backup by pointing this
    /// database's archiving at `archiving_command`, or `None` when archiving
    /// runs and is Temps' own: unset, the startup placeholder, that command,
    /// or paused.
    ///
    /// Without a running archiver the base backup's WAL is never shipped, so
    /// the backup could not be restored. Any other archiver is the
    /// operator's, even one that runs WAL-G: switching it for a base backup
    /// would send segments to Temps' archive that the operator's archive then
    /// never receives, even if the backup is rolled back.
    pub(super) fn walg_takeover_blocker(&self, archiving_command: &str) -> Option<String> {
        if !self.archiving_on() {
            return Some(format!(
                "archive_mode is '{}', so no WAL would be archived while the base backup runs; \
                 it needs archive_mode=on, which takes a restart",
                self.archive_mode
            ));
        }
        if self.has_archive_library() {
            return Some(
                "archive_library is set, so WAL is archived by a module Temps does not manage, \
                 and PostgreSQL refuses an archive_command alongside it"
                    .to_string(),
            );
        }
        let command = self.archive_command.trim();
        if !command.is_empty()
            && command != NO_ARCHIVE_DESTINATION_COMMAND
            && command != archiving_command
        {
            return Some(format!(
                "archive_command (setting source: {}) archives WAL to a destination Temps does \
                 not manage, and switching it to WAL-G would leave gaps in that archive",
                self.archive_command_source.as_deref().unwrap_or("unknown")
            ));
        }
        None
    }

    /// The change that puts back what `archive_command` was after
    /// `archiving_command` was enabled for a base backup that then failed,
    /// or `None` when that was already the command.
    ///
    /// A command nobody had set goes back to the placeholder rather than
    /// empty, so the failed backup does not leave the database retaining WAL.
    /// A value from the operator's own configuration file is restored by
    /// removing Temps' override, so their later edits to that file apply.
    pub(super) fn rollback(&self, archiving_command: &str) -> Option<ArchiveCommandChange> {
        let from_alter_system = self.archive_command_source.as_deref()
            == Some("configuration file")
            && self.set_by_alter_system().unwrap_or(true);
        if self.archive_command == archiving_command {
            // The same command from the operator's own file gained an
            // override that would mask their later edits to that file.
            return (!from_alter_system).then_some(ArchiveCommandChange::Reset);
        }
        if self.archive_command_never_set() {
            return Some(ArchiveCommandChange::Set(
                NO_ARCHIVE_DESTINATION_COMMAND.to_string(),
            ));
        }
        if from_alter_system {
            Some(ArchiveCommandChange::Set(self.archive_command.clone()))
        } else {
            Some(ArchiveCommandChange::Reset)
        }
    }
}

/// An `archive_command` line in a configuration file, loaded or not
/// (`pg_file_settings`).
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub(super) struct ArchiveCommandFileEntry {
    pub sourcefile: Option<String>,
    pub setting: Option<String>,
}

/// The value `postgresql.auto.conf` holds for `archive_command`: the last
/// entry there, which is the one PostgreSQL loads.
fn auto_conf_value(entries: &[ArchiveCommandFileEntry]) -> Option<&str> {
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry
                .sourcefile
                .as_deref()
                .is_some_and(|file| file.ends_with("postgresql.auto.conf"))
        })
        .and_then(|entry| entry.setting.as_deref())
}

/// The `archive_command` the configuration files hold, which PostgreSQL loads
/// on its next reload: the last entry, since later files and lines override
/// earlier ones. `None` when no file sets it.
fn file_value(entries: &[ArchiveCommandFileEntry]) -> Option<&str> {
    entries
        .last()
        .map(|entry| entry.setting.as_deref().unwrap_or(""))
}

/// Whether `entries` hold only the startup placeholder, in
/// postgresql.auto.conf: Temps wrote it, and its reload did not take effect.
fn only_pending_placeholder(entries: &[ArchiveCommandFileEntry]) -> bool {
    file_value(entries) == Some(NO_ARCHIVE_DESTINATION_COMMAND)
        && auto_conf_value(entries) == Some(NO_ARCHIVE_DESTINATION_COMMAND)
        && entries.iter().all(|entry| {
            entry
                .sourcefile
                .as_deref()
                .is_some_and(|file| file.ends_with("postgresql.auto.conf"))
        })
}

/// Identifies one running PostgreSQL server: the cluster's system identifier,
/// which a physical replica shares with its primary, and the time this server
/// started, which it does not.
const INSTANCE_IDENTITY_SQL: &str = "SELECT system_identifier::text || '/' || \
     extract(epoch FROM pg_postmaster_start_time())::text AS instance_identity \
     FROM pg_control_system()";

#[derive(Debug, FromQueryResult)]
struct InstanceIdentity {
    instance_identity: String,
}

/// `Ok` when both reads name the same, non-empty server identity.
fn same_instance(
    container_id: &str,
    connected: &str,
    in_container: &str,
) -> Result<(), BackupError> {
    let (connected, in_container) = (connected.trim(), in_container.trim());
    if !connected.is_empty() && connected == in_container {
        return Ok(());
    }
    Err(BackupError::Internal {
        message: format!(
            "The control-plane database Temps is connected to (system identifier/start time \
             '{}') is not the server in container '{}' ('{}')",
            connected, container_id, in_container
        ),
    })
}

#[derive(Debug, FromQueryResult)]
struct ReloadResult {
    reloaded: bool,
}

/// The container-side steps of a WAL-G base backup that
/// [`BackupService::archive_then_push`] orders and undoes.
pub(super) struct BaseBackupSteps<WriteEnv, Push, RestoreEnv, DiscardPreviousEnv> {
    /// Writes walg.env with the destination's credentials, keeping the
    /// previous one.
    pub write_env: WriteEnv,
    /// Runs the base backup.
    pub push: Push,
    /// Puts back the walg.env `write_env` replaced, if it replaced it.
    pub restore_env: RestoreEnv,
    /// Deletes the kept copy of the previous walg.env.
    pub discard_previous_env: DiscardPreviousEnv,
}

/// What the startup check did to the control-plane database's archiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalArchiveReconcile {
    /// Archiving is off, or something already receives the WAL.
    Unaffected,
    /// The bundled database was retaining WAL with no destination; it now
    /// recycles it normally until an internal backup sets up archiving.
    Released { container_id: String },
    /// The database retains WAL with no destination, but Temps cannot tell
    /// the setting is its own: `archive_command` was set to empty on purpose,
    /// or the database is not a container Temps runs.
    LeftToOperator { reason: String },
}

impl BackupService {
    /// Stop the bundled control-plane database from retaining WAL while no
    /// archive destination is configured.
    ///
    /// Only an `archive_command` nobody ever set, on the local container Temps
    /// backs up with WAL-G, is changed. An empty value someone set is a
    /// deliberate pause, and a database elsewhere (managed PostgreSQL, another
    /// host) has archiving settings that belong to its operator.
    pub async fn reconcile_internal_wal_archiving(
        &self,
    ) -> Result<InternalArchiveReconcile, BackupError> {
        self.reconcile_internal_wal_archiving_with(|| async {
            let (container_id, _pgdata) = self.find_internal_db_container().await?;
            Ok(container_id)
        })
        .await
    }

    /// [`Self::reconcile_internal_wal_archiving`] with the bundled-container
    /// lookup supplied, so the decision can be tested without Docker.
    pub(super) async fn reconcile_internal_wal_archiving_with<F, Fut>(
        &self,
        locate_bundled_container: F,
    ) -> Result<InternalArchiveReconcile, BackupError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, BackupError>>,
    {
        if let Some(outcome) = self.read_archive_settings().await?.release_blocked() {
            return Ok(outcome);
        }

        let container_id = match locate_bundled_container().await {
            Ok(container_id) => container_id,
            Err(error) => {
                return Ok(InternalArchiveReconcile::LeftToOperator {
                    reason: error.to_string(),
                })
            }
        };

        // Finding and checking the container takes a while; an operator may
        // have set archiving up meanwhile, and that must not be replaced.
        if let Some(outcome) = self.read_archive_settings().await?.release_blocked() {
            return Ok(outcome);
        }
        // A command written with ALTER SYSTEM, or edited into a file, is not
        // running until the next reload; writing the placeholder would
        // replace it before it ever applies. The placeholder itself, left
        // pending by a reload that failed on an earlier start, is Temps' own
        // and is written and reloaded again.
        match self.archive_command_file_entries().await {
            Ok(entries) if entries.is_empty() || only_pending_placeholder(&entries) => {}
            Ok(entries) => {
                let files = entries
                    .iter()
                    .filter_map(|entry| entry.sourcefile.as_deref())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(InternalArchiveReconcile::LeftToOperator {
                    reason: format!(
                        "archive_command is set in {} but not loaded yet; it applies on the \
                         next configuration reload",
                        if files.is_empty() {
                            "a configuration file"
                        } else {
                            &files
                        }
                    ),
                });
            }
            Err(error) => {
                return Ok(InternalArchiveReconcile::LeftToOperator {
                    reason: format!(
                        "could not check the configuration files for an archive_command that \
                         is not loaded yet: {}",
                        error
                    ),
                })
            }
        }

        self.apply_archive_command(
            &container_id,
            &ArchiveCommandChange::Set(NO_ARCHIVE_DESTINATION_COMMAND.to_string()),
        )
        .await?;
        Ok(InternalArchiveReconcile::Released { container_id })
    }

    /// Check that `container_id` runs the PostgreSQL server Temps is
    /// connected to, by comparing the server's identity read over that
    /// connection with the one read inside the container.
    ///
    /// A container whose name, alias or address matches the DATABASE_URL host
    /// is not proof by itself: the host may resolve to another server, and a
    /// replica shares its primary's system identifier. Every change Temps
    /// makes to archiving goes through its own connection, so it must be the
    /// container's server.
    pub(super) async fn verify_container_is_connected_database(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
    ) -> Result<(), BackupError> {
        let connected = InstanceIdentity::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            INSTANCE_IDENTITY_SQL.to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the identity of the control-plane database to match it with \
                 container '{}': {}",
                container_id, e
            ),
        })?
        .map(|row| row.instance_identity)
        .unwrap_or_default();

        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL: {}", e),
        })?;
        let database = url.path().trim_start_matches('/');
        let mut cmd = vec![
            "psql".to_string(),
            "-U".to_string(),
            url.username().to_string(),
            "-tAc".to_string(),
            INSTANCE_IDENTITY_SQL.to_string(),
        ];
        if !database.is_empty() {
            cmd.extend(["-d".to_string(), database.to_string()]);
        }
        // The server listens on the DATABASE_URL port inside its container
        // too: the URL reaches it on its network, not a published port.
        if let Some(port) = url.port() {
            cmd.extend(["-p".to_string(), port.to_string()]);
        }
        let password_env = format!("PGPASSWORD={}", url.password().unwrap_or(""));
        let in_container = temps_providers::externalsvc::exec_util::run_exec(
            docker,
            container_id,
            cmd,
            Some(vec![password_env]),
            std::time::Duration::from_secs(30),
        )
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the identity of the database in container '{}': {}",
                container_id, e
            ),
        })?
        .output;

        same_instance(container_id, &connected, &in_container)
    }

    /// Take a WAL-G base backup in the only safe order: write walg.env, point
    /// archive_command at WAL-G (`archiving_command`), then `push` the base
    /// backup, so the WAL written while it runs reaches the archive.
    ///
    /// Archiving that is not Temps' own
    /// ([`ArchiveSettings::walg_takeover_blocker`]) is never switched; the
    /// error makes the caller fall back to pg_dump. If enabling archiving
    /// fails, `push` never starts: a base backup without its WAL cannot be
    /// restored. On any failure walg.env and archive_command are put back as
    /// they were in `before`, because a command left failing on every segment
    /// makes PostgreSQL keep all WAL until the disk fills, and a walg.env
    /// left pointing at the failed destination redirects working WAL-G
    /// archiving. The step's error is returned either way; a failed rollback
    /// is logged.
    pub(super) async fn archive_then_push<T, WriteEnv, Push, RestoreEnv, DiscardPreviousEnv>(
        &self,
        before: &ArchiveSettings,
        archiving_command: &str,
        container_id: &str,
        steps: BaseBackupSteps<WriteEnv, Push, RestoreEnv, DiscardPreviousEnv>,
    ) -> Result<T, BackupError>
    where
        WriteEnv: std::future::Future<Output = Result<(), BackupError>>,
        Push: std::future::Future<Output = Result<T, BackupError>>,
        RestoreEnv: std::future::Future<Output = Result<(), BackupError>>,
        DiscardPreviousEnv: std::future::Future<Output = Result<(), BackupError>>,
    {
        if let Some(reason) = before.walg_takeover_blocker(archiving_command) {
            return Err(BackupError::Internal {
                message: format!(
                    "Not taking a WAL-G base backup of the control-plane database in container \
                     '{}': {}",
                    container_id, reason
                ),
            });
        }

        // A command staged in a file but not loaded yet would be replaced by
        // the WAL-G one, and the rollback would put back the running value,
        // not it.
        let pending = match self.archive_command_file_entries().await {
            Ok(entries) => file_value(&entries).map(str::to_string),
            Err(error) => {
                return Err(BackupError::Internal {
                    message: format!(
                        "Not taking a WAL-G base backup of the control-plane database in \
                         container '{}': could not check its configuration files for an \
                         archive_command that is not loaded yet: {}",
                        container_id, error
                    ),
                })
            }
        };
        if let Some(pending) = pending.filter(|pending| {
            *pending != before.archive_command
                && pending != NO_ARCHIVE_DESTINATION_COMMAND
                && pending != archiving_command
        }) {
            return Err(BackupError::Internal {
                message: format!(
                    "Not taking a WAL-G base backup of the control-plane database in container \
                     '{}': its configuration files set an archive_command that is not loaded \
                     yet ({} characters), which Temps does not manage",
                    container_id,
                    pending.len()
                ),
            });
        }

        let enabling_failed = |error: BackupError| BackupError::Internal {
            message: format!(
                "Cannot take a WAL-G base backup of the internal database in container '{}': \
                 continuous WAL archiving could not be enabled: {}",
                container_id, error
            ),
        };
        // Whether archive_command may have been changed when it failed.
        let (error, command_changed) = match steps.write_env.await {
            Err(error) => (enabling_failed(error), false),
            Ok(()) => match self
                .apply_archive_command(
                    container_id,
                    &ArchiveCommandChange::Set(archiving_command.to_string()),
                )
                .await
            {
                Err(error) => (enabling_failed(error), true),
                Ok(()) => match steps.push.await {
                    Err(error) => (error, true),
                    Ok(value) => {
                        if let Err(error) = steps.discard_previous_env.await {
                            warn!(
                                container_id = %container_id,
                                "WAL-G base backup succeeded, but the copy of the previous \
                                 walg.env could not be deleted: {}",
                                error
                            );
                        }
                        return Ok(value);
                    }
                },
            },
        };

        if let Err(env_error) = steps.restore_env.await {
            error!(
                container_id = %container_id,
                "WAL-G base backup failed, and putting back the previous walg.env failed too, \
                 so WAL-G archiving may use the failed destination: {}. Backup error: {}",
                env_error,
                error
            );
        }
        if command_changed {
            self.roll_back_archive_command(before, archiving_command, container_id, &error)
                .await;
        }
        Err(error)
    }

    /// Undo setting archive_command to `archiving_command` after `error`, if
    /// postgresql.auto.conf still holds it. An attempt whose ALTER SYSTEM
    /// never ran left nothing to undo, and a value someone wrote since is
    /// theirs. When the file cannot be read the change is undone anyway: a
    /// WAL-G command left failing would make PostgreSQL keep all WAL.
    async fn roll_back_archive_command(
        &self,
        before: &ArchiveSettings,
        archiving_command: &str,
        container_id: &str,
        error: &BackupError,
    ) {
        let Some(change) = before.rollback(archiving_command) else {
            return;
        };
        let ownership = temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_millis(200))
            .with_max_delay(std::time::Duration::from_secs(1))
            .retry(|| self.archive_command_file_entries())
            .await;
        match ownership {
            Ok(entries) if auto_conf_value(&entries) == Some(archiving_command) => {}
            Ok(_) => {
                warn!(
                    container_id = %container_id,
                    "WAL-G base backup failed; archive_command in postgresql.auto.conf is not \
                     the one this attempt set, so it is left as it is: {}",
                    error
                );
                return;
            }
            // Writing blind could replace a command someone set since with
            // one that discards WAL; a WAL-G command left in place fails
            // loudly instead, and the WAL archiving alerts report it.
            Err(read_error) => {
                error!(
                    container_id = %container_id,
                    "WAL-G base backup failed, and whether postgresql.auto.conf still holds the \
                     archive_command it set could not be checked, so it was not undone: {}. \
                     Check archive_command on the control-plane database. Backup error: {}",
                    read_error,
                    error
                );
                return;
            }
        }
        match self.apply_archive_command(container_id, &change).await {
            Ok(()) => warn!(
                container_id = %container_id,
                "WAL-G base backup failed; undid archive_command by {} so WAL does not \
                 accumulate: {}",
                change.describe(),
                error
            ),
            Err(rollback_error) => error!(
                container_id = %container_id,
                "WAL-G base backup failed, and undoing archive_command by {} failed too, so \
                 the database may retain WAL until archiving works: {}. Backup error: {}",
                change.describe(),
                rollback_error,
                error
            ),
        }
    }

    /// Every `archive_command` line in the configuration files, loaded or
    /// not, in the order PostgreSQL reads them.
    async fn archive_command_file_entries(
        &self,
    ) -> Result<Vec<ArchiveCommandFileEntry>, BackupError> {
        ArchiveCommandFileEntry::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT sourcefile, setting FROM pg_file_settings \
             WHERE name = 'archive_command' ORDER BY seqno"
                .to_string(),
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the archive_command entries in the configuration files of the \
                 control-plane database: {}",
                e
            ),
        })
    }

    /// Change the control-plane database's `archive_command` and reload.
    async fn apply_archive_command(
        &self,
        container_id: &str,
        change: &ArchiveCommandChange,
    ) -> Result<(), BackupError> {
        // ALTER SYSTEM cannot run in a transaction block, so each statement
        // runs on its own.
        self.db
            .execute_unprepared(&change.sql())
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed {} on the control-plane database in container '{}': {}",
                    change.describe(),
                    container_id,
                    e
                ),
            })?;
        let reloaded = ReloadResult::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT pg_reload_conf() AS reloaded".to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Wrote {} to postgresql.auto.conf of the control-plane database in container \
                 '{}', but reloading the configuration failed, so it is not in effect: {}",
                change.describe(),
                container_id,
                e
            ),
        })?
        .is_some_and(|row| row.reloaded);
        if !reloaded {
            return Err(BackupError::Internal {
                message: format!(
                    "Wrote {} to postgresql.auto.conf of the control-plane database in \
                     container '{}', but pg_reload_conf() could not signal the server, so it is \
                     not in effect",
                    change.describe(),
                    container_id
                ),
            });
        }
        Ok(())
    }

    pub(super) async fn read_archive_settings(&self) -> Result<ArchiveSettings, BackupError> {
        ArchiveSettings::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT current_setting('archive_mode') AS archive_mode, \
             current_setting('archive_command') AS archive_command, \
             current_setting('archive_library', true) AS archive_library, \
             s.source AS archive_command_source, \
             s.sourcefile AS archive_command_sourcefile \
             FROM (SELECT 1) AS one \
             LEFT JOIN pg_settings AS s ON s.name = 'archive_command'"
                .to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the WAL archiving settings of the control-plane database: {}",
                e
            ),
        })?
        .ok_or_else(|| BackupError::Internal {
            message: "Reading the WAL archiving settings of the control-plane database \
                      returned no row"
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests;
