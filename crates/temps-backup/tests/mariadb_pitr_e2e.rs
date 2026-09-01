// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Full-chain MariaDB point-in-time-recovery (PITR) end-to-end integration test.
//!
//! This drives the **real** engine + provider code paths against **real**
//! containers - no mocks of the backup/restore mechanics:
//!
//!   1. Boot MinIO (S3) + create a bucket.
//!   2. Boot a `mariadb:lts` "source" container with binary logging on.
//!   3. Stand up a Postgres test DB with the real schema (`TestDatabase`),
//!      then insert an `external_services` row (config encrypted with the
//!      SAME `EncryptionService` the engine uses) + an `s3_sources` row
//!      (access key / secret key encrypted with that same service).
//!   4. Seed batch A -> run the REAL `MariadbPhysicalEngine` base backup.
//!   5. Insert batch B, capture timestamp T, insert batch C ->
//!      run the REAL `MariaDbService::archive_binlogs` archiver.
//!   6. Run the REAL `MariaDbService::restore_pitr` to time T into a new service.
//!   7. Assert A + B present and C absent in the restored container.
//!
//! All containers are reaped via RAII guards even on panic.
//!
//! ## Docker-access caveat
//! Boots happen via raw `bollard` against the local Docker socket. When the
//! socket is unreachable (the common local case where the user can't reach
//! Docker without sudo, or CI runners without Docker), every boot helper
//! returns `None` and the test prints a skip message and PASSES - it never
//! hard-fails. CI runs this with `--features docker-tests` on a runner that
//! has a real Docker daemon, which is the authoritative run.
//!
//! Gated behind the `docker-tests` feature (mirrors `temps-providers`).
#![cfg(feature = "docker-tests")]

use std::collections::HashMap;
use std::sync::{Arc, Once};
use std::time::Duration;

use aws_sdk_s3::config::Region;
use bollard::Docker;
use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
use sqlx::mysql::MySqlPoolOptions;
use temps_backup_core::engine_v2::{BackupContext, BackupEngine};
use temps_core::EncryptionService;
use temps_providers::externalsvc::{
    ExternalService, MariaDbService, RecoveryTarget, RestoreContext, S3Credentials, ServiceConfig,
    ServiceType,
};
use tokio_util::sync::CancellationToken;

// A fixed 64-hex-char master key (== 32 bytes) shared by the test and every
// EncryptionService instance, so encrypt-here / decrypt-in-engine round-trips.
const MASTER_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ROOT_PASSWORD: &str = "pitr-root-pw-1234"; // >= 8 chars, no quotes/backslashes
const MINIO_ACCESS_KEY: &str = "minioadmin";
const MINIO_SECRET_KEY: &str = "minioadmin";
const BUCKET: &str = "pitr-test-bucket";
const E2E_TIMEOUT: Duration = Duration::from_secs(40 * 60);

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "temps_providers::externalsvc::mariadb=info",
            ))
            .with_test_writer()
            .try_init();
    });
}

/// RAII guard that force-removes a container (and its volumes) on drop, even
/// on panic. Uses `block_in_place` so it works inside the multi-threaded
/// tokio test runtime.
struct ContainerGuard {
    docker: Docker,
    id: String,
    label: String,
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        let id = self.id.clone();
        let label = self.label.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        let _ = docker
                            .stop_container(
                                &id,
                                Some(bollard::query_parameters::StopContainerOptions {
                                    t: Some(3),
                                    signal: None,
                                }),
                            )
                            .await;
                        let _ = docker
                            .remove_container(
                                &id,
                                Some(bollard::query_parameters::RemoveContainerOptions {
                                    force: true,
                                    v: true,
                                    ..Default::default()
                                }),
                            )
                            .await;
                        eprintln!("Reaped container {label} ({id})");
                    });
                });
            }
        }));
    }
}

/// Connect to the local Docker daemon. Returns `None` (skip) when unreachable.
async fn connect_docker() -> Option<Docker> {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Docker unavailable (connect failed), skipping: {e}");
            return None;
        }
    };
    if let Err(e) = docker.ping().await {
        eprintln!("Docker socket unreachable (ping failed), skipping: {e}");
        return None;
    }
    Some(docker)
}

/// Pull an image (best-effort; ignores "already present" style results).
async fn pull_image(docker: &Docker, image: &str) -> anyhow::Result<()> {
    use futures::StreamExt;
    let (name, tag) = image.split_once(':').unwrap_or((image, "latest"));
    let mut stream = docker.create_image(
        Some(bollard::query_parameters::CreateImageOptions {
            from_image: Some(name.to_string()),
            tag: Some(tag.to_string()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(item) = stream.next().await {
        item.map_err(|e| anyhow::anyhow!("pull {image}: {e}"))?;
    }
    Ok(())
}

fn find_available_port(start: u16) -> Option<u16> {
    use std::net::TcpListener;
    (start..start + 200).find(|&p| TcpListener::bind(("127.0.0.1", p)).is_ok())
}

/// Boot a MinIO container, returning (host_port, guard). Skips (None) on
/// failure so the test can bail gracefully.
async fn boot_minio(docker: &Docker) -> Option<(u16, ContainerGuard)> {
    if pull_image(docker, "minio/minio:latest").await.is_err() {
        eprintln!("Could not pull MinIO image, skipping");
        return None;
    }
    let port = find_available_port(9100)?;
    let name = format!("temps-test-pitr-minio-{}", uuid::Uuid::new_v4());

    let config = bollard::models::ContainerCreateBody {
        image: Some("minio/minio:latest".to_string()),
        cmd: Some(vec!["server".to_string(), "/data".to_string()]),
        env: Some(vec![
            format!("MINIO_ROOT_USER={MINIO_ACCESS_KEY}"),
            format!("MINIO_ROOT_PASSWORD={MINIO_SECRET_KEY}"),
        ]),
        host_config: Some(bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "9000/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(port.to_string()),
                }]),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&name)
                    .build(),
            ),
            config,
        )
        .await
        .ok()?;
    let guard = ContainerGuard {
        docker: docker.clone(),
        id: created.id.clone(),
        label: "minio".to_string(),
    };
    docker
        .start_container(
            &created.id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .ok()?;

    // Give MinIO a moment to bind its port.
    tokio::time::sleep(Duration::from_secs(3)).await;
    Some((port, guard))
}

/// Build a host-side S3 client against the local MinIO. Returns None when the
/// AWS SDK panics constructing its TrustStore (some minimal CI hosts).
fn build_s3_client(port: u16) -> Option<aws_sdk_s3::Client> {
    let conf = aws_sdk_s3::Config::builder()
        .endpoint_url(format!("http://127.0.0.1:{port}"))
        .region(Region::new("us-east-1"))
        .behavior_version_latest()
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            MINIO_ACCESS_KEY,
            MINIO_SECRET_KEY,
            None,
            None,
            "minio",
        ))
        .force_path_style(true)
        .build();

    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        aws_sdk_s3::Client::from_conf(conf)
    })) {
        Ok(c) => Some(c),
        Err(_) => {
            eprintln!("AWS SDK panicked building S3 client (TrustStore), skipping");
            None
        }
    }
}

