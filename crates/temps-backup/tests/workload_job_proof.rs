// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ADR-055 Phase 9 proof: a database on a worker node is backed up and
//! restored by `RunToCompletion` jobs the workload plane pins to that node.
//!
//! Against a real Docker daemon (skipped when none is reachable) and a real
//! PostgreSQL schema for the plane:
//!
//! 1. a WAL-G-enabled PostgreSQL ("the database") and RustFS ("the bucket")
//!    run on a private network; the database holds `proof(x) = 42`;
//! 2. the plane's placement plays a worker, node 7, whose driver is the
//!    local daemon. The job pins to node 7, so the reconciler must place it
//!    there (strict pinning) and nowhere else;
//! 3. the backup job dumps the database into the bucket with the exact
//!    script and environment the engine uses; the object must exist;
//! 4. the table is dropped, the restore job applies the dump, and `42` must
//!    be back;
//! 5. a truncated copy of the dump fails its restore job with the corrupt
//!    dump exit code, and the table is untouched;
//! 6. a backup job whose waiter is dropped (the executor's timeout winning)
//!    is stopped and removed by the plane.
//!
//! What this does **not** cover: the worker's own node runtime (the
//! `StreamNodeDriver` + `temps agent` hop). That hop is the plane's
//! existing worker transport, proved by its own tests; the steps to run
//! this against a real local worker are in the phase's progress notes.
//!
//! Gated behind `docker-tests` like the crate's other container tests, and
//! it skips at runtime when Docker is not reachable.
#![cfg(feature = "docker-tests")]

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::config::Region;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptions,
    RemoveContainerOptions, StartContainerOptions,
};
use bollard::Docker;
use futures::{FutureExt, StreamExt};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use temps_backup::engines::workload_job::{
    backup_script, exit, job_env, restore_script, run_database_job, start_database_job,
    stop_and_await_removal, DatabaseJobControl, DatabaseJobError, JobPurpose, JobState,
    PgJobTarget, RemovalTiming, S3JobTarget,
};
use temps_core::{AuditLogger, AuditOperation};
use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
use temps_workload::RuntimeDriver;
use temps_workload_docker::DockerDriver;
use temps_workload_plane::{
    labels, AttemptPlacement, NoEnvSource, NodeDriver, Placed, PlaneConfig, WorkloadPlane,
    WorkloadView,
};
use tokio_util::sync::CancellationToken;

const PG_IMAGE: &str = "gotempsh/postgres-walg:18-bookworm";
const RUSTFS_IMAGE: &str =
    "rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff";
const S3_KEY: &str = "rustfsadmin";
const PG_PASSWORD: &str = "proof-pw-1234";
const BUCKET: &str = "workload-job-proof";
const WORKER_NODE: i32 = 7;

struct NoAudit;

#[async_trait]
impl AuditLogger for NoAudit {
    async fn create_audit_log(&self, _: &dyn AuditOperation) -> temps_core::anyhow::Result<()> {
        Ok(())
    }
}

/// Plays worker node 7: every attempt is placed there, run by the local
/// daemon's driver.
struct SimulatedWorker {
    driver: Arc<dyn RuntimeDriver>,
}

#[async_trait]
impl AttemptPlacement for SimulatedWorker {
    async fn place(&self, _: &WorkloadView) -> Result<Placed, String> {
        Ok(Placed::Node {
            node_id: WORKER_NODE,
            node_name: "worker-proof".into(),
            driver: self.driver.clone(),
        })
    }

    async fn driver_for(&self, node_id: i32, _: &str) -> Result<Arc<dyn RuntimeDriver>, String> {
        if node_id == WORKER_NODE {
            Ok(self.driver.clone())
        } else {
            Err(format!("node {node_id} is not part of this test"))
        }
    }
}

struct Fixture {
    scope: String,
    network: String,
    pg: String,
    s3: String,
}

async fn remove(docker: &Docker, name: &str) {
    let _ = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            }),
        )
        .await;
}

async fn cleanup(docker: &Docker, fixture: &Fixture) {
    let options = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([(
            "label".to_string(),
            vec![format!("{}={}", labels::PLANE, fixture.scope)],
        )])),
        ..Default::default()
    };
    if let Ok(containers) = docker.list_containers(Some(options)).await {
        for id in containers.into_iter().filter_map(|c| c.id) {
            remove(docker, &id).await;
        }
    }
    remove(docker, &fixture.pg).await;
    remove(docker, &fixture.s3).await;
    let _ = docker.remove_network(&fixture.network).await;
}

