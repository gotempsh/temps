// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Getting a control-plane dump into a fresh TimescaleDB host.
//!
//! The dump can't just be replayed there: its globals drop and recreate the
//! superuser the target's own jobs belong to and overwrite the password this
//! restore connects with, its hypertables need TimescaleDB's restoring mode, and
//! the host may have Docker but no PostgreSQL client tools.

use std::io::{self, BufRead, Write};
use std::path::Path;

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    WaitContainerOptionsBuilder,
};
use futures::StreamExt;

/// Roles the target keeps as they are, whatever the dump says. `postgres` owns
/// TimescaleDB's jobs, so it can't be dropped; the connecting role's password is
/// the one everything after this restore still needs.
const ALWAYS_PROTECTED_ROLE: &str = "postgres";

/// Where the file to restore goes inside the tool's container.
const CONTAINER_INPUT_DIR: &str = "/tmp";
const CONTAINER_INPUT_NAME: &str = "restore-input";

/// Lines of the tool's output kept for the error message.
const TOOL_OUTPUT_TAIL_LINES: &str = "50";

/// Puts the target in TimescaleDB's restoring mode, for every session from now on
/// (`timescaledb_pre_restore` sets it on the database), when TimescaleDB is there.
/// Runs on its own connection so it applies to the tool whichever way it runs, and
/// so [`END_RESTORING_MODE_SQL`] can undo it after a tool that failed.
pub(super) const BEGIN_RESTORING_MODE_SQL: &str = "DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'timescaledb') THEN
    CREATE EXTENSION IF NOT EXISTS timescaledb;
    PERFORM timescaledb_pre_restore();
  END IF;
END $$";

/// Leaves restoring mode, with background jobs back on. A restore that failed after
/// the dump's `DROP EXTENSION` has no `timescaledb_post_restore` left to call, and
/// without the setting reset the database would come back with it still on.
pub(super) const END_RESTORING_MODE_SQL: &str = "DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
    PERFORM timescaledb_post_restore();
  ELSE
    EXECUTE format('ALTER DATABASE %I RESET timescaledb.restoring', current_database());
  END IF;
END $$";

/// The role a line of a `pg_dumpall --globals-only` dump drops, creates or alters.
fn role_statement_target(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("DROP ROLE IF EXISTS ")
        .or_else(|| line.strip_prefix("DROP ROLE "))
        .or_else(|| line.strip_prefix("CREATE ROLE "))
        .or_else(|| line.strip_prefix("ALTER ROLE "))?;
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted.split('"').next();
    }
    rest.split([' ', ';']).next()
}

/// Copy a plain dump, leaving out the statements about `connecting_role` and
/// `postgres` in its globals, and return how many were left out. Only the
/// `pg_dumpall` section is looked at, so table data that happens to read like one
/// of those statements stays: after that section the rest is copied as bytes, and
/// memory doesn't depend on the dump's size.
pub(super) fn filter_protected_roles(
    mut input: impl BufRead,
    mut output: impl Write,
    connecting_role: &str,
) -> io::Result<u64> {
    let mut removed = 0;
    let mut line = Vec::new();
    let mut in_globals = false;
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(removed);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if text.starts_with("-- PostgreSQL database cluster dump complete") {
            in_globals = false;
        } else if text.starts_with("-- PostgreSQL database cluster dump") {
            in_globals = true;
        } else if text.starts_with("-- PostgreSQL database dump") {
            output.write_all(&line)?;
            io::copy(&mut input, &mut output)?;
            return Ok(removed);
        }
        let protected = in_globals
            && role_statement_target(text)
                .is_some_and(|role| role == ALWAYS_PROTECTED_ROLE || role == connecting_role);
        if protected {
            removed += 1;
        } else {
            output.write_all(&line)?;
        }
    }
}

/// Connection details of the database being restored into.
#[derive(Debug, Clone)]
pub(super) struct RestoreTarget {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Option<String>,
}

