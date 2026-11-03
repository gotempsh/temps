// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Getting a control-plane dump into a fresh TimescaleDB host.
//!
//! The dump can't just be replayed there: its globals drop and recreate the
//! superuser the target's own jobs belong to and overwrite the password this
//! restore connects with, its hypertables need TimescaleDB's restoring mode, and
//! the host may have Docker but no PostgreSQL client tools.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// What a libpq TLS setting's value is, which decides how it gets into a container.
#[derive(Clone, Copy)]
enum TlsValue {
    Text,
    /// A file, the root certificate's `system` aside.
    RootCert,
    File {
        mode: u32,
    },
    Directory,
}

/// libpq's TLS settings, by database URL parameter and by the variable that sets the
/// same thing for a tool. `sslpassword` has no variable, so a client key protected by
/// one fails the connection rather than going without it.
const LIBPQ_TLS_SETTINGS: &[(&str, &str, TlsValue)] = &[
    ("sslmode", "PGSSLMODE", TlsValue::Text),
    ("sslnegotiation", "PGSSLNEGOTIATION", TlsValue::Text),
    ("sslsni", "PGSSLSNI", TlsValue::Text),
    (
        "ssl_min_protocol_version",
        "PGSSLMINPROTOCOLVERSION",
        TlsValue::Text,
    ),
    (
        "ssl_max_protocol_version",
        "PGSSLMAXPROTOCOLVERSION",
        TlsValue::Text,
    ),
    ("channel_binding", "PGCHANNELBINDING", TlsValue::Text),
    ("sslrootcert", "PGSSLROOTCERT", TlsValue::RootCert),
    ("sslcert", "PGSSLCERT", TlsValue::File { mode: 0o644 }),
    // libpq refuses a client key others can read.
    ("sslkey", "PGSSLKEY", TlsValue::File { mode: 0o600 }),
    ("sslcrl", "PGSSLCRL", TlsValue::File { mode: 0o644 }),
    ("sslcrldir", "PGSSLCRLDIR", TlsValue::Directory),
];