async fn pull(docker: &Docker, image: &str) -> Result<(), String> {
    let mut stream = docker.create_image(
        Some(CreateImageOptionsBuilder::new().from_image(image).build()),
        None,
        None,
    );
    while let Some(item) = stream.next().await {
        item.map_err(|e| format!("pulling {image}: {e}"))?;
    }
    Ok(())
}

/// A network with an explicit subnet: a daemon whose default address pools
/// are exhausted cannot pick one itself.
async fn create_network(docker: &Docker, name: &str) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..8u32 {
        let octet = 100 + (uuid::Uuid::new_v4().as_u128() % 100) as u32 + attempt;
        let request = bollard::models::NetworkCreateRequest {
            name: name.to_string(),
            ipam: Some(bollard::models::Ipam {
                config: Some(vec![bollard::models::IpamConfig {
                    subnet: Some(format!("10.233.{}.0/24", octet % 250)),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        match docker.create_network(request).await {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("creating network {name}: {last}"))
}

async fn run_container(
    docker: &Docker,
    name: &str,
    network: &str,
    body: ContainerCreateBody,
) -> Result<(), String> {
    let mut body = body;
    let mut host = body.host_config.take().unwrap_or_default();
    host.network_mode = Some(network.to_string());
    body.host_config = Some(host);
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(name).build()),
            body,
        )
        .await
        .map_err(|e| format!("creating {name}: {e}"))?;
    docker
        .start_container(name, None::<StartContainerOptions>)
        .await
        .map_err(|e| format!("starting {name}: {e}"))
}

/// Run `cmd` in `container`; its stdout, or why it failed.
async fn exec(docker: &Docker, container: &str, cmd: &[&str]) -> Result<String, String> {
    let created = docker
        .create_exec(
            container,
            bollard::models::ExecConfig {
                cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("exec in {container}: {e}"))?;
    let mut out = String::new();
    if let bollard::exec::StartExecResults::Attached { mut output, .. } = docker
        .start_exec(&created.id, None)
        .await
        .map_err(|e| format!("exec in {container}: {e}"))?
    {
        while let Some(Ok(chunk)) = output.next().await {
            out.push_str(&chunk.to_string());
        }
    }
    let inspect = docker
        .inspect_exec(&created.id)
        .await
        .map_err(|e| format!("inspecting exec in {container}: {e}"))?;
    match inspect.exit_code {
        Some(0) => Ok(out),
        code => Err(format!("{cmd:?} in {container} exited {code:?}: {out}")),
    }
}

async fn psql(docker: &Docker, pg: &str, sql: &str) -> Result<String, String> {
    exec(
        docker,
        pg,
        &["psql", "-U", "postgres", "-h", "127.0.0.1", "-tAc", sql],
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_on_a_worker_is_backed_up_and_restored_by_jobs_on_its_node() {
    let docker = match Docker::connect_with_defaults() {
        Ok(docker) => docker,
        Err(error) => {
            println!("Docker not available, skipping: {error}");
            return;
        }
    };
    if let Err(error) = docker.ping().await {
        println!("Docker not available, skipping: {error}");
        return;
    }
    let db = match TestDatabase::new().await {
        Ok(db) => db,
        Err(error) => {
            let text = format!("{error:#}");
            if is_container_runtime_unavailable(&text) {
                println!("Docker not available for the test database, skipping: {text}");
                return;
            }
            panic!("test database could not be created: {text}");
        }
    };
    // The plane's tables, the claims table as `m20261022_000001` creates
    // it, then the node state change log and its triggers.
    let claims = "CREATE TABLE IF NOT EXISTS dns_name_claims ( \
                      fqdn TEXT PRIMARY KEY, \
                      owner_kind TEXT NOT NULL, \
                      owner_id BIGINT NOT NULL, \
                      target_workload_id BIGINT NULL, \
                      created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                      updated_at TIMESTAMPTZ NOT NULL DEFAULT now() \
                  )";
    for statement in temps_migrations::WORKLOAD_TABLES_SQL
        .iter()
        .chain(temps_migrations::VOLUME_TABLES_SQL)
        .chain(std::iter::once(
            &temps_migrations::WORKLOAD_INSTANCE_PORTS_SQL,
        ))
        .chain(std::iter::once(&claims))
        .chain(temps_migrations::NODE_STATE_TABLES_SQL)
        .chain(temps_migrations::NODE_STATE_DNS_SQL)
    {
        db.db
            .execute_unprepared(statement)
            .await
            .unwrap_or_else(|e| panic!("creating the plane's tables failed at {statement}: {e}"));
    }
    for image in [PG_IMAGE, RUSTFS_IMAGE] {
        if let Err(error) = pull(&docker, image).await {
            println!("Image not available, skipping: {error}");
            return;
        }
    }

    let tag = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let fixture = Fixture {
        scope: format!("p9job{tag}"),
        network: format!("p9-job-proof-{tag}"),
        pg: format!("p9-job-proof-pg-{tag}"),
        s3: format!("p9-job-proof-s3-{tag}"),
    };
    let outcome = AssertUnwindSafe(scenario(&docker, &db, &fixture))
        .catch_unwind()
        .await;
    cleanup(&docker, &fixture).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn scenario(docker: &Docker, db: &TestDatabase, fixture: &Fixture) {
    // 1. The database and the bucket on a private network.
    create_network(docker, &fixture.network)
        .await
        .expect("network");
    run_container(
        docker,
        &fixture.pg,
        &fixture.network,
        ContainerCreateBody {
            image: Some(PG_IMAGE.to_string()),
            env: Some(vec![format!("POSTGRES_PASSWORD={PG_PASSWORD}")]),
            ..Default::default()
        },
    )
    .await
    .expect("database container");
    let s3_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
        listener.local_addr().expect("port").port()
    };
    run_container(
        docker,
        &fixture.s3,
        &fixture.network,
        ContainerCreateBody {
            image: Some(RUSTFS_IMAGE.to_string()),
            env: Some(vec![
                format!("RUSTFS_ACCESS_KEY={S3_KEY}"),
                format!("RUSTFS_SECRET_KEY={S3_KEY}"),
            ]),
            host_config: Some(HostConfig {
                port_bindings: Some(HashMap::from([(
                    "9000/tcp".to_string(),
                    Some(vec![PortBinding {
                        host_ip: Some("127.0.0.1".to_string()),
                        host_port: Some(s3_port.to_string()),
                    }]),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .expect("bucket container");

    let mut ready = false;
    for _ in 0..90 {
        if psql(docker, &fixture.pg, "select 1").await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(ready, "the database never accepted connections");
    psql(
        docker,
        &fixture.pg,
        "create table proof(x int); insert into proof values (42);",
    )
    .await
    .expect("seeding the database");

    let s3 = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .endpoint_url(format!("http://127.0.0.1:{s3_port}"))
            .region(Region::new("us-east-1"))
            .behavior_version_latest()
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                S3_KEY, S3_KEY, None, None, "rustfs",
            ))
            .force_path_style(true)
            .build(),
    );
    let mut bucket = Err(String::new());
    for _ in 0..60 {
        bucket = s3
            .create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"));
        if bucket.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bucket.expect("creating the bucket");

    // 2. A plane whose only placement is worker node 7.
    let driver: Arc<dyn RuntimeDriver> = Arc::new(DockerDriver::new(docker.clone()));
    let mut config = PlaneConfig::new(fixture.scope.clone());
    config.network = Some(fixture.network.clone());
    config.poll_interval = Duration::from_millis(500);
    let plane = Arc::new(
        WorkloadPlane::new(
            db.db.clone(),
            NodeDriver(None),
            Arc::new(NoEnvSource),
            Arc::new(NoAudit),
            config,
        )
        .with_placement(Arc::new(SimulatedWorker { driver })),
    );
    let reconciler = plane.start().expect("reconciler");

    let target = PgJobTarget {
        service_id: 4_242,
        service_name: "proof-db".into(),
        node_id: WORKER_NODE,
        image: PG_IMAGE.into(),
        host: fixture.pg.clone(),
        port: 5432,
        username: "postgres".into(),
        database: "postgres".into(),
    };
    let bucket_target = S3JobTarget {
        s3_source_id: 1,
        bucket: BUCKET.into(),
        region: "us-east-1".into(),
        endpoint: Some(format!("http://{}:9000", fixture.s3)),
        force_path_style: true,
        access_key_id: S3_KEY.into(),
        secret_access_key: S3_KEY.into(),
        session_token: None,
    };
    let key = format!(
        "external_services/postgres/proof-db/{}/dump.sql.gz",
        fixture.scope
    );

    // 3. Backup.
    let backup_id = run_database_job(
        &plane,
        &target,
        JobPurpose::Backup { backup_id: 1 },
        backup_script(&target, &key),
        job_env(PG_PASSWORD, &bucket_target),
        &CancellationToken::new(),
    )
    .await
    .expect("the backup job succeeds");
    let object = s3
        .head_object()
        .bucket(BUCKET)
        .key(&key)
        .send()
        .await
        .expect("the dump is in the bucket");
    assert!(
        object.content_length().unwrap_or_default() > 0,
        "empty dump"
    );
    assert_ran_on_worker(db, backup_id).await;

    // 4. Lose the data, then restore it.
    psql(docker, &fixture.pg, "drop table proof;")
        .await
        .expect("dropping the table");
    let restore_id = run_database_job(
        &plane,
        &target,
        JobPurpose::Restore { restore_run_id: 1 },
        restore_script(&target, &key),
        job_env(PG_PASSWORD, &bucket_target),
        &CancellationToken::new(),
    )
    .await
    .expect("the restore job succeeds");
    assert_ran_on_worker(db, restore_id).await;
    let restored = psql(docker, &fixture.pg, "select x from proof;")
        .await
        .expect("reading the restored table");
    assert_eq!(restored.trim(), "42", "the restore brought the row back");

    // 5. A corrupt dump that S3 serves successfully fails the restore
    //    before psql touches the database.
    let corrupt_key = format!(
        "external_services/postgres/proof-db/{}/corrupt.sql.gz",
        fixture.scope
    );
    let mut corrupt = s3
        .get_object()
        .bucket(BUCKET)
        .key(&key)
        .send()
        .await
        .expect("reading the dump")
        .body
        .collect()
        .await
        .expect("reading the dump body")
        .into_bytes()
        .to_vec();
    corrupt.truncate(corrupt.len() / 2);
    s3.put_object()
        .bucket(BUCKET)
        .key(&corrupt_key)
        .body(corrupt.into())
        .send()
        .await
        .expect("uploading the truncated dump");
    let refused = run_database_job(
        &plane,
        &target,
        JobPurpose::Restore { restore_run_id: 2 },
        restore_script(&target, &corrupt_key),
        job_env(PG_PASSWORD, &bucket_target),
        &CancellationToken::new(),
    )
    .await
    .expect_err("a truncated dump must fail the restore");
    assert!(
        matches!(
            refused,
            DatabaseJobError::Failed {
                exit_code: Some(exit::CORRUPT_DUMP),
                ..
            }
        ),
        "{refused}"
    );
    let untouched = psql(docker, &fixture.pg, "select x from proof;")
        .await
        .expect("the table is still there");
    assert_eq!(untouched.trim(), "42", "nothing of the truncated dump ran");

    // 6. A backup job whose waiter is dropped (the executor's timeout won)
    //    is stopped and removed, never left dumping.
    let job = start_database_job(
        &plane,
        &target,
        JobPurpose::Backup { backup_id: 3 },
        "sleep 600".to_string(),
        job_env(PG_PASSWORD, &bucket_target),
    )
    .await
    .expect("the job starts");
    let abandoned = job.workload_id();
    let dropped =
        tokio::time::timeout(Duration::from_secs(5), job.wait(&CancellationToken::new())).await;
    assert!(dropped.is_err(), "the job sleeps past the timeout");
    stop_and_await_removal(
        plane.as_ref(),
        target.service_id,
        "backup",
        abandoned,
        "proof: waiting for the abandoned job",
        RemovalTiming {
            timeout: Duration::from_secs(60),
            poll: Duration::from_millis(500),
        },
    )
    .await
    .expect("the abandoned job is removed");
    assert_eq!(
        plane.job_state(abandoned).await.expect("state"),
        JobState::Gone
    );

    reconciler.abort();
}

/// Every attempt of workload `id` ran on the worker, never on node 0.
async fn assert_ran_on_worker(db: &TestDatabase, id: i64) {
    use temps_entities::workload_instances;
    let instances = workload_instances::Entity::find()
        .filter(workload_instances::Column::WorkloadId.eq(id))
        .all(db.db.as_ref())
        .await
        .expect("reading the job's instances");
    assert!(!instances.is_empty(), "workload {id} had no instance");
    for instance in instances {
        assert_eq!(
            instance.node_id,
            Some(WORKER_NODE),
            "instance {} of workload {id} ran off its pinned node",
            instance.id
        );
    }
}