impl RestoreTarget {
    pub fn from_database_url(database_url: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(database_url)
            .map_err(|error| anyhow::anyhow!("Failed to parse database URL: {}", error))?;
        let decode = |value: &str, field: &str| {
            urlencoding::decode(value)
                .map(|decoded| decoded.into_owned())
                .map_err(|error| {
                    anyhow::anyhow!("Database URL has an invalid {}: {}", field, error)
                })
        };
        Ok(Self {
            host: url.host_str().unwrap_or("localhost").to_string(),
            port: url.port().unwrap_or(5432),
            database: url.path().trim_start_matches('/').to_string(),
            username: decode(url.username(), "username")?,
            password: url
                .password()
                .map(|password| decode(password, "password"))
                .transpose()?,
        })
    }

    /// The tool and its arguments to replay `input`.
    pub fn tool_command(&self, is_plain_sql: bool, input: &str) -> (&'static str, Vec<String>) {
        let connection = [
            "--host".to_string(),
            self.host.clone(),
            "--port".to_string(),
            self.port.to_string(),
            "--username".to_string(),
            self.username.clone(),
            "--dbname".to_string(),
            self.database.clone(),
        ];
        if is_plain_sql {
            // Current control-plane backups are pg_dumpall globals followed by a
            // plain pg_dump stream. `pg_restore` cannot read that format.
            let mut args = vec![
                "--no-password".to_string(),
                "--quiet".to_string(),
                "--set=ON_ERROR_STOP=on".to_string(),
            ];
            args.extend(connection);
            args.extend(["--file".to_string(), input.to_string()]);
            ("psql", args)
        } else {
            let mut args = vec![
                "--no-password".to_string(),
                "--clean".to_string(),
                "--if-exists".to_string(),
                "--no-owner".to_string(),
                "--no-acl".to_string(),
            ];
            args.extend(connection);
            args.push(input.to_string());
            ("pg_restore", args)
        }
    }
}

/// Major version in `psql --version` output (`psql (PostgreSQL) 18.1 (Debian ...)`).
fn tool_major_version(version_output: &str) -> Option<u32> {
    version_output
        .split_whitespace()
        .find_map(|word| word.split('.').next()?.parse().ok())
}

/// Whether the host's own tool can restore into a server of this major version. An
/// older client can't read what a newer server's dump contains, so it counts as
/// not installed.
pub(super) fn host_tool_can_restore(tool: &str, server_major: u32) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| tool_major_version(&String::from_utf8_lossy(&output.stdout)))
        .is_some_and(|major| major >= server_major)
}