/// Boot a `mariadb:lts` source container with binlog enabled. Returns
/// (container_name, host_port, guard). The container name is `mariadb-<name>`
/// so it matches what the engine/provider derive from the service name.
async fn boot_mariadb_source(
    docker: &Docker,
    service_name: &str,
) -> Option<(String, u16, ContainerGuard)> {
    if pull_image(docker, "mariadb:lts").await.is_err() {
        eprintln!("Could not pull mariadb:lts image, skipping");
        return None;
    }
    let port = find_available_port(33060)?;
    let container_name = format!("mariadb-{service_name}");

    let config = bollard::models::ContainerCreateBody {
        image: Some("mariadb:lts".to_string()),
        cmd: Some(vec![
            "--log-bin=mysql-bin".to_string(),
            "--server-id=1".to_string(),
            "--binlog-format=ROW".to_string(),
        ]),
        env: Some(vec![
            format!("MARIADB_ROOT_PASSWORD={ROOT_PASSWORD}"),
            "TZ=UTC".to_string(),
        ]),
        host_config: Some(bollard::models::HostConfig {
            port_bindings: Some(HashMap::from([(
                "3306/tcp".to_string(),
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(port.to_string()),
                }]),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = docker
        .create_container(
            Some(
                bollard::query_parameters::CreateContainerOptionsBuilder::new()
                    .name(&container_name)
                    .build(),
            ),
            config,
        )
        .await
        .ok()?;
    let guard = ContainerGuard {
        docker: docker.clone(),
        id: created.id.clone(),
        label: container_name.clone(),
    };
    docker
        .start_container(
            &created.id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
        .ok()?;

    // Wait for MariaDB to accept connections on the mapped host port.
    let conn_str = format!("mysql://root:{ROOT_PASSWORD}@127.0.0.1:{port}/");
    for attempt in 0..40 {
        match MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(3))
            .connect(&conn_str)
            .await
        {
            Ok(pool) => {
                pool.close().await;
                return Some((container_name, port, guard));
            }
            Err(_) if attempt < 39 => tokio::time::sleep(Duration::from_millis(750)).await,
            Err(e) => {
                eprintln!("MariaDB source never became reachable: {e}");
                return None;
            }
        }
    }
    None
}

/// Open a sqlx MySQL pool against the given host port.
async fn mysql_pool(port: u16) -> anyhow::Result<sqlx::MySqlPool> {
    let conn = format!("mysql://root:{ROOT_PASSWORD}@127.0.0.1:{port}/");
    MySqlPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&conn)
        .await
        .map_err(|e| anyhow::anyhow!("connect mysql on {port}: {e}"))
}

/// The MariaDB ServiceConfig parameters JSON that both the engine and the
/// provider parse (`MariaDbInputConfig`). `container_name` is set so the
/// provider talks to our pre-created `mariadb-<name>` container.
fn mariadb_params(service_name: &str, host_port: u16) -> serde_json::Value {
    serde_json::json!({
        "host": "localhost",
        "port": host_port.to_string(),
        "database": "appdb",
        "username": "root",
        "password": ROOT_PASSWORD,
        "root_password": ROOT_PASSWORD,
        "docker_image": "mariadb:lts",
        "container_name": format!("mariadb-{service_name}"),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mariadb_pitr_full_chain_e2e() {
    tokio::time::timeout(E2E_TIMEOUT, mariadb_pitr_full_chain_e2e_inner())
        .await
        .expect("MariaDB PITR E2E timed out")
}

async fn mariadb_pitr_full_chain_e2e_inner() {
    init_tracing();

    // Docker / DB availability gate (graceful skip).
    let Some(docker) = connect_docker().await else {
        return;
    };

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Test database unavailable, skipping: {e}");
            return;
        }
    };
    let pool = test_db.connection_arc();

    let Some((minio_port, _minio_guard)) = boot_minio(&docker).await else {
        return;
    };
    let Some(s3_client) = build_s3_client(minio_port) else {
        return;
    };
    if let Err(e) = s3_client.create_bucket().bucket(BUCKET).send().await {
        eprintln!("Could not create MinIO bucket, skipping: {e}");
        return;
    }

    let service_name = format!("pitr{}", uuid::Uuid::new_v4().simple());
    let Some((container_name, mariadb_port, _mariadb_guard)) =
        boot_mariadb_source(&docker, &service_name).await
    else {
        return;
    };
    eprintln!("Booted MariaDB source container {container_name} on host port {mariadb_port}");

    // From here on, any assertion failure should still reap containers (RAII
    // guards on the stack handle that). We run the real flow.
    run_pitr_flow(
        &docker,
        &s3_client,
        minio_port,
        pool,
        &service_name,
        &container_name,
        mariadb_port,
    )
    .await
    .expect("PITR end-to-end flow");
}

#[allow(clippy::too_many_arguments)]
async fn run_pitr_flow(
    docker: &Docker,
    s3_client: &aws_sdk_s3::Client,
    minio_port: u16,
    pool_arc: Arc<temps_database::DbConnection>,
    service_name: &str,
    container_name: &str,
    mariadb_port: u16,
) -> anyhow::Result<()> {
    let pool: &temps_database::DbConnection = pool_arc.as_ref();
    eprintln!("Running PITR flow against source container {container_name}");
    let encryption = Arc::new(EncryptionService::new(MASTER_KEY_HEX)?);

    // Insert encrypted DB rows.
    // The engine decrypts `external_services.config` and the s3 creds with the
    // SAME EncryptionService, so we encrypt with it here.
    let config_plaintext = mariadb_params(service_name, mariadb_port).to_string();
    let config_encrypted = encryption.encrypt_string(&config_plaintext)?;

    let service_model = temps_entities::external_services::ActiveModel {
        name: Set(service_name.to_string()),
        service_type: Set("mariadb".to_string()),
        version: Set(None),
        status: Set("running".to_string()),
        config: Set(Some(config_encrypted)),
        topology: Set("standalone".to_string()),
        ..Default::default()
    }
    .insert(pool)
    .await?;
    let service_id = service_model.id;

    let s3_source_model = temps_entities::s3_sources::ActiveModel {
        name: Set("pitr-s3".to_string()),
        bucket_name: Set(BUCKET.to_string()),
        region: Set("us-east-1".to_string()),
        // Host-side clients (engine + archiver + restore) all reach MinIO on
        // localhost - MariaDB does ALL S3 IO host-side (download base/binlogs
        // to host, then upload into the container), so localhost is correct.
        endpoint: Set(Some(format!("http://127.0.0.1:{minio_port}"))),
        bucket_path: Set(String::new()),
        access_key_id: Set(encryption.encrypt_string(MINIO_ACCESS_KEY)?),
        secret_key: Set(encryption.encrypt_string(MINIO_SECRET_KEY)?),
        force_path_style: Set(Some(true)),
        is_default: Set(true),
        ..Default::default()
    }
    .insert(pool)
    .await?;
    let s3_source_id = s3_source_model.id;

    // The production executor creates the user-owned parent backup row before
    // invoking an engine. The engine uses its public UUID as the artifact
    // directory, so the E2E fixture must exercise that same contract.
    let user = temps_entities::users::ActiveModel {
        name: Set("pitr-test-user".to_string()),
        email: Set(format!(
            "pitr-{}@example.test",
            uuid::Uuid::new_v4().simple()
        )),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        ..Default::default()
    }
    .insert(pool)
    .await?;
    let parent_backup = temps_entities::backups::ActiveModel {
        name: Set("pitr-base".to_string()),
        backup_id: Set(uuid::Uuid::new_v4().to_string()),
        backup_type: Set("full".to_string()),
        state: Set("running".to_string()),
        started_at: Set(chrono::Utc::now()),
        s3_source_id: Set(s3_source_id),
        s3_location: Set(String::new()),
        metadata: Set("{}".to_string()),
        compression_type: Set("gzip".to_string()),
        created_by: Set(user.id),
        tags: Set("[]".to_string()),
        ..Default::default()
    }
    .insert(pool)
    .await?;

    // Seed data: create DB + table, insert batch A.
    let src = mysql_pool(mariadb_port).await?;
    sqlx::query("CREATE DATABASE IF NOT EXISTS appdb")
        .execute(&src)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS appdb.events (id INT PRIMARY KEY AUTO_INCREMENT, batch CHAR(1) NOT NULL, note VARCHAR(64))",
    )
    .execute(&src)
    .await?;
    for i in 0..5 {
        sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('A', ?)")
            .bind(format!("a{i}"))
            .execute(&src)
            .await?;
    }
    let count_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events WHERE batch='A'")
        .fetch_one(&src)
        .await?;
    assert_eq!(count_a, 5, "batch A seeded");

    // Run the REAL base-backup engine.
    let engine = temps_backup::engines::mariadb_physical::MariadbPhysicalEngine::new(
        temps_backup::engines::mariadb_physical::MariadbPhysicalDeps {
            db: Arc::clone(&pool_arc),
            encryption_service: Arc::clone(&encryption),
            docker: docker.clone(),
        },
    );
    let ctx = BackupContext {
        backup_id: parent_backup.id,
        engine_key: "mariadb_physical".to_string(),
        params: serde_json::json!({ "service_id": service_id, "s3_source_id": s3_source_id }),
        cancel: CancellationToken::new(),
        db: Arc::clone(&pool_arc),
    };
    let outcome = engine
        .run(&ctx)
        .await
        .map_err(|e| anyhow::anyhow!("base backup engine: {e}"))?;
    let mut completed_backup = parent_backup.into_active_model();
    completed_backup.state = Set("completed".to_string());
    completed_backup.finished_at = Set(Some(chrono::Utc::now()));
    completed_backup.size_bytes = Set(outcome.size_bytes);
    completed_backup.s3_location = Set(outcome.location.clone());
    let backup_model = completed_backup.update(pool).await?;
    eprintln!("Base backup landed at key: {}", outcome.location);
    assert!(
        outcome.location.ends_with("base.mbstream.gz"),
        "engine should produce a physical base, got {}",
        outcome.location
    );

    // Confirm the base object actually landed in MinIO.
    let head = s3_client
        .head_object()
        .bucket(BUCKET)
        .key(&outcome.location)
        .send()
        .await;
    assert!(head.is_ok(), "base object must exist in MinIO: {head:?}");

    // DIAGNOSTIC: verify the stored base object is valid gzip.
    {
        let obj = s3_client
            .get_object()
            .bucket(BUCKET)
            .key(&outcome.location)
            .send()
            .await?;
        let bytes = obj.body.collect().await?.into_bytes();
        eprintln!(
            "DIAG base object: {} bytes, first4={:02x?}",
            bytes.len(),
            &bytes[..bytes.len().min(4)]
        );
    }

    // Insert batch B, capture T, insert batch C.
    for i in 0..4 {
        sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('B', ?)")
            .bind(format!("b{i}"))
            .execute(&src)
            .await?;
    }
    // Capture T strictly between B and C. MariaDB binlog event timestamps have
    // 1-second resolution and `mariadb-binlog --stop-datetime` truncates T to
    // whole seconds, so we need a comfortable gap on each side of T: ~4s after
    // B and ~4s before C guarantees B's events land in a strictly-earlier
    // whole second than T, and C's in a strictly-later one.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let t: chrono::DateTime<chrono::Utc> = chrono::Utc::now();
    tokio::time::sleep(Duration::from_secs(4)).await;
    for i in 0..3 {
        sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('C', ?)")
            .bind(format!("c{i}"))
            .execute(&src)
            .await?;
    }
    src.close().await;

    // Run the REAL binlog archiver.
    // The archiver FLUSHes binary logs (closing the active segment) and ships
    // the now-closed segments to MinIO. Run it twice so the segment that
    // contains B and C is closed by a later FLUSH and then shipped.
    let mariadb_svc = MariaDbService::new(service_name.to_string(), Arc::new(docker.clone()));
    let mariadb_config = parse_mariadb_config(service_name, mariadb_port);

    // Decrypt the s3 source row the way the orchestrator does before calling
    // the provider: the archiver reads `s3_source.bucket_name`/`bucket_path`
    // only (creds come from the passed s3_client), so the model can stay as-is.
    let mut shipped_total = 0usize;
    for round in 0..2 {
        let n = mariadb_svc
            .archive_binlogs(s3_client, &s3_source_model, &mariadb_config)
            .await
            .map_err(|e| anyhow::anyhow!("archive_binlogs round {round}: {e}"))?;
        shipped_total += n;
        eprintln!("archive_binlogs round {round} shipped {n} segment(s)");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!("Total binlog segments shipped: {shipped_total}");

    // DIAGNOSTIC: dump the base metadata.json and the binlog manifest from S3
    // so we can see the recorded binlog coordinates and which segments shipped.
    eprintln!("DIAG recovery target T (UTC) = {t}");
    {
        let meta_key = {
            let (dir, _) = outcome.location.rsplit_once('/').unwrap();
            format!("{dir}/metadata.json")
        };
        if let Ok(o) = s3_client
            .get_object()
            .bucket(BUCKET)
            .key(&meta_key)
            .send()
            .await
        {
            let b = o.body.collect().await?.into_bytes();
            eprintln!("DIAG base metadata.json = {}", String::from_utf8_lossy(&b));
        }
        if let Ok(o) = s3_client
            .get_object()
            .bucket(BUCKET)
            .key(format!(
                "external_services/mariadb/{service_name}/binlog/manifest.json"
            ))
            .send()
            .await
        {
            let b = o.body.collect().await?.into_bytes();
            eprintln!("DIAG binlog manifest = {}", String::from_utf8_lossy(&b));
        }
    }

    // Build a decrypted RestoreContext (as the orchestrator hands it).
    let decrypted_s3_source = {
        let mut m = s3_source_model.clone();
        m.access_key_id = encryption.decrypt_string(&s3_source_model.access_key_id)?;
        m.secret_key = encryption.decrypt_string(&s3_source_model.secret_key)?;
        m
    };
    let s3_credentials = S3Credentials {
        access_key_id: MINIO_ACCESS_KEY.to_string(),
        secret_key: MINIO_SECRET_KEY.to_string(),
        region: "us-east-1".to_string(),
        endpoint: decrypted_s3_source.endpoint.clone(),
        bucket_name: BUCKET.to_string(),
        bucket_path: String::new(),
        force_path_style: true,
    };

    let source_config = ServiceConfig {
        name: service_name.to_string(),
        service_type: ServiceType::Mariadb,
        version: None,
        parameters: mariadb_params(service_name, mariadb_port),
    };

    let restored_name = format!("{service_name}-restored");
    let restore_ctx = RestoreContext {
        s3_client,
        s3_credentials: &s3_credentials,
        s3_source: &decrypted_s3_source,
        backup: &backup_model,
        backup_location: &outcome.location,
        source_service: &service_model,
        source_config,
        pool,
    };

    // Run the REAL restore (PITR to time T, into a new service).
    let result = mariadb_svc
        .restore_pitr(
            restore_ctx,
            RecoveryTarget::Time { time: t },
            true,
            Some(restored_name.clone()),
        )
        .await
        .map_err(|e| anyhow::anyhow!("restore_pitr: {e}"))?;
    let result = result.expect("restore_to_new_service result");
    eprintln!("Restore produced new service: {}", result.connection_info);

    // Register the restored container for cleanup.
    let restored_container = format!("mariadb-{restored_name}");
    let _restored_guard = ContainerGuard {
        docker: docker.clone(),
        id: restored_container.clone(),
        label: restored_container.clone(),
    };
    // The restore helper container is removed by the provider, but its data
    // volume (`mariadb_data_<restored_name>`) is left; remove it best-effort.
    let restored_volume = format!("mariadb_data_{restored_name}");

    // Verify: A + B present, C absent in the restored container.
    let restored_port: u16 = result
        .parameters
        .get("port")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "restored service has no port param: {:?}",
                result.parameters
            )
        })?;
    eprintln!("Restored MariaDB on host port {restored_port}");

    // Give the restored server a moment after health to settle.
    let restored = {
        let conn = format!("mysql://root:{ROOT_PASSWORD}@127.0.0.1:{restored_port}/");
        let mut pool = None;
        for attempt in 0..30 {
            match MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(3))
                .connect(&conn)
                .await
            {
                Ok(p) => {
                    pool = Some(p);
                    break;
                }
                Err(_) if attempt < 29 => tokio::time::sleep(Duration::from_millis(750)).await,
                Err(e) => return Err(anyhow::anyhow!("connect restored mariadb: {e}")),
            }
        }
        pool.ok_or_else(|| anyhow::anyhow!("restored mariadb never reachable"))?
    };

    let a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events WHERE batch='A'")
        .fetch_one(&restored)
        .await?;
    let b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events WHERE batch='B'")
        .fetch_one(&restored)
        .await?;
    let c: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events WHERE batch='C'")
        .fetch_one(&restored)
        .await?;
    restored.close().await;

    eprintln!("Restored row counts - A={a} B={b} C={c} (expected A=5 B=4 C=0)");
    assert_eq!(a, 5, "batch A (in base) must be present after PITR");
    assert_eq!(b, 4, "batch B (before T) must be replayed");
    assert_eq!(
        c, 0,
        "batch C (after T) must be excluded by PITR stop-datetime"
    );

    // Best-effort: remove the restored data volume so it doesn't leak.
    let _ = docker
        .remove_volume(
            &restored_volume,
            Some(bollard::query_parameters::RemoveVolumeOptions { force: true }),
        )
        .await;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared setup for the additional end-to-end flows below.
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the extra E2E flows need: a live Docker daemon, a migrated
/// Postgres test DB, a MinIO with `BUCKET` created, and a running MariaDB
/// source container with binary logging on.
///
/// The `_*_guard` fields are RAII container reapers — they must be held for the
/// lifetime of the test, and `_test_db` must outlive `pool` (dropping the
/// `TestDatabase` tears down its schema).
struct E2eEnv {
    docker: Docker,
    _test_db: temps_database::test_utils::TestDatabase,
    pool: Arc<temps_database::DbConnection>,
    minio_port: u16,
    _minio_guard: ContainerGuard,
    s3_client: aws_sdk_s3::Client,
    service_name: String,
    container_name: String,
    mariadb_port: u16,
    _mariadb_guard: ContainerGuard,
}

/// Boot the whole fixture. Returns `None` (graceful skip) whenever any piece of
/// infrastructure is unavailable — never panics on missing infrastructure, only
/// on real assertion failures once a flow is running.
async fn setup_e2e_env(name_prefix: &str) -> Option<E2eEnv> {
    init_tracing();

    let docker = connect_docker().await?;

    let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Test database unavailable, skipping: {e}");
            return None;
        }
    };
    let pool = test_db.connection_arc();

    let (minio_port, minio_guard) = boot_minio(&docker).await?;
    let s3_client = build_s3_client(minio_port)?;
    if let Err(e) = s3_client.create_bucket().bucket(BUCKET).send().await {
        eprintln!("Could not create MinIO bucket, skipping: {e}");
        return None;
    }

    let service_name = format!("{name_prefix}{}", uuid::Uuid::new_v4().simple());
    let (container_name, mariadb_port, mariadb_guard) =
        boot_mariadb_source(&docker, &service_name).await?;
    eprintln!("Booted MariaDB source container {container_name} on host port {mariadb_port}");

    Some(E2eEnv {
        docker,
        _test_db: test_db,
        pool,
        minio_port,
        _minio_guard: minio_guard,
        s3_client,
        service_name,
        container_name,
        mariadb_port,
        _mariadb_guard: mariadb_guard,
    })
}

