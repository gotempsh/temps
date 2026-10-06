// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `MariadbDumpEngine`: logical (`mariadb-dump`) backup of an external MariaDB
//! service, implemented against `engine_v2::BackupEngine`.
//!
//! This is the **fallback** engine (no PITR), the MariaDB analog of
//! `postgres_pgdump`. The preferred PITR path is `mariadb_physical`
//! (physical `mariadb-backup` base + binary-log archiving). Dispatch
//! (`dispatch::resolve_engine_key`) selects this engine only when the
//! physical-backup prerequisites are absent.
//!
//! ## Flow
//! 1. Load + decrypt the external-service row for the root password + image.
//! 2. Validate the configured S3 source.
//! 3. `docker exec` `mariadb-dump --databases ... --single-transaction | gzip`
//!    inside the running container, streaming the gzipped stdout to a file in
//!    an attempt-scoped host temp dir (`dump_capture::DumpAttempt`), so a
//!    retry or a concurrent attempt never truncates another attempt's file.
//!    Credentials travel via `MYSQL_PWD` env — never argv (PR #149).
//! 4. Upload the `.sql.gz` to S3.
//! 5. Write the `metadata.json` companion.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{json, Value};
use tracing::{debug, info};

use super::dump_capture::{bounded_tail, DumpAttempt, DumpCaptureError};
use super::mariadb_exec::exec_stream_stdout_to_file;
use super::v2_common;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};

pub(crate) const ENGINE_KEY: &str = "mariadb_dump";
const DUMP_FILE_SUFFIX: &str = "dump.sql.gz";
/// Database name used in user-facing dump errors.
const MARIADB_TOOL: &str = "MariaDB";
/// File name of the dump inside the attempt's host directory.
const HOST_DUMP_FILE: &str = "dump.sql.gz";
/// What the dump is read from, for error messages: the exec's stdout, not a
/// file inside the container.
const EXEC_STDOUT: &str = "stdout of `docker exec`";
/// Upper bound on the stderr carried in an export error.
const MAX_STDERR_IN_ERROR: usize = 4_000;

/// In-container shell that dumps all user databases and gzips the result.
/// Credentials are NOT present here — `-uroot` relies on `MYSQL_PWD` from the
/// exec env. Keep it that way (PR #149).
const DUMP_SHELL: &str = "if command -v mariadb-dump >/dev/null 2>&1; then dump=mariadb-dump; else dump=mysqldump; fi; \
     if command -v mariadb >/dev/null 2>&1; then client=mariadb; else client=mysql; fi; \
     dbs=$($client -N -B -uroot -e \"SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') ORDER BY SCHEMA_NAME\"); \
     if [ -z \"$dbs\" ]; then echo '-- No user databases to dump'; exit 0; fi; \
     $dump --databases $dbs --single-transaction --quick -uroot | gzip";

pub struct MariadbDumpDeps {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub docker: bollard::Docker,
}

pub struct MariadbDumpEngine {
    deps: Arc<MariadbDumpDeps>,
}

impl MariadbDumpEngine {
    pub fn new(deps: MariadbDumpDeps) -> Self {
        Self {
            deps: Arc::new(deps),
        }
    }
}