/// Run the restore tool in a throwaway container of the server's major version, the
/// way backups run `pg_dump`, on host networking so it reaches the same address the
/// host would. The file goes in through the Docker API, so this also works when the
/// daemon runs in a VM with a different filesystem. The container is removed on
/// every path.
pub(super) async fn run_tool_in_container(
    target: &RestoreTarget,
    is_plain_sql: bool,
    input: &Path,
    server_major: u32,
) -> anyhow::Result<()> {
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| anyhow::anyhow!("Failed to connect to Docker: {}", error))?;
    let image = format!("postgres:{server_major}");
    temps_backup::engines::image_pull::ensure_image_pulled_v2(&image, "cli-restore")
        .await
        .map_err(|error| anyhow::anyhow!("Failed to pull {}: {}", image, error))?;

    let name = format!("temps-restore-tool-{}", uuid::Uuid::new_v4());
    let container_input = format!("{CONTAINER_INPUT_DIR}/{CONTAINER_INPUT_NAME}");
    let (tool, args) = target.tool_command(is_plain_sql, &container_input);
    let mut cmd = vec![tool.to_string()];
    cmd.extend(args);
    let mut env = vec!["PGCONNECT_TIMEOUT=30".to_string()];
    if let Some(password) = &target.password {
        env.push(format!("PGPASSWORD={password}"));
    }
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(&name).build()),
            ContainerCreateBody {
                image: Some(image.clone()),
                cmd: Some(cmd),
                env: Some(env),
                host_config: Some(HostConfig {
                    network_mode: Some("host".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("Failed to create restore container {}: {}", name, error)
        })?;

    let finished = run_created_container(&docker, &name, tool, input).await;
    if let Err(error) = docker
        .remove_container(
            &name,
            Some(RemoveContainerOptionsBuilder::new().force(true).build()),
        )
        .await
    {
        tracing::warn!(container = %name, "Could not remove the restore container: {}", error);
    }
    finished
}

async fn run_created_container(
    docker: &bollard::Docker,
    name: &str,
    tool: &str,
    input: &Path,
) -> anyhow::Result<()> {
    temps_providers::externalsvc::container_upload::upload_file_to_container(
        docker,
        name,
        input,
        CONTAINER_INPUT_DIR,
        CONTAINER_INPUT_NAME,
        0o644,
    )
    .await
    .map_err(|error| anyhow::anyhow!("Failed to copy the backup into {}: {}", name, error))?;
    docker
        .start_container(
            name,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Failed to start {}: {}", name, error))?;

    let mut wait = docker.wait_container(name, Some(WaitContainerOptionsBuilder::new().build()));
    let exit_code = match wait.next().await {
        Some(Ok(response)) => response.status_code,
        // bollard reports a non-zero exit as this error, not as a response.
        Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
        Some(Err(error)) => {
            return Err(anyhow::anyhow!("Failed waiting for {}: {}", name, error));
        }
        None => return Err(anyhow::anyhow!("{} ended without an exit code", name)),
    };
    if exit_code == 0 {
        return Ok(());
    }

    let mut logs = docker.logs(
        name,
        Some(
            LogsOptionsBuilder::new()
                .stdout(true)
                .stderr(true)
                .tail(TOOL_OUTPUT_TAIL_LINES)
                .build(),
        ),
    );
    let mut output = String::new();
    while let Some(Ok(chunk)) = logs.next().await {
        output.push_str(&chunk.to_string());
    }
    Err(anyhow::anyhow!(
        "{} failed with exit code {}: {}",
        tool,
        exit_code,
        output.trim()
    ))
}

/// What to do about a service's backup row in the restored control plane.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BackupRowCheck {
    /// The row already says what the manifest says.
    Matches,
    /// The snapshot caught the row mid-run. The manifest, published and signed only
    /// once the run finished, says how it ended.
    TakeFromManifest,
    /// The two really disagree, so this isn't the backup the manifest means.
    Conflict,
}

/// The control-plane snapshot is taken during the scheduled run it belongs to, so
/// the run's own service backups are in it still in progress, with no location yet.
pub(super) fn check_backup_row(
    row: &temps_entities::backups::Model,
    manifest_location: &str,
    manifest_type: &str,
) -> BackupRowCheck {
    if row.state == "completed" && row.s3_location == manifest_location {
        return BackupRowCheck::Matches;
    }
    let in_progress = matches!(row.state.as_str(), "pending" | "running");
    if in_progress && row.s3_location.is_empty() && row.backup_type == manifest_type {
        return BackupRowCheck::TakeFromManifest;
    }
    BackupRowCheck::Conflict
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(dump: &str, role: &str) -> (String, u64) {
        let mut output = Vec::new();
        let removed =
            filter_protected_roles(dump.as_bytes(), &mut output, role).expect("in-memory copy");
        (String::from_utf8(output).expect("utf-8"), removed)
    }

    const GLOBALS: &str = "\
--
-- PostgreSQL database cluster dump
--

DROP ROLE IF EXISTS postgres;
DROP ROLE IF EXISTS temps_owner;
DROP ROLE IF EXISTS temps_reader;
CREATE ROLE postgres;
ALTER ROLE postgres WITH SUPERUSER INHERIT PASSWORD 'SCRAM-source';
CREATE ROLE temps_owner;
ALTER ROLE temps_owner WITH LOGIN PASSWORD 'SCRAM-source';
CREATE ROLE temps_reader;
ALTER ROLE temps_reader WITH LOGIN PASSWORD 'SCRAM-reader';
--
-- PostgreSQL database cluster dump complete
--
";

    #[test]
    fn the_superuser_and_the_connecting_role_are_left_as_they_are() {
        let (sql, removed) = filtered(GLOBALS, "temps_owner");
        assert_eq!(removed, 6);
        assert!(!sql.contains("postgres;"), "{sql}");
        assert!(!sql.contains("ROLE postgres"), "{sql}");
        assert!(!sql.contains("temps_owner"), "{sql}");
        // Roles the application created still come across.
        assert!(sql.contains("DROP ROLE IF EXISTS temps_reader;"), "{sql}");
        assert!(sql.contains("CREATE ROLE temps_reader;"), "{sql}");
        assert!(sql.contains("'SCRAM-reader'"), "{sql}");
    }

    #[test]
    fn a_quoted_role_is_recognised_and_a_similar_name_is_not() {
        let dump = "-- PostgreSQL database cluster dump\n\
                    ALTER ROLE \"my-owner\" WITH PASSWORD 'a';\n\
                    ALTER ROLE postgres2 WITH PASSWORD 'b';\n";
        let (sql, removed) = filtered(dump, "my-owner");
        assert_eq!(removed, 1);
        assert!(!sql.contains("my-owner"), "{sql}");
        assert!(sql.contains("postgres2"), "{sql}");
    }

    /// A customer's row can read like a role statement. It's their data.
    #[test]
    fn table_data_that_reads_like_a_role_statement_is_untouched() {
        let database = "--\n-- PostgreSQL database dump\n--\n\
                        COPY public.notes (body) FROM stdin;\n\
                        DROP ROLE IF EXISTS postgres;\n\
                        \\.\n";
        let dump = format!("{GLOBALS}{database}");
        let (sql, removed) = filtered(&dump, "temps_owner");
        assert_eq!(removed, 6);
        assert!(sql.ends_with(database), "{sql}");
    }

    #[test]
    fn a_dump_without_globals_passes_through_unchanged() {
        let dump = "COPY public.notes (body) FROM stdin;\nDROP ROLE postgres;\n\\.\n";
        let (sql, removed) = filtered(dump, "postgres");
        assert_eq!(removed, 0);
        assert_eq!(sql, dump);
    }

    #[test]
    fn bytes_that_are_not_utf8_survive_the_copy() {
        let dump = b"-- PostgreSQL database dump\nCOPY t FROM stdin;\n\xff\xfe\n\\.\n";
        let mut output = Vec::new();
        filter_protected_roles(&dump[..], &mut output, "postgres").expect("in-memory copy");
        assert_eq!(output, dump);
    }

    #[test]
    fn the_target_is_read_from_the_url_with_its_password_decoded() {
        let target = RestoreTarget::from_database_url(
            "postgres://owner:p%40ss%2Fword@db.internal:6543/temps",
        )
        .expect("valid url");
        assert_eq!(target.host, "db.internal");
        assert_eq!(target.port, 6543);
        assert_eq!(target.database, "temps");
        assert_eq!(target.username, "owner");
        assert_eq!(target.password.as_deref(), Some("p@ss/word"));
    }

    #[test]
    fn the_tool_stops_at_the_first_error_and_never_carries_the_password() {
        let target = RestoreTarget::from_database_url("postgres://owner:secret@localhost/temps")
            .expect("valid url");
        let (tool, args) = target.tool_command(true, "/tmp/in");
        assert_eq!(tool, "psql");
        assert!(args.contains(&"--set=ON_ERROR_STOP=on".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("secret")));
        assert_eq!(target.tool_command(false, "/tmp/in").0, "pg_restore");
    }

    #[test]
    fn the_tool_version_is_read_from_its_own_output() {
        assert_eq!(
            tool_major_version("psql (PostgreSQL) 18.1 (Debian 18.1-1)"),
            Some(18)
        );
        assert_eq!(tool_major_version("pg_restore (PostgreSQL) 16.4"), Some(16));
        assert_eq!(tool_major_version("nothing useful"), None);
    }

    /// Against real TimescaleDB: what makes a dump restorable is the setting these
    /// two statements manage, and a database left with it on has its jobs stopped.
    #[tokio::test]
    async fn restoring_mode_reaches_later_sessions_and_is_always_left() {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let Ok(test_db) = temps_database::test_utils::TestDatabase::new_isolated().await else {
            eprintln!("Docker unavailable; skipping the restoring mode test");
            return;
        };
        let db = test_db.connection();
        let restoring = || async {
            db.query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_db_role_setting s \
                 JOIN pg_database d ON d.oid = s.setdatabase \
                 WHERE d.datname = current_database() AND s.setrole = 0 \
                 AND 'timescaledb.restoring=on' = ANY(s.setconfig)) AS restoring"
                    .to_string(),
            ))
            .await
            .expect("query the database's settings")
            .expect("one row")
            .try_get::<bool>("", "restoring")
            .expect("a boolean")
        };
        let run = |sql: &'static str| db.execute_unprepared(sql);

        assert!(!restoring().await);
        run(BEGIN_RESTORING_MODE_SQL).await.expect("begin");
        assert!(restoring().await, "set on the database, not this session");
        run(END_RESTORING_MODE_SQL).await.expect("end");
        assert!(!restoring().await);

        // A restore that failed after the dump's `DROP EXTENSION`.
        run(BEGIN_RESTORING_MODE_SQL).await.expect("begin again");
        run("DROP EXTENSION timescaledb CASCADE")
            .await
            .expect("drop the extension");
        run(END_RESTORING_MODE_SQL)
            .await
            .expect("end without the extension");
        assert!(!restoring().await);
    }

    fn row(state: &str, location: &str) -> temps_entities::backups::Model {
        temps_entities::backups::Model {
            id: 3,
            name: "run".to_string(),
            backup_id: "uuid".to_string(),
            schedule_id: Some(1),
            backup_type: "full".to_string(),
            state: state.to_string(),
            started_at: chrono::Utc::now(),
            finished_at: None,
            size_bytes: None,
            file_count: None,
            s3_source_id: 1,
            s3_location: location.to_string(),
            error_message: None,
            metadata: "{}".to_string(),
            checksum: None,
            compression_type: "gzip".to_string(),
            created_by: 1,
            expires_at: None,
            tags: "[]".to_string(),
            schedule_run_id: Some(1),
        }
    }

    /// The scheduled set that couldn't be restored: the snapshot caught this
    /// run's service backups still running.
    #[test]
    fn a_row_the_snapshot_caught_mid_run_takes_the_manifests_outcome() {
        assert_eq!(
            check_backup_row(&row("running", ""), "s3://b/k", "full"),
            BackupRowCheck::TakeFromManifest
        );
        assert_eq!(
            check_backup_row(&row("pending", ""), "s3://b/k", "full"),
            BackupRowCheck::TakeFromManifest
        );
    }

    #[test]
    fn a_row_that_really_disagrees_is_still_refused() {
        for disagreeing in [
            row("completed", "s3://b/other"),
            row("failed", ""),
            row("running", "s3://b/other"),
        ] {
            assert_eq!(
                check_backup_row(&disagreeing, "s3://b/k", "full"),
                BackupRowCheck::Conflict,
                "{disagreeing:?}"
            );
        }
        assert_eq!(
            check_backup_row(&row("running", ""), "s3://b/k", "incremental"),
            BackupRowCheck::Conflict
        );
        assert_eq!(
            check_backup_row(&row("completed", "s3://b/k"), "s3://b/k", "full"),
            BackupRowCheck::Matches
        );
    }
}