/// Insert the encrypted `external_services` + `s3_sources` + `users` rows the
/// engines read, mirroring what the production executor persists before it
/// invokes an engine. Returns `(service_model, s3_source_model, user_id)`.
async fn seed_service_rows(
    pool: &temps_database::DbConnection,
    encryption: &EncryptionService,
    service_name: &str,
    mariadb_port: u16,
    minio_port: u16,
) -> anyhow::Result<(
    temps_entities::external_services::Model,
    temps_entities::s3_sources::Model,
    i32,
)> {
    let config_plaintext = mariadb_params(service_name, mariadb_port).to_string();
    let config_encrypted = encryption.encrypt_string(&config_plaintext)?;

    let service_model = temps_entities::external_services::ActiveModel {
        name: Set(service_name.to_string()),
        service_type: Set("mariadb".to_string()),
        version: Set(None),
        status: Set("running".to_string()),
        config: Set(Some(config_encrypted)),
        topology: Set("standalone".to_string()),
        ..Default::default()
    }
    .insert(pool)
    .await?;

    let s3_source_model = temps_entities::s3_sources::ActiveModel {
        name: Set("pitr-s3".to_string()),
        bucket_name: Set(BUCKET.to_string()),
        region: Set("us-east-1".to_string()),
        endpoint: Set(Some(format!("http://127.0.0.1:{minio_port}"))),
        bucket_path: Set(String::new()),
        access_key_id: Set(encryption.encrypt_string(MINIO_ACCESS_KEY)?),
        secret_key: Set(encryption.encrypt_string(MINIO_SECRET_KEY)?),
        force_path_style: Set(Some(true)),
        is_default: Set(true),
        ..Default::default()
    }
    .insert(pool)
    .await?;

    let user = temps_entities::users::ActiveModel {
        name: Set("pitr-test-user".to_string()),
        email: Set(format!(
            "pitr-{}@example.test",
            uuid::Uuid::new_v4().simple()
        )),
        email_verified: Set(true),
        mfa_enabled: Set(false),
        ..Default::default()
    }
    .insert(pool)
    .await?;

    Ok((service_model, s3_source_model, user.id))
}

