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
