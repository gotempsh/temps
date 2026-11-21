// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep the bundled control-plane database from retaining WAL while no
//! archive destination exists.
//!
//! The bundled Compose files start PostgreSQL with `archive_mode=on`, because
//! `archive_mode` only changes on a restart and Temps cannot restart the
//! database it runs on when a backup is first configured. `archive_command`
//! is left to Temps, which sets it to `wal-g wal-push` before the first
//! internal backup. Until then PostgreSQL has archiving enabled with nothing
//! to archive to, and in that state it keeps every completed WAL segment in
//! `pg_wal` and waits for a command. An install that never configures backups
//! fills its disk this way.
//!
//! On startup Temps gives such a database a command that accepts each segment
//! without storing it. PostgreSQL then recycles WAL as it would with archiving
//! off, and clears any backlog already built up. WAL written before the first
//! base backup is not needed to restore it, so nothing restorable is lost. The
//! first internal backup replaces the command with a real one before it starts,
//! and puts back what was there if its base backup then fails.
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
        if self.archive_command == archiving_command {
            return None;
        }
        if self.archive_command_never_set() {
            return Some(ArchiveCommandChange::Set(
                NO_ARCHIVE_DESTINATION_COMMAND.to_string(),
            ));
        }
        let from_alter_system = self.archive_command_source.as_deref()
            == Some("configuration file")
            && self.set_by_alter_system().unwrap_or(true);
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
        // replace it before it ever applies.
        match self.archive_command_file_entries().await {
            Ok(entries) if entries.is_empty() => {}
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
        match self.archive_command_file_entries().await {
            Ok(entries) if auto_conf_value(&entries) != Some(archiving_command) => {
                warn!(
                    container_id = %container_id,
                    "WAL-G base backup failed; archive_command in postgresql.auto.conf is not \
                     the one this attempt set, so it is left as it is: {}",
                    error
                );
                return;
            }
            Ok(_) => {}
            Err(read_error) => warn!(
                container_id = %container_id,
                "Could not check whether postgresql.auto.conf still holds the archive_command \
                 this attempt set; undoing it anyway: {}",
                read_error
            ),
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
mod tests {
    use super::super::test_support::make_scope_test_service;
    use super::*;
    use sea_orm::{MockDatabase, MockExecResult};

    const WALG_COMMAND: &str = ". /home/postgres/pgdata/walg.env && wal-g wal-push %p";
    const AUTO_CONF: &str = "/home/postgres/pgdata/postgresql.auto.conf";

    /// Settings whose `archive_command` nobody set.
    fn settings(mode: &str, command: &str, library: Option<&str>) -> ArchiveSettings {
        ArchiveSettings {
            archive_mode: mode.to_string(),
            archive_command: command.to_string(),
            archive_library: library.map(str::to_string),
            archive_command_source: Some("default".to_string()),
            archive_command_sourcefile: None,
        }
    }

    /// Settings whose `archive_command` was set in `sourcefile`.
    fn settings_in(command: &str, library: Option<&str>, sourcefile: &str) -> ArchiveSettings {
        ArchiveSettings {
            archive_command_source: Some("configuration file".to_string()),
            archive_command_sourcefile: Some(sourcefile.to_string()),
            ..settings("on", command, library)
        }
    }

    fn settings_row(settings: &ArchiveSettings) -> BTreeMap<&'static str, Value> {
        let optional = |value: &Option<String>| Value::String(value.clone().map(Box::new));
        BTreeMap::from([
            ("archive_mode", Value::from(settings.archive_mode.as_str())),
            (
                "archive_command",
                Value::from(settings.archive_command.as_str()),
            ),
            ("archive_library", optional(&settings.archive_library)),
            (
                "archive_command_source",
                optional(&settings.archive_command_source),
            ),
            (
                "archive_command_sourcefile",
                optional(&settings.archive_command_sourcefile),
            ),
        ])
    }

    fn reload_row(reloaded: bool) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([("reloaded", Value::Bool(Some(reloaded)))])
    }

    /// The SQL a service ran on `db`, once the service has been dropped.
    fn logged_sql(db: Arc<DatabaseConnection>) -> Vec<String> {
        Arc::try_unwrap(db)
            .expect("service released its database handle")
            .into_transaction_log()
            .iter()
            .flat_map(|txn| txn.statements().iter())
            .map(|statement| statement.sql.clone())
            .collect()
    }

    fn push_failure() -> BackupError {
        BackupError::Internal {
            message: "wal-g backup-push failed with exit code 1 in container temps-postgres"
                .to_string(),
        }
    }

    fn exec_ok() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }
    }

    fn file_entry_row(sourcefile: &str, setting: &str) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("sourcefile", Value::from(sourcefile)),
            ("setting", Value::from(setting)),
        ])
    }

    /// pg_file_settings once Temps wrote the WAL-G command.
    fn walg_in_auto_conf() -> Vec<BTreeMap<&'static str, Value>> {
        vec![file_entry_row(AUTO_CONF, WALG_COMMAND)]
    }

    /// A database answering `queries` in order and accepting `changes`
    /// statements (ALTER SYSTEM).
    fn mock(
        queries: Vec<Vec<BTreeMap<&'static str, Value>>>,
        changes: usize,
    ) -> Arc<DatabaseConnection> {
        Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(queries)
                .append_exec_results((0..changes).map(|_| exec_ok()))
                .into_connection(),
        )
    }

    /// Steps recorded by `archive_then_push` tests, in the order they ran.
    type Steps = Arc<std::sync::Mutex<Vec<&'static str>>>;

    fn step(steps: &Steps, name: &'static str) {
        steps.lock().unwrap().push(name);
    }

    type StepFuture<'a> =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BackupError>> + 'a>>;

    /// Base backup steps that record themselves, with writing walg.env and
    /// the push succeeding or failing as asked.
    fn recorded_steps(
        steps: &Steps,
        write_ok: bool,
        push_ok: bool,
    ) -> BaseBackupSteps<StepFuture<'_>, StepFuture<'_>, StepFuture<'_>, StepFuture<'_>> {
        BaseBackupSteps {
            write_env: Box::pin(async move {
                step(steps, "write walg.env");
                if write_ok {
                    Ok(())
                } else {
                    Err(BackupError::Internal {
                        message: "Failed to write walg.env in container 'temps-postgres'"
                            .to_string(),
                    })
                }
            }),
            push: Box::pin(async move {
                step(steps, "backup-push");
                if push_ok {
                    Ok(())
                } else {
                    Err(push_failure())
                }
            }),
            restore_env: Box::pin(async move {
                step(steps, "restore walg.env");
                Ok(())
            }),
            discard_previous_env: Box::pin(async move {
                step(steps, "discard previous walg.env");
                Ok(())
            }),
        }
    }

    #[test]
    fn archiving_on_with_no_command_or_library_retains_wal() {
        assert!(settings("on", "", Some("")).retains_wal_without_destination());
        assert!(settings("always", "  ", None).retains_wal_without_destination());
        assert!(settings("ON", "", None).retains_wal_without_destination());
    }

    #[test]
    fn archiving_off_or_with_a_destination_does_not_retain_wal() {
        assert!(!settings("off", "", Some("")).retains_wal_without_destination());
        assert!(!settings("on", WALG_COMMAND, Some("")).retains_wal_without_destination());
        assert!(!settings("on", "", Some("basic_archive")).retains_wal_without_destination());
        assert!(
            !settings("on", NO_ARCHIVE_DESTINATION_COMMAND, None).retains_wal_without_destination()
        );
    }

    #[tokio::test]
    async fn bundled_database_retaining_wal_gets_a_discarding_archive_command() {
        let retaining = settings_row(&settings("on", "", Some("")));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![retaining.clone()],
                vec![retaining],
                vec![],
                vec![reload_row(true)],
            ])
            .append_exec_results([exec_ok()])
            .into_connection();
        let db = Arc::new(db);
        let service = make_scope_test_service(db.clone());

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::Released {
                container_id: "temps-postgres".to_string()
            }
        );
        drop(service);
        let sql = logged_sql(db);
        assert_eq!(sql.len(), 5, "unexpected statements: {sql:?}");
        assert!(sql[2].contains("pg_file_settings"), "{sql:?}");
        assert_eq!(sql[3], "ALTER SYSTEM SET archive_command = '/bin/true'");
        assert_eq!(sql[4], "SELECT pg_reload_conf() AS reloaded");
    }

    /// The reported failure, against a real PostgreSQL started the way the
    /// bundled Compose files start it: completed WAL segments wait in
    /// `pg_wal/archive_status` as `.ready` and are never archived, until the
    /// reconcile gives the archiver a command and it clears them.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn bundled_database_stops_retaining_wal_after_the_reconcile() {
        use sea_orm::{ConnectionTrait, Database};
        use testcontainers::core::{ContainerPort, WaitFor};
        use testcontainers::runners::AsyncRunner;
        use testcontainers::{GenericImage, ImageExt};

        let docker_up = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_up {
            println!("Docker not available, skipping");
            return;
        }
        // With Docker up, a container that does not start is a failure.
        let container = GenericImage::new("pgvector/pgvector", "pg18")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_cmd([
                "postgres",
                "-c",
                "wal_level=replica",
                "-c",
                "archive_mode=on",
                "-c",
                "archive_timeout=60",
            ])
            .start()
            .await
            .expect("PostgreSQL container failed to start");
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        // The ready message is also logged by the init-time server, which
        // then restarts; retry until the final server accepts connections.
        let mut connected = None;
        for _ in 0..40 {
            match Database::connect(&url).await {
                Ok(db) if db.execute_unprepared("SELECT 1").await.is_ok() => {
                    connected = Some(db);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        }
        let db = Arc::new(connected.expect("PostgreSQL never accepted connections"));

        async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
            #[derive(FromQueryResult)]
            struct Row {
                n: i64,
            }
            Row::find_by_statement(Statement::from_string(
                DatabaseBackend::Postgres,
                sql.to_string(),
            ))
            .one(db)
            .await
            .unwrap()
            .unwrap()
            .n
        }
        const READY: &str = "SELECT count(*) AS n FROM pg_ls_dir('pg_wal/archive_status') \
                             AS f WHERE f LIKE '%.ready'";
        const ARCHIVED: &str = "SELECT archived_count AS n FROM pg_stat_archiver";

        // Complete a few segments, as normal background activity does.
        db.execute_unprepared("CREATE TABLE wal_filler (x int)")
            .await
            .unwrap();
        for _ in 0..3 {
            db.execute_unprepared("INSERT INTO wal_filler SELECT generate_series(1, 1000)")
                .await
                .unwrap();
            db.execute_unprepared("SELECT pg_switch_wal()")
                .await
                .unwrap();
        }
        let pending = scalar(&db, READY).await;
        assert!(pending >= 3, "expected retained segments, found {pending}");
        assert_eq!(scalar(&db, ARCHIVED).await, 0);

        let service = make_scope_test_service(db.clone());
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();
        assert!(matches!(outcome, InternalArchiveReconcile::Released { .. }));

        // The archiver works through the backlog once it has a command.
        // The archiver drops a segment's .ready file before it counts the
        // segment, so wait for both.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while scalar(&db, READY).await > 0 || scalar(&db, ARCHIVED).await < pending {
            assert!(
                std::time::Instant::now() < deadline,
                "segments still waiting for the archiver after 30s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        // A second start leaves the database as it is.
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a released database must not trigger container discovery")
            })
            .await
            .unwrap();
        assert_eq!(outcome, InternalArchiveReconcile::Unaffected);

        // Emptying the command on purpose pauses archiving, and is left alone.
        // pg_reload_conf() only signals the server, so wait until it applies.
        db.execute_unprepared("ALTER SYSTEM SET archive_command = ''")
            .await
            .unwrap();
        db.execute_unprepared("SELECT pg_reload_conf()")
            .await
            .unwrap();
        const STILL_RELEASED: &str = "SELECT count(*) AS n FROM pg_settings \
                                      WHERE name = 'archive_command' AND setting <> ''";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while scalar(&db, STILL_RELEASED).await > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "archive_command was not emptied 10s after the reload"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a paused database must not trigger container discovery")
            })
            .await
            .unwrap();
        assert!(
            matches!(outcome, InternalArchiveReconcile::LeftToOperator { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn database_already_archiving_is_left_alone_without_looking_for_a_container() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(&settings_in(
                WALG_COMMAND,
                Some(""),
                AUTO_CONF,
            ))]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("an archiving database must not trigger container discovery")
            })
            .await
            .unwrap();

        assert_eq!(outcome, InternalArchiveReconcile::Unaffected);
    }

    #[tokio::test]
    async fn database_temps_does_not_run_is_left_to_its_operator() {
        // Only the settings query is queued: an ALTER SYSTEM would fail the
        // mock with "no exec result".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(&settings("on", "", None))]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                Err(BackupError::Internal {
                    message: "No Docker container found for database host 'db.example.internal'"
                        .to_string(),
                })
            })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::LeftToOperator {
                reason: "Internal error: No Docker container found for database host \
                         'db.example.internal'"
                    .to_string()
            }
        );
    }

    #[tokio::test]
    async fn archiving_set_up_while_the_container_was_located_is_not_replaced() {
        // No exec result is queued: an ALTER SYSTEM would fail the mock.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![settings_row(&settings("on", "", None))],
                vec![settings_row(&settings_in(
                    "cp %p /archive/%f",
                    None,
                    AUTO_CONF,
                ))],
            ])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        assert_eq!(outcome, InternalArchiveReconcile::Unaffected);
    }

    #[tokio::test]
    async fn archiving_paused_while_the_container_was_located_is_left_to_its_operator() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![settings_row(&settings("on", "", None))],
                vec![settings_row(&settings_in("", None, AUTO_CONF))],
            ])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        assert!(
            matches!(outcome, InternalArchiveReconcile::LeftToOperator { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_command_written_but_not_yet_loaded_is_not_replaced() {
        // ALTER SYSTEM without a reload: the running value is still the
        // default, the file already holds the operator's command. No exec
        // result is queued: an ALTER SYSTEM would fail the mock.
        let retaining = settings_row(&settings("on", "", None));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![retaining.clone()],
                vec![retaining],
                vec![file_entry_row(AUTO_CONF, "cp %p /archive/%f")],
            ])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::LeftToOperator {
                reason: format!(
                    "archive_command is set in {AUTO_CONF} but not loaded yet; it applies on \
                     the next configuration reload"
                )
            }
        );
    }

    #[tokio::test]
    async fn configuration_files_that_cannot_be_read_leave_the_database_alone() {
        let retaining = settings_row(&settings("on", "", None));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![retaining.clone()], vec![retaining]])
            .append_query_errors([sea_orm::DbErr::Custom(
                "permission denied for view pg_file_settings".to_string(),
            )])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        let InternalArchiveReconcile::LeftToOperator { reason } = outcome else {
            panic!("expected LeftToOperator, got {outcome:?}");
        };
        assert!(reason.contains("permission denied"), "{reason}");
    }

    #[tokio::test]
    async fn failing_alter_system_names_the_container_and_the_command() {
        let retaining = settings_row(&settings("on", "", Some("")));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![retaining.clone()], vec![retaining], vec![]])
            .append_exec_errors([sea_orm::DbErr::Custom(
                "permission denied to set parameter \"archive_command\"".to_string(),
            )])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let error = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("temps-postgres"), "{error}");
        assert!(error.contains("/bin/true"), "{error}");
        assert!(error.contains("permission denied"), "{error}");
    }

    #[tokio::test]
    async fn a_reload_that_cannot_signal_the_server_is_not_a_release() {
        let retaining = settings_row(&settings("on", "", Some("")));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![retaining.clone()],
                vec![retaining],
                vec![],
                vec![reload_row(false)],
            ])
            .append_exec_results([exec_ok()])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let error = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("not in effect"), "{error}");
        assert!(error.contains("temps-postgres"), "{error}");
    }

    #[test]
    fn a_container_is_the_connected_database_only_with_the_same_identity() {
        let primary = "7301234567890123456/1763712345.123456";
        assert!(same_instance("temps-postgres", primary, &format!("{primary}\n")).is_ok());

        // Another cluster.
        let error = same_instance(
            "temps-postgres",
            primary,
            "7309999999999999999/1763712345.123456",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("temps-postgres"), "{error}");
        assert!(error.contains("7309999999999999999"), "{error}");

        // A replica of the connected server: same cluster, its own start.
        assert!(same_instance(
            "temps-postgres",
            primary,
            "7301234567890123456/1763799999.654321"
        )
        .is_err());

        // An empty read (psql printed nothing) never counts as a match.
        assert!(same_instance("temps-postgres", "", "").is_err());
    }

    #[tokio::test]
    async fn archiving_paused_on_purpose_is_left_to_its_operator() {
        // Only the settings query is queued: an ALTER SYSTEM would fail the
        // mock with "no exec result".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(&settings_in(
                "",
                Some(""),
                "/etc/postgresql/postgresql.conf",
            ))]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a paused database must not trigger container discovery")
            })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::LeftToOperator {
                reason: "archive_command was set to an empty value (setting source: \
                         configuration file), which pauses archiving on purpose"
                    .to_string()
            }
        );
    }

    #[test]
    fn a_failed_base_backup_puts_back_what_archive_command_was() {
        let set = |command: &str| Some(ArchiveCommandChange::Set(command.to_string()));
        // Nobody had set it: back to the placeholder, not to retaining WAL.
        assert_eq!(
            settings("on", "", Some("")).rollback(WALG_COMMAND),
            set(NO_ARCHIVE_DESTINATION_COMMAND)
        );
        // The startup check had released it.
        assert_eq!(
            settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF).rollback(WALG_COMMAND),
            set(NO_ARCHIVE_DESTINATION_COMMAND)
        );
        // A pause through ALTER SYSTEM is put back as it was.
        assert_eq!(
            settings_in("", None, AUTO_CONF).rollback(WALG_COMMAND),
            set("")
        );
        // A pause in the operator's own file: Temps' override is removed, so
        // their next edit to that file applies.
        assert_eq!(
            settings_in("", None, "/etc/postgresql/postgresql.conf").rollback(WALG_COMMAND),
            Some(ArchiveCommandChange::Reset)
        );
        // A file PostgreSQL does not show: keep the value that was in effect.
        let hidden = ArchiveSettings {
            archive_command_sourcefile: None,
            ..settings_in("", None, AUTO_CONF)
        };
        assert_eq!(hidden.rollback(WALG_COMMAND), set(""));
        // Archiving with WAL-G was already in place: nothing to undo.
        assert_eq!(
            settings_in(WALG_COMMAND, None, AUTO_CONF).rollback(WALG_COMMAND),
            None
        );
    }

    #[test]
    fn archiving_temps_does_not_manage_is_never_switched_to_walg() {
        let blocked = |settings: ArchiveSettings| settings.walg_takeover_blocker(WALG_COMMAND);
        assert!(blocked(settings_in("cp %p /archive/%f", None, AUTO_CONF)).is_some());
        assert!(blocked(settings("on", "", Some("basic_archive"))).is_some());
        // WAL-G run by someone else, to their own archive.
        assert!(blocked(settings_in(
            "env WALG_S3_PREFIX=s3://operator/archive wal-g wal-push %p",
            None,
            AUTO_CONF
        ))
        .is_some());
        // The managed-service images' command, not the control plane's.
        assert!(blocked(settings_in(
            ". /var/lib/postgresql/walg.env && wal-g wal-push %p",
            None,
            AUTO_CONF
        ))
        .is_some());
        // No archiver runs, so the base backup's WAL would never be shipped.
        let off = blocked(settings("off", "", None)).unwrap();
        assert!(off.contains("archive_mode is 'off'"), "{off}");

        // Temps' own states: unset, the placeholder, paused, its WAL-G command.
        assert_eq!(blocked(settings("on", "", Some(""))), None);
        assert_eq!(
            blocked(settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF)),
            None
        );
        assert_eq!(blocked(settings_in("", None, AUTO_CONF)), None);
        assert_eq!(blocked(settings_in(WALG_COMMAND, None, AUTO_CONF)), None);
    }

    #[tokio::test]
    async fn base_backup_refuses_to_take_over_an_operator_archive() {
        // Nothing is queued: any statement would fail the mock.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings_in("cp %p '/archive/%f'", None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, true),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("Temps does not manage"), "{error}");
        assert!(error.contains("temps-postgres"), "{error}");
        assert!(steps.lock().unwrap().is_empty());
        drop(service);
        assert!(logged_sql(db).is_empty());
    }

    #[tokio::test]
    async fn base_backup_starts_only_after_archiving_is_enabled() {
        let db = mock(vec![vec![reload_row(true)]], 1);
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        service
            .archive_then_push(
                &settings("on", "", Some("")),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, true),
            )
            .await
            .unwrap();

        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "backup-push", "discard previous walg.env"]
        );
        drop(service);
        assert_eq!(
            logged_sql(db),
            [
                format!("ALTER SYSTEM SET archive_command = '{WALG_COMMAND}'"),
                "SELECT pg_reload_conf() AS reloaded".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn walg_env_that_cannot_be_written_changes_no_setting() {
        // Nothing is queued: any statement would fail the mock.
        let db = mock(vec![], 0);
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, false, true),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("could not be enabled"), "{error}");
        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "restore walg.env"]
        );
        drop(service);
        assert!(logged_sql(db).is_empty());
    }

    #[tokio::test]
    async fn archive_command_that_was_never_written_is_not_rolled_back() {
        // ALTER SYSTEM is refused, so postgresql.auto.conf does not hold the
        // WAL-G command and there is nothing to undo.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_errors([sea_orm::DbErr::Custom(
                    "permission denied to set parameter \"archive_command\"".to_string(),
                )])
                .append_query_results([Vec::<BTreeMap<&'static str, Value>>::new()])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, true),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("could not be enabled"), "{error}");
        assert!(error.contains("permission denied"), "{error}");
        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "restore walg.env"]
        );
        drop(service);
        let sql = logged_sql(db);
        assert!(
            !sql.iter().any(|statement| statement.contains("/bin/true")),
            "{sql:?}"
        );
    }

    #[tokio::test]
    async fn archiving_whose_reload_failed_is_rolled_back_without_a_base_backup() {
        let db = mock(
            vec![
                vec![reload_row(false)],
                walg_in_auto_conf(),
                vec![reload_row(true)],
            ],
            2,
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings("on", "", Some("")),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, true),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("could not be enabled"), "{error}");
        assert!(error.contains("not in effect"), "{error}");
        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "restore walg.env"]
        );
        drop(service);
        let sql = logged_sql(db);
        assert_eq!(sql[3], "ALTER SYSTEM SET archive_command = '/bin/true'");
        assert_eq!(sql.len(), 5, "{sql:?}");
    }

    #[tokio::test]
    async fn failed_first_base_backup_puts_back_the_placeholder_and_walg_env() {
        let db = mock(
            vec![
                vec![reload_row(true)],
                walg_in_auto_conf(),
                vec![reload_row(true)],
            ],
            2,
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, false),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("wal-g backup-push failed"), "{error}");
        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "backup-push", "restore walg.env"]
        );
        drop(service);
        let sql = logged_sql(db);
        assert_eq!(
            sql[3..],
            [
                "ALTER SYSTEM SET archive_command = '/bin/true'",
                "SELECT pg_reload_conf() AS reloaded"
            ]
        );
    }

    #[tokio::test]
    async fn failed_base_backup_removes_the_override_of_a_pause_in_the_operator_file() {
        let db = mock(
            vec![
                vec![reload_row(true)],
                walg_in_auto_conf(),
                vec![reload_row(true)],
            ],
            2,
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let result = service
            .archive_then_push(
                &settings_in("", None, "/etc/postgresql/postgresql.conf"),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, false),
            )
            .await;

        assert!(result.is_err());
        drop(service);
        assert_eq!(logged_sql(db)[3], "ALTER SYSTEM RESET archive_command");
    }

    #[tokio::test]
    async fn failed_base_backup_leaves_a_command_written_since_by_someone_else() {
        // Only the enabling change is accepted: a rollback would fail the mock.
        let db = mock(
            vec![
                vec![reload_row(true)],
                vec![
                    file_entry_row(AUTO_CONF, WALG_COMMAND),
                    file_entry_row(AUTO_CONF, "cp %p /archive/%f"),
                ],
            ],
            1,
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let error = service
            .archive_then_push(
                &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, false),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("wal-g backup-push failed"), "{error}");
        drop(service);
        assert_eq!(logged_sql(db).len(), 3);
    }

    #[tokio::test]
    async fn failed_base_backup_is_rolled_back_when_ownership_cannot_be_checked() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![reload_row(true)]])
                .append_query_errors([sea_orm::DbErr::Custom(
                    "permission denied for view pg_file_settings".to_string(),
                )])
                .append_query_results([vec![reload_row(true)]])
                .append_exec_results([exec_ok(), exec_ok()])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let result = service
            .archive_then_push(
                &settings("on", "", None),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, false),
            )
            .await;

        assert!(result.is_err());
        drop(service);
        assert_eq!(
            logged_sql(db)[3],
            "ALTER SYSTEM SET archive_command = '/bin/true'"
        );
    }

    #[tokio::test]
    async fn failed_base_backup_keeps_walg_archiving_but_puts_back_its_walg_env() {
        // walg.env is what tells WAL-G where the working archive is; the
        // command was already WAL-G's, so only the enabling change runs.
        let db = mock(vec![vec![reload_row(true)]], 1);
        let service = make_scope_test_service(db.clone());
        let steps = Steps::default();

        let result = service
            .archive_then_push(
                &settings_in(WALG_COMMAND, None, AUTO_CONF),
                WALG_COMMAND,
                "temps-postgres",
                recorded_steps(&steps, true, false),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            *steps.lock().unwrap(),
            ["write walg.env", "backup-push", "restore walg.env"]
        );
        drop(service);
        assert_eq!(logged_sql(db).len(), 2);
    }

    #[tokio::test]
    async fn successful_base_backup_returns_the_push_result() {
        let db = mock(vec![vec![reload_row(true)]], 1);
        let service = make_scope_test_service(db.clone());

        let value = service
            .archive_then_push(
                &settings("on", "", Some("")),
                WALG_COMMAND,
                "temps-postgres",
                BaseBackupSteps {
                    write_env: async { Ok(()) },
                    push: async { Ok::<_, BackupError>(42) },
                    restore_env: async { panic!("a successful backup keeps the new walg.env") },
                    discard_previous_env: async {
                        Err(BackupError::Internal {
                            message: "rm: cannot remove walg.env.prev".to_string(),
                        })
                    },
                },
            )
            .await
            .unwrap();

        // Failing to delete the copy of the old walg.env does not fail it.
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn failed_rollback_still_reports_the_backup_failure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec_ok()])
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .append_query_results([vec![reload_row(true)], walg_in_auto_conf()])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let error = service
            .archive_then_push(
                &settings("on", "", Some("")),
                WALG_COMMAND,
                "temps-postgres",
                BaseBackupSteps {
                    write_env: async { Ok(()) },
                    push: async { Err::<(), _>(push_failure()) },
                    restore_env: async {
                        Err(BackupError::Internal {
                            message: "mv: cannot move walg.env.prev".to_string(),
                        })
                    },
                    discard_previous_env: async {
                        panic!("a failed backup keeps the old walg.env")
                    },
                },
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("wal-g backup-push failed"), "{error}");
    }
}