/// Insert the parent `backups` row an engine expects (its public UUID becomes
/// the artifact directory), run `engine`, then mark the row completed with the
/// produced location — the same contract `run_pitr_flow` exercises.
async fn run_engine_backup<E: BackupEngine>(
    pool_arc: &Arc<temps_database::DbConnection>,
    engine: &E,
    engine_key: &str,
    backup_name: &str,
    service_id: i32,
    s3_source_id: i32,
    user_id: i32,
) -> anyhow::Result<(String, temps_entities::backups::Model)> {
    let pool: &temps_database::DbConnection = pool_arc.as_ref();
    let parent_backup = temps_entities::backups::ActiveModel {
        name: Set(backup_name.to_string()),
        backup_id: Set(uuid::Uuid::new_v4().to_string()),
        backup_type: Set("full".to_string()),
        state: Set("running".to_string()),
        started_at: Set(chrono::Utc::now()),
        s3_source_id: Set(s3_source_id),
        s3_location: Set(String::new()),
        metadata: Set("{}".to_string()),
        compression_type: Set("gzip".to_string()),
        created_by: Set(user_id),
        tags: Set("[]".to_string()),
        ..Default::default()
    }
    .insert(pool)
    .await?;

    let ctx = BackupContext {
        backup_id: parent_backup.id,
        engine_key: engine_key.to_string(),
        params: serde_json::json!({ "service_id": service_id, "s3_source_id": s3_source_id }),
        cancel: CancellationToken::new(),
        db: Arc::clone(pool_arc),
    };
    let outcome = engine
        .run(&ctx)
        .await
        .map_err(|e| anyhow::anyhow!("{engine_key} engine ({backup_name}): {e}"))?;

    let mut completed = parent_backup.into_active_model();
    completed.state = Set("completed".to_string());
    completed.finished_at = Set(Some(chrono::Utc::now()));
    completed.size_bytes = Set(outcome.size_bytes);
    completed.s3_location = Set(outcome.location.clone());
    let backup_model = completed.update(pool).await?;

    eprintln!(
        "Backup '{backup_name}' ({engine_key}) landed at key: {}",
        outcome.location
    );
    Ok((outcome.location, backup_model))
}