#[async_trait]
impl BackupEngine for MariadbDumpEngine {
    fn engine(&self) -> &'static str {
        ENGINE_KEY
    }

    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
        let backup_id = ctx.backup_id;
        let deps = Arc::clone(&self.deps);

        let service_id = v2_common::require_i32_param(&ctx.params, "service_id")?;
        let s3_source_id = v2_common::require_i32_param(&ctx.params, "s3_source_id")?;

        let service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(deps.db.as_ref())
            .await
            .map_err(|e| BackupError::Failed {
                reason: format!("db error loading service {}: {}", service_id, e),
            })?
            .ok_or_else(|| BackupError::PermanentFailure {
                reason: format!("service {} not found", service_id),
            })?;

        let (s3_source, s3_client) = v2_common::load_and_build_s3_client(
            deps.db.as_ref(),
            &deps.encryption_service,
            s3_source_id,
            "mariadb-dump-engine",
        )
        .await?;
        v2_common::assert_bucket_reachable(&s3_client, &s3_source.bucket_name).await?;

        let backup_uuid = v2_common::load_backup_uuid(deps.db.as_ref(), backup_id).await?;
        let s3_key = v2_common::build_external_service_s3_key(
            &s3_source.bucket_path,
            "mariadb",
            &service.name,
            &backup_uuid,
            DUMP_FILE_SUFFIX,
        );

        info!(
            backup_id,
            service_id,
            s3_key = %s3_key,
            "MariadbDumpEngine: starting logical dump",
        );

        let config_json = deps
            .encryption_service
            .decrypt_string(service.config.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| "{}".to_string());
        let root_password = root_password_from_config(&config_json);

        let container_name = format!("mariadb-{}", service.name);
        // This attempt's own host directory: a retry, or an attempt racing a
        // lease that was lost, never truncates or deletes another attempt's
        // file. Dropping `attempt` deletes the directory on every exit path.
        let attempt = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid)?;

        // Credentials via env only (MYSQL_PWD / MARIADB_PWD) — never argv.
        let env = vec![
            format!("MYSQL_PWD={}", root_password),
            format!("MARIADB_PWD={}", root_password),
        ];

        let file_size = dump_into_attempt(
            &deps.docker,
            &container_name,
            DUMP_SHELL,
            &env,
            &attempt,
            &ctx.cancel,
        )
        .await?;
        let file_size = i64::try_from(file_size).map_err(|_| BackupError::Failed {
            reason: format!(
                "mariadb-dump output for backup {backup_id} is larger than i64::MAX bytes"
            ),
        })?;
        let host_dump_path_str = attempt
            .host_path(HOST_DUMP_FILE)
            .to_string_lossy()
            .into_owned();

        if ctx.cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }
        let tags = v2_common::BackupTags::load_for_backup(&ctx.db, ctx.backup_id).await;
        v2_common::upload_file(
            &s3_client,
            &s3_source.bucket_name,
            &s3_key,
            &host_dump_path_str,
            "application/x-gzip",
            file_size,
            Some(&tags),
            &ctx.cancel,
        )
        .await
        .map_err(|error| {
            DumpCaptureError::upload(MARIADB_TOOL, &s3_source.bucket_name, &s3_key, error)
        })?;
        // Deletes this attempt's host dir (and the dump in it); every early
        // return above does the same when `attempt` drops.
        drop(attempt);

        let metadata_key = v2_common::derive_metadata_key(&s3_key);
        v2_common::write_metadata_companion(
            &s3_client,
            &s3_source.bucket_name,
            &metadata_key,
            ENGINE_KEY,
            &backup_uuid,
            &s3_key,
            file_size,
            s3_source_id,
            "gzip",
            Some(json!({
                "backup_tool": "mariadb-dump",
                "pitr": false,
                "service": { "id": service_id, "name": service.name },
            })),
        )
        .await?;

        info!(
            backup_id,
            key = %s3_key,
            size_bytes = file_size,
            "MariadbDumpEngine: backup complete",
        );

        Ok(BackupOutcome {
            location: s3_key,
            size_bytes: Some(file_size),
            compression: "gzip".to_string(),
        })
    }
}