/// Connection details of the database being restored into.
#[derive(Debug, Clone)]
pub(super) struct RestoreTarget {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Option<String>,
    /// The URL's TLS settings as the tool's environment. Without them the tool would
    /// fall back to libpq's default, which accepts any certificate or none at all,
    /// and send the password over that.
    pub tls_env: Vec<(&'static str, String)>,
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
            tls_env: url
                .query_pairs()
                .filter_map(|(name, value)| {
                    let (_, variable, _) = LIBPQ_TLS_SETTINGS
                        .iter()
                        .find(|(parameter, _, _)| *parameter == name)?;
                    Some((*variable, value.into_owned()))
                })
                .collect(),
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

/// A TLS file to put in the tool's container before it starts.
#[derive(Debug, PartialEq, Eq)]
struct TlsUpload {
    variable: &'static str,
    host_path: PathBuf,
    file_name: String,
    mode: u32,
}

/// The tool's TLS environment in a container, and the files it names. A tool on the
/// host inherits this process's variables and sees its files; a container gets
/// neither, and without them `verify-full` would quietly become no verification.
fn container_tls_settings(
    target: &RestoreTarget,
    ambient: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<(Vec<String>, Vec<TlsUpload>)> {
    let mut env = Vec::new();
    let mut uploads = Vec::new();
    for (parameter, variable, kind) in LIBPQ_TLS_SETTINGS {
        let from_url = target
            .tls_env
            .iter()
            .find(|(name, _)| name == variable)
            .map(|(_, value)| value.clone());
        let Some(value) = from_url
            .or_else(|| ambient(variable))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let upload_mode = match kind {
            TlsValue::Text => None,
            TlsValue::RootCert if value == "system" => None,
            TlsValue::RootCert => Some(0o644),
            TlsValue::File { mode } => Some(*mode),
            TlsValue::Directory => {
                return Err(anyhow::anyhow!(
                    "{parameter} names a directory ({value}), which can't be copied into the \
                     restore container. Install a PostgreSQL client on this host, or use sslcrl"
                ));
            }
        };
        let Some(mode) = upload_mode else {
            env.push(format!("{variable}={value}"));
            continue;
        };
        let file_name = format!("restore-{parameter}");
        env.push(format!("{variable}={CONTAINER_INPUT_DIR}/{file_name}"));
        uploads.push(TlsUpload {
            variable,
            host_path: PathBuf::from(value),
            file_name,
            mode,
        });
    }
    Ok((env, uploads))
}

/// A libpq setting from this process's environment, for a tool in a container.
#[allow(
    clippy::disallowed_methods,
    reason = "bootstrap: libpq's own variables, which a tool on the host inherits, passed on so a tool in a container connects the same way"
)]
fn ambient_libpq_setting(variable: &str) -> Option<String> {
    std::env::var(variable).ok()
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
    let (tls_env, tls_uploads) = container_tls_settings(target, ambient_libpq_setting)?;
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
    env.extend(tls_env);
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

    let finished = run_created_container(&docker, &name, tool, input, &tls_uploads).await;
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

/// Longest a restore tool may run. Large databases legitimately take many hours, so
/// this only catches a tool that's stuck, such as one waiting on a lock forever. The
/// caller then removes what's left of it and takes the database out of restoring mode,
/// which can't happen while this is still waiting.
pub(super) const RESTORE_TOOL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) async fn within_restore_deadline<F: std::future::Future>(
    deadline: Duration,
    tool: &str,
    work: F,
) -> anyhow::Result<F::Output> {
    tokio::time::timeout(deadline, work).await.map_err(|_| {
        anyhow::anyhow!(
            "{} did not finish within {} seconds and was stopped. The restore is incomplete; \
             check the target database for sessions holding locks before retrying",
            tool,
            deadline.as_secs()
        )
    })
}

async fn run_created_container(
    docker: &bollard::Docker,
    name: &str,
    tool: &str,
    input: &Path,
    tls_uploads: &[TlsUpload],
) -> anyhow::Result<()> {
    for upload in tls_uploads {
        temps_providers::externalsvc::container_upload::upload_file_to_container(
            docker,
            name,
            &upload.host_path,
            CONTAINER_INPUT_DIR,
            &upload.file_name,
            upload.mode,
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "Failed to copy {} ({}) into {}: {}",
                upload.variable,
                upload.host_path.display(),
                name,
                error
            )
        })?;
    }
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
    let waited = within_restore_deadline(RESTORE_TOOL_TIMEOUT, tool, wait.next()).await?;
    let exit_code = match waited {
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

/// A row's metadata once it takes its outcome from the manifest, naming the WAL-G
/// snapshot to fetch. Engines record this only after the backup finishes, so a row
/// the snapshot caught mid-run never has it, and a restore without it fetches `LATEST`,
/// which may be a different backup from the one the manifest vouches for. It's
/// derivable, because every engine labels its snapshot with the row's `backup_id`. A
/// snapshot without that label fails the fetch rather than restoring the wrong one.
pub(super) fn metadata_with_walg_selector(
    metadata: &str,
    backup_id: &str,
) -> Result<String, String> {
    let mut parsed: serde_json::Value = serde_json::from_str(metadata)
        .map_err(|error| format!("metadata of backup {backup_id} is invalid JSON: {error}"))?;
    let fields = parsed
        .as_object_mut()
        .ok_or_else(|| format!("metadata of backup {backup_id} is not a JSON object"))?;
    if !fields.contains_key("walg_target_user_data") {
        fields.insert("walg_identity_version".to_string(), serde_json::json!(1));
        fields.insert(
            "walg_target_user_data".to_string(),
            serde_json::json!({ "temps_backup_id": backup_id }),
        );
    }
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stuck_restore_tool_fails_naming_the_tool_rather_than_waiting_forever() {
        let error = within_restore_deadline(
            Duration::from_millis(5),
            "psql",
            std::future::pending::<()>(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("psql did not finish"), "{error}");
        assert!(
            within_restore_deadline(Duration::from_secs(5), "psql", async { 7 })
                .await
                .is_ok_and(|value| value == 7)
        );
    }

    #[test]
    fn a_row_taken_from_the_manifest_names_its_own_walg_snapshot() {
        let merged = metadata_with_walg_selector(r#"{"lsn":"0/1"}"#, "uuid-1").unwrap();
        let merged: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(merged["lsn"], "0/1", "other metadata is kept");
        assert_eq!(merged["walg_identity_version"], 1);
        assert_eq!(merged["walg_target_user_data"]["temps_backup_id"], "uuid-1");
    }

    #[test]
    fn a_selector_the_row_already_has_is_left_alone() {
        let existing =
            r#"{"walg_identity_version":1,"walg_target_user_data":{"temps_backup_id":"kept"}}"#;
        let merged = metadata_with_walg_selector(existing, "uuid-1").unwrap();
        assert!(merged.contains("kept"), "{merged}");
    }

    #[test]
    fn metadata_that_cannot_carry_a_selector_is_refused_naming_the_backup() {
        for bad in ["not json", "[]"] {
            let error = metadata_with_walg_selector(bad, "uuid-1").unwrap_err();
            assert!(error.contains("uuid-1"), "{error}");
        }
    }

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
    fn the_urls_tls_settings_reach_the_tool_environment_and_nothing_else_does() {
        let target = RestoreTarget::from_database_url(
            "postgres://owner:secret@db.internal/temps?sslmode=verify-full\
             &sslrootcert=%2Fetc%2Fca.pem&application_name=temps",
        )
        .expect("valid url");
        assert_eq!(
            target.tls_env,
            vec![
                ("PGSSLMODE", "verify-full".to_string()),
                ("PGSSLROOTCERT", "/etc/ca.pem".to_string()),
            ]
        );
    }

    #[test]
    fn a_container_gets_tls_settings_with_their_files_at_paths_it_can_read() {
        let target = RestoreTarget::from_database_url(
            "postgres://owner@db.internal/temps?sslmode=verify-full&sslrootcert=/etc/ca.pem\
             &sslkey=/etc/client.key",
        )
        .expect("valid url");
        let (env, uploads) = container_tls_settings(&target, |_| None).expect("settings");
        assert_eq!(
            env,
            vec![
                "PGSSLMODE=verify-full",
                "PGSSLROOTCERT=/tmp/restore-sslrootcert",
                "PGSSLKEY=/tmp/restore-sslkey",
            ]
        );
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[0].host_path, PathBuf::from("/etc/ca.pem"));
        assert_eq!(uploads[1].host_path, PathBuf::from("/etc/client.key"));
        assert_eq!(uploads[1].mode, 0o600, "libpq refuses a readable key");
    }

    #[test]
    fn a_container_gets_what_a_host_tool_would_inherit_with_the_url_taking_priority() {
        let target = RestoreTarget::from_database_url("postgres://owner@db/temps?sslmode=require")
            .expect("valid url");
        let ambient = |variable: &str| match variable {
            "PGSSLMODE" => Some("disable".to_string()),
            "PGCHANNELBINDING" => Some("require".to_string()),
            _ => None,
        };
        let (env, _) = container_tls_settings(&target, ambient).expect("settings");
        assert_eq!(env, vec!["PGSSLMODE=require", "PGCHANNELBINDING=require"]);
    }

    #[test]
    fn the_systems_root_certificates_are_not_a_file_to_copy() {
        let target = RestoreTarget::from_database_url(
            "postgres://owner@db/temps?sslmode=verify-full&sslrootcert=system",
        )
        .expect("valid url");
        let (env, uploads) = container_tls_settings(&target, |_| None).expect("settings");
        assert!(env.contains(&"PGSSLROOTCERT=system".to_string()), "{env:?}");
        assert!(uploads.is_empty(), "{uploads:?}");
    }

    #[test]
    fn a_setting_that_cant_reach_a_container_refuses_rather_than_going_without() {
        let target =
            RestoreTarget::from_database_url("postgres://owner@db/temps?sslcrldir=/etc/crls")
                .expect("valid url");
        let error = container_tls_settings(&target, |_| None).unwrap_err();
        assert!(error.to_string().contains("/etc/crls"), "{error}");
    }

    /// What the container tool does about TLS, against a real server: it refuses a
    /// server the URL says must have it, and verifies with the file the URL names.
    #[tokio::test]
    async fn the_container_tool_connects_the_way_the_database_url_demands() {
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping test");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not responding, skipping test");
            return;
        }

        const MAJOR: u32 = 17;
        let image = format!("postgres:{MAJOR}");
        temps_backup::engines::image_pull::ensure_image_pulled_v2(&image, "cli-restore-test")
            .await
            .expect("pull the server image");
        let run = uuid::Uuid::new_v4().simple().to_string();
        let port = 20_000 + u16::from_str_radix(&run[..4], 16).expect("hex") % 20_000;
        let name = format!("temps-restore-tls-test-{run}");
        // On its own thread and runtime: this test's is shutting down when it runs.
        struct RemoveOnDrop(String);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let name = self.0.clone();
                let removal = std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("cleanup runtime");
                    runtime.block_on(async {
                        let docker = bollard::Docker::connect_with_local_defaults()
                            .expect("connect for cleanup");
                        let options = RemoveContainerOptionsBuilder::new().force(true).build();
                        let _ = docker.remove_container(&name, Some(options)).await;
                    });
                });
                let _ = removal.join();
            }
        }
        let start_server = |name: String, tls: bool| {
            let docker = docker.clone();
            let image = image.clone();
            async move {
                let mut cmd = vec![
                    "postgres".to_string(),
                    "-c".to_string(),
                    format!("port={}", if tls { port } else { port + 1 }),
                ];
                if tls {
                    for setting in [
                        "ssl=on",
                        "ssl_cert_file=/etc/ssl/certs/ssl-cert-snakeoil.pem",
                        "ssl_key_file=/etc/ssl/private/ssl-cert-snakeoil.key",
                    ] {
                        cmd.extend(["-c".to_string(), setting.to_string()]);
                    }
                }
                docker
                    .create_container(
                        Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                        ContainerCreateBody {
                            image: Some(image),
                            cmd: Some(cmd),
                            env: Some(vec!["POSTGRES_PASSWORD=secret".to_string()]),
                            host_config: Some(HostConfig {
                                network_mode: Some("host".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("create the server");
                docker
                    .start_container(
                        &name,
                        None::<bollard::query_parameters::StartContainerOptions>,
                    )
                    .await
                    .expect("start the server");
            }
        };
        let exec = |name: String, cmd: Vec<String>| {
            let docker = docker.clone();
            async move {
                let created = docker
                    .create_exec(
                        &name,
                        bollard::exec::CreateExecOptions {
                            cmd: Some(cmd),
                            attach_stdout: Some(true),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("create exec");
                let mut stdout = String::new();
                if let bollard::exec::StartExecResults::Attached { mut output, .. } = docker
                    .start_exec(&created.id, None)
                    .await
                    .expect("start exec")
                {
                    while let Some(Ok(chunk)) = output.next().await {
                        stdout.push_str(&chunk.to_string());
                    }
                }
                let code = docker
                    .inspect_exec(&created.id)
                    .await
                    .expect("inspect exec")
                    .exit_code;
                (code, stdout)
            }
        };

        let plain_name = format!("{name}-plain");
        let _tls_server = RemoveOnDrop(name.clone());
        let _plain_server = RemoveOnDrop(plain_name.clone());
        start_server(name.clone(), true).await;
        start_server(plain_name.clone(), false).await;

        // The image's entrypoint restarts the server once after initialising, so
        // ask over TCP, which only the real one listens on.
        for (server, server_port) in [(&name, port), (&plain_name, port + 1)] {
            let ready = ["pg_isready", "-h", "127.0.0.1", "-p"]
                .map(String::from)
                .into_iter()
                .chain([server_port.to_string()])
                .collect::<Vec<_>>();
            let mut waited = 0;
            while exec(server.clone(), ready.clone()).await.0 != Some(0) {
                waited += 1;
                assert!(waited < 120, "{server} never became ready");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        let (_, certificate) = exec(
            name.clone(),
            vec![
                "cat".to_string(),
                "/etc/ssl/certs/ssl-cert-snakeoil.pem".to_string(),
            ],
        )
        .await;
        let files = tempfile::tempdir().expect("temp dir");
        let right_root = files.path().join("right.pem");
        std::fs::write(&right_root, certificate).expect("write the root certificate");
        let input = files.path().join("input.sql");
        std::fs::write(&input, "SELECT 1;\n").expect("write the input");

        let restore = |url: String| {
            let input = input.clone();
            async move {
                let target = RestoreTarget::from_database_url(&url).expect("valid url");
                run_tool_in_container(&target, true, &input, MAJOR).await
            }
        };
        let base = "postgres://postgres:secret@127.0.0.1";

        // Without the URL's sslmode reaching the tool, these two would connect anyway.
        let plain = restore(format!("{base}:{}/postgres?sslmode=require", port + 1))
            .await
            .expect_err("the server has no TLS and the URL requires it");
        assert!(plain.to_string().contains("SSL"), "{plain}");
        let missing = restore(format!(
            "{base}:{port}/postgres?sslmode=verify-ca&sslrootcert=/nonexistent/root.pem"
        ))
        .await
        .expect_err("a named root certificate that can't be copied stops the restore");
        assert!(missing.to_string().contains("PGSSLROOTCERT"), "{missing}");

        restore(format!(
            "{base}:{port}/postgres?sslmode=verify-ca&sslrootcert={}",
            right_root.display()
        ))
        .await
        .expect("the server's certificate verifies against the root the URL names");
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