/// S3 key of one archived binlog segment (empty bucket_path). Mirrors the
/// provider's `MariaDbService::binlog_object_key`, which is `pub(crate)`.
fn binlog_segment_key(service_name: &str, file: &str) -> String {
    format!("external_services/mariadb/{service_name}/binlog/{file}.gz")
}

/// S3 key of the binlog manifest (empty bucket_path).
fn binlog_manifest_key(service_name: &str) -> String {
    format!("external_services/mariadb/{service_name}/binlog/manifest.json")
}

/// Read and parse the binlog manifest straight out of S3.
async fn fetch_binlog_manifest(
    s3_client: &aws_sdk_s3::Client,
    service_name: &str,
) -> anyhow::Result<temps_providers::externalsvc::mariadb::BinlogManifest> {
    let obj = s3_client
        .get_object()
        .bucket(BUCKET)
        .key(binlog_manifest_key(service_name))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("get binlog manifest: {e}"))?;
    let bytes = obj.body.collect().await?.into_bytes();
    eprintln!("DIAG binlog manifest = {}", String::from_utf8_lossy(&bytes));
    Ok(serde_json::from_slice(&bytes)?)
}

/// Mirror of the provider's `pub(crate)` `MariaDbService::binlog_is_strictly_older`
/// ordering rule: same basename, same zero-padded suffix width, lexicographically
/// smaller. Anything it cannot order with certainty answers `false` (= keep).
///
/// Deliberately re-stated here rather than approximated with a plain string
/// compare — the assertion is only meaningful if it predicts what the
/// production predicate does.
fn expect_strictly_older(candidate: &str, anchor: &str) -> bool {
    fn split(file: &str) -> Option<(&str, &str)> {
        let (base, suffix) = file.rsplit_once('.')?;
        if base.is_empty() || suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some((base, suffix))
    }
    match (split(candidate), split(anchor)) {
        (Some((cb, cs)), Some((ab, asuf))) => cb == ab && cs.len() == asuf.len() && cs < asuf,
        _ => false,
    }
}

/// Whether an object exists in the test bucket.
async fn object_exists(s3_client: &aws_sdk_s3::Client, key: &str) -> bool {
    s3_client
        .head_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .is_ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// E2E #2 — binary-log retention pruning.
// ─────────────────────────────────────────────────────────────────────────────

/// Drives the REAL binlog retention pruner (`MariaDbService::prune_stale_binlogs`)
/// against real archived segments in real object storage.
///
/// `prune_stale_binlogs` DELETES recovery data, so its safety invariants are
/// only meaningfully proven against real S3 objects and a real manifest:
///
///  * pruning against the OLDEST retained base's anchor never touches that
///    base's own segment or anything after it (it may drop segments that
///    predate every retained base — those are genuinely unreachable);
///  * pruning against a LATER base's anchor (the realistic case: the first base
///    has aged out, so base #2 is now the oldest retained) deletes exactly the
///    segments strictly older than that anchor, and nothing else;
///  * the deleted objects are really gone and the retained ones really remain;
///  * the rewritten manifest drops only the deleted entries and NEVER rewinds
///    `last_shipped_file` (rewinding it would re-ship every segment);
///  * a repeat prune with the same anchor is a no-op, not a double-delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mariadb_pitr_full_chain_e2e_binlog_retention_prune() {
    tokio::time::timeout(E2E_TIMEOUT, mariadb_binlog_retention_prune_inner())
        .await
        .expect("MariaDB binlog retention E2E timed out")
}

async fn mariadb_binlog_retention_prune_inner() {
    let Some(env) = setup_e2e_env("prune").await else {
        return;
    };
    run_binlog_retention_flow(&env)
        .await
        .expect("binlog retention end-to-end flow");
}