/// Stream `shell`'s stdout from `container_name` (via `docker exec`) into this
/// attempt's own host file and return its size.
///
/// The file lives in the attempt's private directory, so it can never be
/// another attempt's file. On every failure the partial file is removed
/// here; the directory itself goes when `attempt` drops.
async fn dump_into_attempt(
    docker: &bollard::Docker,
    container_name: &str,
    shell: &str,
    env: &[String],
    attempt: &DumpAttempt,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<u64, DumpCaptureError> {
    let host_path = attempt.host_path(HOST_DUMP_FILE);
    let outcome = async {
        let exec =
            exec_stream_stdout_to_file(docker, container_name, shell, env, &host_path, cancel)
                .await
                .map_err(|error| DumpCaptureError::exec(MARIADB_TOOL, container_name, error))?;
        if exec.exit_code != 0 {
            return Err(DumpCaptureError::Export {
                tool: MARIADB_TOOL,
                container: container_name.to_string(),
                exit_code: exec.exit_code,
                stderr: bounded_tail(&exec.stderr, MAX_STDERR_IN_ERROR),
            });
        }
        if !exec.stderr.trim().is_empty() {
            debug!(
                container = container_name,
                attempt = attempt.attempt_id(),
                "mariadb-dump stderr: {}",
                exec.stderr.trim()
            );
        }
        let size = tokio::fs::metadata(&host_path)
            .await
            .map_err(|e| DumpCaptureError::DumpUnreadable {
                tool: MARIADB_TOOL,
                container: container_name.to_string(),
                container_path: EXEC_STDOUT.to_string(),
                host_path: host_path.display().to_string(),
                reason: e.to_string(),
            })?
            .len();
        if size == 0 {
            return Err(DumpCaptureError::EmptyDump {
                tool: MARIADB_TOOL,
                container: container_name.to_string(),
                container_path: EXEC_STDOUT.to_string(),
            });
        }
        Ok(size)
    }
    .await;
    if outcome.is_err() {
        v2_common::best_effort_remove(&host_path).await;
    }
    outcome
}

/// Extract the root password from the decrypted service config JSON.
fn root_password_from_config(config_json: &str) -> String {
    let params: Value = serde_json::from_str(config_json).unwrap_or_else(|_| json!({}));
    params
        .get("root_password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #149 invariant: the dump shell must not contain the password.
    /// Credentials travel via the exec env (`MYSQL_PWD`), so a password
    /// containing shell metacharacters can never break out of `sh -c`.
    #[test]
    fn dump_shell_contains_no_credentials() {
        assert!(!DUMP_SHELL.contains("MYSQL_PWD"));
        assert!(!DUMP_SHELL.contains("password"));
        // Connects as root via env-provided password, no password flag.
        assert!(DUMP_SHELL.contains("-uroot"));
        assert!(!DUMP_SHELL.contains("--password"));
        assert!(!DUMP_SHELL.contains("-p'"));
        assert!(!DUMP_SHELL.contains("-p\""));
    }

    #[test]
    fn root_password_parsed_from_config() {
        assert_eq!(
            root_password_from_config(r#"{"root_password":"s3cr3t"}"#),
            "s3cr3t"
        );
        assert_eq!(root_password_from_config("{}"), "");
        assert_eq!(root_password_from_config("not json"), "");
    }

    // ── Attempt scoping ──────────────────────────────────────────────────

    #[test]
    fn attempts_of_one_backup_never_share_a_host_file() {
        let parent = tempfile::tempdir().expect("parent");
        // The per-backup layout the engine used before: one shared directory,
        // one `{uuid}.sql.gz` per backup, opened with a truncating create.
        let legacy_dir = parent.path().join("temps-mariadb-backup");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        let legacy = legacy_dir.join("b-uuid.sql.gz");
        std::fs::write(&legacy, b"stale").expect("legacy leftover");

        let first =
            DumpAttempt::new_in(parent.path(), MARIADB_TOOL, ENGINE_KEY, "b-uuid").expect("first");
        let second =
            DumpAttempt::new_in(parent.path(), MARIADB_TOOL, ENGINE_KEY, "b-uuid").expect("second");
        let first_file = first.host_path(HOST_DUMP_FILE);
        let second_file = second.host_path(HOST_DUMP_FILE);
        assert_ne!(first_file, second_file);
        assert_ne!(first_file, legacy);
        assert!(!first_file.exists() && !second_file.exists());

        std::fs::write(&first_file, b"first").expect("first dump");
        std::fs::write(&second_file, b"second").expect("second dump");
        let second_dir = second.host_dir().to_path_buf();
        drop(second);

        assert!(!second_dir.exists(), "an attempt removes its own directory");
        assert_eq!(std::fs::read(&first_file).expect("first kept"), b"first");
        assert_eq!(std::fs::read(&legacy).expect("legacy kept"), b"stale");
    }

    // ── Error classification ─────────────────────────────────────────────

    #[test]
    fn exec_failures_keep_cancellation_and_retry_policy() {
        let transient = DumpCaptureError::exec(
            MARIADB_TOOL,
            "mariadb-orders",
            BackupError::Failed {
                reason: "create exec on mariadb-orders: No such container".into(),
            },
        );
        let message = transient.to_string();
        assert!(
            message.starts_with(
                "MariaDB dump could not be streamed out of container 'mariadb-orders' through docker exec"
            ),
            "{message}"
        );
        assert!(message.contains("No such container"));
        assert!(matches!(
            BackupError::from(transient),
            BackupError::Failed { .. }
        ));

        let permanent = DumpCaptureError::exec(
            MARIADB_TOOL,
            "mariadb-orders",
            BackupError::PermanentFailure {
                reason: "unsupported".into(),
            },
        );
        assert!(matches!(
            BackupError::from(permanent),
            BackupError::PermanentFailure { .. }
        ));

        let cancelled =
            DumpCaptureError::exec(MARIADB_TOOL, "mariadb-orders", BackupError::Cancelled);
        assert!(matches!(cancelled, DumpCaptureError::Cancelled { .. }));
        assert!(matches!(
            BackupError::from(cancelled),
            BackupError::Cancelled
        ));
    }

    #[test]
    fn export_and_upload_failures_are_labelled_by_where_they_happened() {
        let export = DumpCaptureError::Export {
            tool: MARIADB_TOOL,
            container: "mariadb-orders".into(),
            exit_code: 2,
            stderr: "mariadb-dump: Got error: 1045: Access denied".into(),
        };
        let message = export.to_string();
        assert!(message.starts_with("MariaDB export failed"), "{message}");
        assert!(message.contains("exit code 2") && message.contains("Access denied"));
        assert!(matches!(
            BackupError::from(export),
            BackupError::Failed { .. }
        ));

        let empty = DumpCaptureError::EmptyDump {
            tool: MARIADB_TOOL,
            container: "mariadb-orders".into(),
            container_path: EXEC_STDOUT.into(),
        };
        assert!(empty.to_string().contains("empty dump"));

        let upload = DumpCaptureError::upload(
            MARIADB_TOOL,
            "bucket",
            "mariadb/orders/dump.sql.gz",
            BackupError::PermanentFailure {
                reason: "AccessDenied".into(),
            },
        );
        assert!(upload
            .to_string()
            .starts_with("MariaDB backup was captured, but uploading"));
        assert!(matches!(
            BackupError::from(upload),
            BackupError::PermanentFailure { .. }
        ));
    }

    // ── Docker-backed tests ──────────────────────────────────────────────
    //
    // Run the real `DUMP_SHELL` through `docker exec` against a real MariaDB.
    // They skip (with a message) when no Docker daemon answers or the image
    // cannot be pulled.

    const TEST_IMAGE: &str = "mariadb:11.4";
    const TEST_ROOT_PASSWORD: &str = "test-root-pass";

    async fn docker_or_skip(test: &str) -> Option<bollard::Docker> {
        let docker = match bollard::Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(e) => {
                println!("Docker not available, skipping {test}: {e}");
                return None;
            }
        };
        if let Err(e) = docker.ping().await {
            println!("Docker daemon not reachable, skipping {test}: {e}");
            return None;
        }
        if let Err(e) =
            super::super::image_pull::ensure_image_pulled_v2(TEST_IMAGE, ENGINE_KEY).await
        {
            println!("Could not pull {TEST_IMAGE}, skipping {test}: {e}");
            return None;
        }
        Some(docker)
    }

    /// Run a command in a container; return its exit code and output.
    async fn exec_in(docker: &bollard::Docker, container: &str, cmd: &[&str]) -> (i64, String) {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use futures::StreamExt;
        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|part| part.to_string()).collect()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create exec");
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.start_exec(&exec.id, None).await.expect("start exec")
        {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&chunk.to_string());
            }
        }
        let code = docker
            .inspect_exec(&exec.id)
            .await
            .expect("inspect exec")
            .exit_code
            .unwrap_or(-1);
        (code, text)
    }

    /// A seeded MariaDB, force-removed by name on drop (also on panic).
    struct SeededMariadb {
        name: String,
    }

    impl Drop for SeededMariadb {
        fn drop(&mut self) {
            let name = std::mem::take(&mut self.name);
            let cleanup = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    if let Ok(docker) = bollard::Docker::connect_with_local_defaults() {
                        let _ = docker
                            .remove_container(
                                &name,
                                Some(bollard::query_parameters::RemoveContainerOptions {
                                    force: true,
                                    v: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                    }
                });
            });
            let _ = cleanup.join();
        }
    }

    async fn seeded_mariadb(docker: &bollard::Docker, run_id: &str) -> SeededMariadb {
        let name = format!("temps-test-mariadb-dump-{run_id}");
        let seeded = SeededMariadb { name: name.clone() };
        docker
            .create_container(
                Some(
                    bollard::query_parameters::CreateContainerOptionsBuilder::new()
                        .name(&name)
                        .build(),
                ),
                bollard::models::ContainerCreateBody {
                    image: Some(TEST_IMAGE.to_string()),
                    env: Some(vec![format!("MARIADB_ROOT_PASSWORD={TEST_ROOT_PASSWORD}")]),
                    ..Default::default()
                },
            )
            .await
            .expect("create test mariadb");
        docker
            .start_container(
                &name,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .expect("start test mariadb");

        // TCP readiness: the init-time server runs with networking off, so
        // this succeeds only once the real server is up.
        let password_flag = format!("-p{TEST_ROOT_PASSWORD}");
        let mut ready = false;
        for _ in 0..120 {
            let (code, _) = exec_in(
                docker,
                &name,
                &[
                    "mariadb",
                    "-h",
                    "127.0.0.1",
                    "-uroot",
                    &password_flag,
                    "-e",
                    "SELECT 1",
                ],
            )
            .await;
            if code == 0 {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert!(ready, "test mariadb {name} never became ready");
        let (code, output) = exec_in(
            docker,
            &name,
            &[
                "mariadb",
                "-h",
                "127.0.0.1",
                "-uroot",
                &password_flag,
                "-e",
                "CREATE DATABASE shop; CREATE TABLE shop.orders (id INT PRIMARY KEY, label TEXT); \
                 INSERT INTO shop.orders VALUES (1, 'seeded-order-row');",
            ],
        )
        .await;
        assert_eq!(code, 0, "seeding failed: {output}");
        seeded
    }

    fn gunzip_to_string(path: &std::path::Path) -> String {
        use std::io::Read;
        let mut sql = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).expect("open dump"))
            .read_to_string(&mut sql)
            .expect("gunzip dump");
        sql
    }

    fn root_env() -> Vec<String> {
        vec![
            format!("MYSQL_PWD={TEST_ROOT_PASSWORD}"),
            format!("MARIADB_PWD={TEST_ROOT_PASSWORD}"),
        ]
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_attempts_of_one_backup_each_keep_their_own_dump() {
        let Some(docker) =
            docker_or_skip("concurrent_attempts_of_one_backup_each_keep_their_own_dump").await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let mariadb = seeded_mariadb(&docker, &run_id).await;
        let backup_uuid = format!("concurrent-{run_id}");
        let cancel = tokio_util::sync::CancellationToken::new();
        let env = root_env();

        // What the previous binary left: the per-backup file a retry used to
        // truncate with `File::create`.
        let legacy_dir = std::env::temp_dir().join("temps-mariadb-backup");
        std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
        let legacy = legacy_dir.join(format!("{backup_uuid}.sql.gz"));
        std::fs::write(&legacy, b"stale").expect("legacy leftover");

        let first = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("first");
        let second = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("second");
        println!(
            "mariadb attempt host dirs: {} and {}",
            first.host_dir().display(),
            second.host_dir().display()
        );
        let (first_size, second_size) = tokio::join!(
            dump_into_attempt(&docker, &mariadb.name, DUMP_SHELL, &env, &first, &cancel),
            dump_into_attempt(&docker, &mariadb.name, DUMP_SHELL, &env, &second, &cancel),
        );
        let first_size = first_size.expect("first attempt dumps");
        let second_size = second_size.expect("second attempt dumps");

        for (attempt, size) in [(&first, first_size), (&second, second_size)] {
            let path = attempt.host_path(HOST_DUMP_FILE);
            assert_eq!(std::fs::metadata(&path).expect("dump").len(), size);
            let sql = gunzip_to_string(&path);
            assert!(sql.contains("CREATE DATABASE"), "not a dump of `shop`");
            assert!(sql.contains("seeded-order-row"), "seeded row missing");
        }

        let first_dir = first.host_dir().to_path_buf();
        drop(first);
        assert!(!first_dir.exists(), "first attempt dir not cleaned up");
        assert!(
            second.host_path(HOST_DUMP_FILE).exists(),
            "dropping one attempt must not touch another's dump"
        );
        drop(second);
        assert_eq!(std::fs::read(&legacy).expect("legacy kept"), b"stale");
        let _ = std::fs::remove_file(&legacy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_attempts_remove_only_their_own_file_and_are_classified() {
        let Some(docker) =
            docker_or_skip("failed_attempts_remove_only_their_own_file_and_are_classified").await
        else {
            return;
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let mariadb = seeded_mariadb(&docker, &run_id).await;
        let backup_uuid = format!("failed-{run_id}");
        let cancel = tokio_util::sync::CancellationToken::new();
        let env = root_env();

        // A finished earlier attempt that still holds its dump.
        let earlier = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("earlier");
        dump_into_attempt(&docker, &mariadb.name, DUMP_SHELL, &env, &earlier, &cancel)
            .await
            .expect("earlier attempt dumps");
        let earlier_dump = std::fs::read(earlier.host_path(HOST_DUMP_FILE)).expect("earlier dump");

        // The dump tool fails after writing part of its output.
        let failing = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("failing");
        let error = dump_into_attempt(
            &docker,
            &mariadb.name,
            "echo partial-output; echo 'mariadb-dump: Got error: 1045' >&2; exit 2",
            &env,
            &failing,
            &cancel,
        )
        .await
        .expect_err("tool fails");
        assert!(
            matches!(
                &error,
                DumpCaptureError::Export { exit_code: 2, stderr, container, .. }
                    if stderr.contains("Got error: 1045") && container == &mariadb.name
            ),
            "{error}"
        );
        assert!(
            !failing.host_path(HOST_DUMP_FILE).exists(),
            "the failed attempt's partial file must be removed"
        );

        // The tool exits 0 without output.
        let empty = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("empty");
        let error = dump_into_attempt(&docker, &mariadb.name, "true", &env, &empty, &cancel)
            .await
            .expect_err("no output");
        assert!(
            matches!(error, DumpCaptureError::EmptyDump { .. }),
            "{error}"
        );
        assert!(!empty.host_path(HOST_DUMP_FILE).exists());

        // The service container does not exist.
        let missing = DumpAttempt::new(MARIADB_TOOL, ENGINE_KEY, &backup_uuid).expect("missing");
        let missing_container = format!("temps-test-no-such-mariadb-{run_id}");
        let error = dump_into_attempt(
            &docker,
            &missing_container,
            DUMP_SHELL,
            &env,
            &missing,
            &cancel,
        )
        .await
        .expect_err("no container");
        assert!(
            matches!(&error, DumpCaptureError::Exec { container, permanent: false, .. } if container == &missing_container),
            "{error}"
        );
        assert!(matches!(
            BackupError::from(error),
            BackupError::Failed { .. }
        ));

        assert_eq!(
            std::fs::read(earlier.host_path(HOST_DUMP_FILE)).expect("earlier kept"),
            earlier_dump,
            "failed attempts must not touch an earlier attempt's dump"
        );
    }
}