async fn run_binlog_retention_flow(env: &E2eEnv) -> anyhow::Result<()> {
    let pool: &temps_database::DbConnection = env.pool.as_ref();
    let service_name = env.service_name.as_str();
    eprintln!(
        "Running binlog-retention flow against source container {}",
        env.container_name
    );
    let encryption = Arc::new(EncryptionService::new(MASTER_KEY_HEX)?);

    let (service_model, s3_source_model, user_id) = seed_service_rows(
        pool,
        &encryption,
        service_name,
        env.mariadb_port,
        env.minio_port,
    )
    .await?;

    // Seed a user database so the physical base has something to copy.
    let src = mysql_pool(env.mariadb_port).await?;
    sqlx::query("CREATE DATABASE IF NOT EXISTS appdb")
        .execute(&src)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS appdb.events (id INT PRIMARY KEY AUTO_INCREMENT, batch CHAR(1) NOT NULL, note VARCHAR(64))",
    )
    .execute(&src)
    .await?;
    for i in 0..5 {
        sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('A', ?)")
            .bind(format!("a{i}"))
            .execute(&src)
            .await?;
    }

    let engine = temps_backup::engines::mariadb_physical::MariadbPhysicalEngine::new(
        temps_backup::engines::mariadb_physical::MariadbPhysicalDeps {
            db: Arc::clone(&env.pool),
            encryption_service: Arc::clone(&encryption),
            docker: env.docker.clone(),
        },
    );

    // ── Base #1: the first retained base backup ─────────────────────────────
    let (base1_location, _base1_backup) = run_engine_backup(
        &env.pool,
        &engine,
        "mariadb_physical",
        "retention-base-1",
        service_model.id,
        s3_source_model.id,
        user_id,
    )
    .await?;
    assert!(
        base1_location.ends_with("base.mbstream.gz"),
        "base #1 must be a physical base, got {base1_location}"
    );

    let mariadb_svc = MariaDbService::new(service_name.to_string(), Arc::new(env.docker.clone()));
    let mariadb_config = parse_mariadb_config(service_name, env.mariadb_port);

    // ── Ship several segments: each round FLUSHes (rotating the active
    //    segment closed) and uploads everything newly closed. Real writes in
    //    between so the segments carry real events. ────────────────────────
    let mut shipped_total = 0usize;
    for round in 0..3 {
        for i in 0..3 {
            sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('B', ?)")
                .bind(format!("pre{round}-{i}"))
                .execute(&src)
                .await?;
        }
        let n = mariadb_svc
            .archive_binlogs(&env.s3_client, &s3_source_model, &mariadb_config)
            .await
            .map_err(|e| anyhow::anyhow!("archive_binlogs pre-round {round}: {e}"))?;
        shipped_total += n;
        eprintln!("archive_binlogs pre-round {round} shipped {n} segment(s)");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── Base #2, later in the timeline: its anchor is a LATER segment ───────
    let (base2_location, _base2_backup) = run_engine_backup(
        &env.pool,
        &engine,
        "mariadb_physical",
        "retention-base-2",
        service_model.id,
        s3_source_model.id,
        user_id,
    )
    .await?;
    assert!(
        base2_location.ends_with("base.mbstream.gz"),
        "base #2 must be a physical base, got {base2_location}"
    );

    // ── Ship more segments AFTER base #2 so the prune has both a "delete"
    //    side and a "keep" side to prove. ─────────────────────────────────
    for round in 0..2 {
        for i in 0..3 {
            sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('C', ?)")
                .bind(format!("post{round}-{i}"))
                .execute(&src)
                .await?;
        }
        let n = mariadb_svc
            .archive_binlogs(&env.s3_client, &s3_source_model, &mariadb_config)
            .await
            .map_err(|e| anyhow::anyhow!("archive_binlogs post-round {round}: {e}"))?;
        shipped_total += n;
        eprintln!("archive_binlogs post-round {round} shipped {n} segment(s)");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    src.close().await;
    eprintln!("Total binlog segments shipped: {shipped_total}");

    // ── Anchors ─────────────────────────────────────────────────────────────
    let anchor1 = mariadb_svc
        .base_binlog_anchor(&env.s3_client, BUCKET, &base1_location)
        .await
        .map_err(|e| anyhow::anyhow!("base_binlog_anchor(base #1): {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("base #1 must expose a PITR binlog anchor; got None (pitr disabled?)")
        })?;
    let anchor2 = mariadb_svc
        .base_binlog_anchor(&env.s3_client, BUCKET, &base2_location)
        .await
        .map_err(|e| anyhow::anyhow!("base_binlog_anchor(base #2): {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("base #2 must expose a PITR binlog anchor; got None (pitr disabled?)")
        })?;
    eprintln!("DIAG anchors: base#1={anchor1} base#2={anchor2}");
    assert!(
        expect_strictly_older(&anchor1, &anchor2),
        "the FLUSH rotations between the two bases must advance the anchor \
         (base#1={anchor1}, base#2={anchor2}); without that this test proves nothing"
    );

    let before = fetch_binlog_manifest(&env.s3_client, service_name).await?;
    assert!(
        before.shipped_files.len() >= 3,
        "need at least 3 shipped segments to prove partial pruning, got {:?}",
        before.shipped_files
    );
    let last_shipped_before = before.last_shipped_file.clone();
    assert!(
        last_shipped_before.is_some(),
        "manifest must record a high-water mark after shipping"
    );

    // ── Invariant 1: pruning against the OLDEST retained base's anchor keeps
    //    that base's OWN segment and everything after it, and drops only what
    //    predates it.
    //
    //    Note: this is NOT necessarily a no-op. `mariadb-backup` runs a FLUSH
    //    of its own, so base #1's anchor is typically the segment *after* the
    //    one the server started on — that earlier segment predates every
    //    retained base and is genuinely unreachable, so deleting it is correct.
    //    The invariant that matters is the boundary, not the count. ─────────
    let expected_stale1: Vec<String> = before
        .shipped_files
        .iter()
        .filter(|f| expect_strictly_older(f, &anchor1))
        .cloned()
        .collect();
    let expected_kept1: Vec<String> = before
        .shipped_files
        .iter()
        .filter(|f| !expect_strictly_older(f, &anchor1))
        .cloned()
        .collect();
    eprintln!("DIAG prune anchor={anchor1} stale={expected_stale1:?} kept={expected_kept1:?}");
    assert!(
        expected_kept1.contains(&anchor1),
        "base #1's own anchor segment ({anchor1}) must never be prunable against its own \
         anchor — PITR replay starts there; manifest was {:?}",
        before.shipped_files
    );

    let deleted1 = mariadb_svc
        .prune_stale_binlogs(&env.s3_client, &s3_source_model, &anchor1)
        .await
        .map_err(|e| anyhow::anyhow!("prune_stale_binlogs(anchor of oldest base): {e}"))?;
    assert_eq!(
        deleted1,
        expected_stale1.len(),
        "pruning against the oldest retained base's anchor ({anchor1}) must delete exactly \
         the segments that predate it (expected {expected_stale1:?})"
    );
    for file in &expected_stale1 {
        assert!(
            !object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "segment {file} predates the oldest retained base and must be gone"
        );
    }
    for file in &expected_kept1 {
        assert!(
            object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "segment {file} is still reachable from base #1 and must survive"
        );
    }

    let after1 = fetch_binlog_manifest(&env.s3_client, service_name).await?;
    assert_eq!(
        after1.shipped_files, expected_kept1,
        "manifest must list exactly the segments still reachable from base #1"
    );
    assert_eq!(
        after1.last_shipped_file, last_shipped_before,
        "prune must NEVER rewind last_shipped_file — rewinding it re-ships every segment"
    );

    // ── Invariant 2: once base #1 ages out, base #2 becomes the oldest
    //    retained base and its (later) anchor prunes strictly more —
    //    but still nothing at-or-after itself. ──────────────────────────────
    let expected_stale: Vec<String> = after1
        .shipped_files
        .iter()
        .filter(|f| expect_strictly_older(f, &anchor2))
        .cloned()
        .collect();
    let expected_kept: Vec<String> = after1
        .shipped_files
        .iter()
        .filter(|f| !expect_strictly_older(f, &anchor2))
        .cloned()
        .collect();
    eprintln!("DIAG prune anchor={anchor2} stale={expected_stale:?} kept={expected_kept:?}");
    assert!(
        !expected_stale.is_empty(),
        "expected at least one prunable segment older than {anchor2}, manifest was {:?}",
        after1.shipped_files
    );
    assert!(
        !expected_kept.is_empty(),
        "expected at least one retained segment at-or-after {anchor2}, manifest was {:?}",
        after1.shipped_files
    );
    assert!(
        expected_kept.contains(&anchor2),
        "base #2's own anchor segment ({anchor2}) must survive a prune against its own anchor"
    );

    let deleted = mariadb_svc
        .prune_stale_binlogs(&env.s3_client, &s3_source_model, &anchor2)
        .await
        .map_err(|e| anyhow::anyhow!("prune_stale_binlogs(anchor of new oldest base): {e}"))?;
    assert_eq!(
        deleted,
        expected_stale.len(),
        "prune must delete exactly the segments strictly older than {anchor2} \
         (expected {expected_stale:?})"
    );

    // Objects really gone / really kept.
    for file in &expected_stale {
        assert!(
            !object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "pruned segment {file} must no longer exist in S3"
        );
    }
    for file in &expected_kept {
        assert!(
            object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "retained segment {file} must still exist in S3"
        );
    }

    // Manifest rewritten to match, high-water mark untouched.
    let after = fetch_binlog_manifest(&env.s3_client, service_name).await?;
    assert_eq!(
        after.shipped_files, expected_kept,
        "manifest must list exactly the retained segments, in ship order"
    );
    assert_eq!(
        after.last_shipped_file, last_shipped_before,
        "prune must NEVER rewind last_shipped_file — rewinding it re-ships every segment"
    );

    // ── Invariant 3: repeat prune with the same anchor is a no-op ───────────
    let deleted_again = mariadb_svc
        .prune_stale_binlogs(&env.s3_client, &s3_source_model, &anchor2)
        .await
        .map_err(|e| anyhow::anyhow!("prune_stale_binlogs(repeat): {e}"))?;
    assert_eq!(
        deleted_again, 0,
        "a repeat prune against the same anchor must be idempotent"
    );
    let after_repeat = fetch_binlog_manifest(&env.s3_client, service_name).await?;
    assert_eq!(
        after_repeat, after,
        "an idempotent prune must leave the manifest byte-identical"
    );
    for file in &expected_kept {
        assert!(
            object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "retained segment {file} must survive a repeat prune"
        );
    }

    // ── Invariant 4: fail closed on an unorderable/unsafe anchor ────────────
    let err = mariadb_svc
        .prune_stale_binlogs(&env.s3_client, &s3_source_model, "../../etc/passwd")
        .await
        .expect_err("an unsafe anchor must be rejected, not acted on");
    eprintln!("DIAG unsafe-anchor rejection: {err}");
    for file in &expected_kept {
        assert!(
            object_exists(&env.s3_client, &binlog_segment_key(service_name, file)).await,
            "retained segment {file} must survive a rejected prune"
        );
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// E2E #3 — logical (`mariadb_dump`) backup + restore-to-new-service.
// ─────────────────────────────────────────────────────────────────────────────

/// Root/app credentials for the RESTORE TARGET. They are deliberately different
/// from the origin's (`ROOT_PASSWORD`) — that is exactly the state
/// `credential_propagation_gates` produces for a `mariadb_dump` restore, which
/// returns `(false, false)` and therefore does NOT merge the origin's
/// credentials into the config handed to the provider.
const RESTORED_ROOT_PASSWORD: &str = "target-root-pw-9876";
const RESTORED_APP_USER: &str = "appuser";
const RESTORED_APP_PASSWORD: &str = "target-app-pw-9876";

/// `ServiceConfig.parameters` for the restore target. Mirrors `mariadb_params`
/// but with the target's OWN credentials and a non-root app user (the MariaDB
/// entrypoint refuses `MARIADB_USER=root`).
fn restored_mariadb_params(service_name: &str) -> serde_json::Value {
    serde_json::json!({
        "host": "localhost",
        "database": "appdb",
        "username": RESTORED_APP_USER,
        "password": RESTORED_APP_PASSWORD,
        "root_password": RESTORED_ROOT_PASSWORD,
        "docker_image": "mariadb:lts",
        "container_name": format!("mariadb-{service_name}"),
    })
}

/// Full logical-dump round trip: REAL `MariadbDumpEngine` -> real `dump.sql.gz`
/// in real object storage -> REAL `restore_to_new_service` into a real new
/// container.
///
/// Two things nothing else covers end-to-end:
///
///  1. The dump engine's output is actually restorable — no e2e test exercised
///     `mariadb_dump` at all before this one.
///  2. The M-2 credential fix. `credential_propagation_gates` returns
///     `(false, false)` for `mariadb_dump` because the dump excludes the
///     `mysql` schema, so the restored server must keep the TARGET's own
///     credentials. Only a unit test asserted the boolean today; this proves
///     the restored container really authenticates with the target's password
///     and really REJECTS the origin's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mariadb_pitr_full_chain_e2e_logical_dump_restore_keeps_target_credentials() {
    tokio::time::timeout(E2E_TIMEOUT, mariadb_logical_dump_restore_inner())
        .await
        .expect("MariaDB logical dump restore E2E timed out")
}

async fn mariadb_logical_dump_restore_inner() {
    let Some(env) = setup_e2e_env("dump").await else {
        return;
    };
    run_logical_dump_restore_flow(&env)
        .await
        .expect("logical dump restore end-to-end flow");
}

async fn run_logical_dump_restore_flow(env: &E2eEnv) -> anyhow::Result<()> {
    let pool: &temps_database::DbConnection = env.pool.as_ref();
    let service_name = env.service_name.as_str();
    eprintln!(
        "Running logical-dump flow against source container {}",
        env.container_name
    );
    let encryption = Arc::new(EncryptionService::new(MASTER_KEY_HEX)?);

    let (service_model, s3_source_model, user_id) = seed_service_rows(
        pool,
        &encryption,
        service_name,
        env.mariadb_port,
        env.minio_port,
    )
    .await?;

    // ── Seed real user data on the origin ───────────────────────────────────
    let src = mysql_pool(env.mariadb_port).await?;
    sqlx::query("CREATE DATABASE IF NOT EXISTS appdb")
        .execute(&src)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS appdb.events (id INT PRIMARY KEY AUTO_INCREMENT, batch CHAR(1) NOT NULL, note VARCHAR(64))",
    )
    .execute(&src)
    .await?;
    for i in 0..7 {
        sqlx::query("INSERT INTO appdb.events (batch, note) VALUES ('A', ?)")
            .bind(format!("a{i}"))
            .execute(&src)
            .await?;
    }
    let seeded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events")
        .fetch_one(&src)
        .await?;
    assert_eq!(seeded, 7, "origin rows seeded");
    src.close().await;

    // ── REAL logical dump engine ────────────────────────────────────────────
    let engine = temps_backup::engines::mariadb_dump::MariadbDumpEngine::new(
        temps_backup::engines::mariadb_dump::MariadbDumpDeps {
            db: Arc::clone(&env.pool),
            encryption_service: Arc::clone(&encryption),
            docker: env.docker.clone(),
        },
    );
    let (dump_location, backup_model) = run_engine_backup(
        &env.pool,
        &engine,
        "mariadb_dump",
        "logical-dump",
        service_model.id,
        s3_source_model.id,
        user_id,
    )
    .await?;
    assert!(
        dump_location.ends_with("dump.sql.gz"),
        "engine should produce a logical dump, got {dump_location}"
    );
    assert!(
        env.s3_client
            .head_object()
            .bucket(BUCKET)
            .key(&dump_location)
            .send()
            .await
            .is_ok(),
        "dump object must exist in MinIO at {dump_location}"
    );
    // This is the exact predicate `credential_propagation_gates` keys off to
    // decide (false, false) for MariaDB — assert it on the REAL produced key.
    assert!(
        !MariaDbService::is_physical_base_location(&dump_location),
        "a logical dump must not classify as a physical base ({dump_location}); \
         if it did, the restore would merge the ORIGIN's credentials"
    );

    // ── REAL restore into a new service with the TARGET's own credentials ───
    let decrypted_s3_source = {
        let mut m = s3_source_model.clone();
        m.access_key_id = encryption.decrypt_string(&s3_source_model.access_key_id)?;
        m.secret_key = encryption.decrypt_string(&s3_source_model.secret_key)?;
        m
    };
    let s3_credentials = S3Credentials {
        access_key_id: MINIO_ACCESS_KEY.to_string(),
        secret_key: MINIO_SECRET_KEY.to_string(),
        region: "us-east-1".to_string(),
        endpoint: decrypted_s3_source.endpoint.clone(),
        bucket_name: BUCKET.to_string(),
        bucket_path: String::new(),
        force_path_style: true,
    };

    let restored_name = format!("{service_name}-dumprestore");
    let source_config = ServiceConfig {
        name: restored_name.clone(),
        service_type: ServiceType::Mariadb,
        version: None,
        // No origin-credential merge: this is what the (false, false) gate
        // hands the provider for a `mariadb_dump` restore.
        parameters: restored_mariadb_params(&restored_name),
    };

    let mariadb_svc = MariaDbService::new(service_name.to_string(), Arc::new(env.docker.clone()));
    let restore_ctx = RestoreContext {
        s3_client: &env.s3_client,
        s3_credentials: &s3_credentials,
        s3_source: &decrypted_s3_source,
        backup: &backup_model,
        backup_location: &dump_location,
        source_service: &service_model,
        source_config,
        pool,
    };

    // Register the container for reaping BEFORE the restore runs: the provider
    // creates it partway through, and a mid-restore failure must not leak it.
    let restored_container = format!("mariadb-{restored_name}");
    let _restored_guard = ContainerGuard {
        docker: env.docker.clone(),
        id: restored_container.clone(),
        label: restored_container.clone(),
    };
    let restored_volume = format!("mariadb_data_{restored_name}");

    let result = mariadb_svc
        .restore_to_new_service(restore_ctx, restored_name.clone(), serde_json::Value::Null)
        .await
        .map_err(|e| anyhow::anyhow!("restore_to_new_service: {e}"))?;
    eprintln!("Restore produced new service: {}", result.connection_info);

    let restored_port: u16 = result
        .parameters
        .get("port")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "restored service has no port param: {:?}",
                result.parameters
            )
        })?;
    eprintln!("Restored MariaDB on host port {restored_port}");

    // ── Assert: the target's OWN root password works ────────────────────────
    let restored = {
        let conn = format!("mysql://root:{RESTORED_ROOT_PASSWORD}@127.0.0.1:{restored_port}/");
        let mut pool = None;
        for attempt in 0..30 {
            match MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(3))
                .connect(&conn)
                .await
            {
                Ok(p) => {
                    pool = Some(p);
                    break;
                }
                Err(_) if attempt < 29 => tokio::time::sleep(Duration::from_millis(750)).await,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "restored MariaDB rejected the TARGET's own root password — the \
                         logical-dump restore must not overwrite the target's credentials: {e}"
                    ))
                }
            }
        }
        pool.ok_or_else(|| {
            anyhow::anyhow!(
                "restored MariaDB never accepted the TARGET's own root password; the \
                 logical-dump restore must leave mysql.user untouched"
            )
        })?
    };

    // ── Assert: the data really round-tripped ───────────────────────────────
    let restored_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events")
        .fetch_one(&restored)
        .await?;
    let batch_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM appdb.events WHERE batch='A'")
        .fetch_one(&restored)
        .await?;
    eprintln!(
        "Restored logical-dump row counts - total={restored_rows} A={batch_a} (expected 7/7)"
    );
    assert_eq!(restored_rows, 7, "all dumped rows must be restored");
    assert_eq!(batch_a, 7, "restored rows must carry their original values");
    restored.close().await;

    // ── The key assertion: the ORIGIN's root password must NOT work ─────────
    // `mariadb_dump` excludes the `mysql` schema, so nothing in the dump can
    // replace the target's `mysql.user`. If this ever succeeds, the restore
    // propagated origin credentials it had no business propagating.
    let origin_conn = format!("mysql://root:{ROOT_PASSWORD}@127.0.0.1:{restored_port}/");
    let origin_auth = MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&origin_conn)
        .await;
    match origin_auth {
        Ok(p) => {
            p.close().await;
            panic!(
                "SECURITY: the restored service accepted the ORIGIN's root password. A \
                 mariadb_dump restore must never propagate the origin's credentials \
                 (credential_propagation_gates returns (false, false) for this format)."
            );
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!("Origin password correctly rejected by restored service: {msg}");
            assert!(
                msg.contains("1045") || msg.to_lowercase().contains("access denied"),
                "expected an authentication failure (1045 / access denied) when using the \
                 origin's password, got a different error: {msg}"
            );
        }
    }

    // Best-effort: remove the restored data volume so it doesn't leak.
    let _ = env
        .docker
        .remove_volume(
            &restored_volume,
            Some(bollard::query_parameters::RemoveVolumeOptions { force: true }),
        )
        .await;

    Ok(())
}

/// Build the provider-side `MariaDbConfig` indirectly: the provider parses a
/// `ServiceConfig` internally, so we hand `archive_binlogs` a config by
/// round-tripping through the same parameters. The provider's `archive_binlogs`
/// takes a `&MariaDbConfig`, which is constructed from the input config - but
/// that type is private, so we build it via the public `from`-able path used
/// by the engine isn't available either. Instead we rely on the provider's
/// own parsing: see note in caller. This helper returns the runtime config by
/// deserializing through the public input type.
fn parse_mariadb_config(
    service_name: &str,
    host_port: u16,
) -> temps_providers::externalsvc::mariadb::MariaDbConfig {
    let input: temps_providers::externalsvc::mariadb::MariaDbInputConfig =
        serde_json::from_value(mariadb_params(service_name, host_port))
            .expect("parse MariaDbInputConfig");
    temps_providers::externalsvc::mariadb::MariaDbConfig::from(input)
}
