// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A real pg_auto_failover monitor and data node, built from the production
//! member commands, switched to a managed certificate and back while
//! running. Skips when Docker is unavailable.

use super::*;
use crate::externalsvc::ClusterMemberSpec;
use crate::postgres_cluster::{
    ClusterAuthSecrets, PostgresClusterConfig, PostgresClusterService, DEFAULT_CLUSTER_IMAGE,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_core::managed_service_pki::{generate_managed_services_ca, issue_server_certificate};

struct Harness {
    docker: Arc<Docker>,
    containers: Vec<String>,
    volumes: Vec<String>,
}

impl Harness {
    async fn new() -> Option<Self> {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => Arc::new(docker),
            Err(e) => {
                eprintln!("Docker not available, skipping: {e}");
                return None;
            }
        };
        if docker.ping().await.is_err() {
            eprintln!("Docker daemon not responding, skipping");
            return None;
        }
        if docker.inspect_image(DEFAULT_CLUSTER_IMAGE).await.is_err() {
            let (repo, tag) = DEFAULT_CLUSTER_IMAGE
                .rsplit_once(':')
                .unwrap_or((DEFAULT_CLUSTER_IMAGE, "latest"));
            let mut pull = docker.create_image(
                Some(
                    CreateImageOptionsBuilder::new()
                        .from_image(repo)
                        .tag(tag)
                        .build(),
                ),
                None,
                None,
            );
            while let Some(step) = pull.next().await {
                if let Err(e) = step {
                    eprintln!("Cannot pull {DEFAULT_CLUSTER_IMAGE}, skipping: {e}");
                    return None;
                }
            }
        }
        Some(Self {
            docker,
            containers: Vec::new(),
            volumes: Vec::new(),
        })
    }

    /// Run a member in the host's network namespace with its own volume at
    /// the production mount point, so the test needs no Docker network.
    async fn run_member(&mut self, name: &str, env: &HashMap<String, String>, cmd: Vec<String>) {
        let volume = format!("{name}_data");
        self.containers.push(name.to_string());
        self.volumes.push(volume.clone());
        let body = ContainerCreateBody {
            image: Some(DEFAULT_CLUSTER_IMAGE.to_string()),
            cmd: Some(cmd),
            env: Some(env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            host_config: Some(HostConfig {
                network_mode: Some("host".to_string()),
                binds: Some(vec![format!("{volume}:/var/lib/postgresql")]),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(name).build()),
                body,
            )
            .await
            .unwrap_or_else(|e| panic!("create member {name}: {e}"));
        self.docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .unwrap_or_else(|e| panic!("start member {name}: {e}"));
    }

    /// Run `cmd` as root in `container` with `env`: `(exit, stdout+stderr)`,
    /// `(-1, reason)` when it cannot run, never a panic.
    async fn exec(
        &self,
        container: &str,
        cmd: Vec<String>,
        env: &HashMap<String, String>,
    ) -> (i64, String) {
        let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let exec = match self
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd),
                    env: Some(env),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(exec) => exec,
            Err(e) => return (-1, format!("create exec in {container}: {e}")),
        };
        let mut out = String::new();
        if let Ok(StartExecResults::Attached { mut output, .. }) =
            self.docker.start_exec(&exec.id, None).await
        {
            while let Some(Ok(chunk)) = output.next().await {
                out.push_str(&chunk.to_string());
            }
        }
        let code = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .ok()
            .and_then(|i| i.exit_code)
            .unwrap_or(-1);
        (code, out)
    }

    async fn sh(&self, container: &str, script: &str) -> (i64, String) {
        self.exec(
            container,
            vec!["bash".into(), "-c".into(), script.into()],
            &HashMap::new(),
        )
        .await
    }

    async fn wait_for(&self, what: &str, timeout: Duration, container: &str, script: &str) {
        let start = Instant::now();
        loop {
            let (code, out) = self.sh(container, script).await;
            if code == 0 {
                return;
            }
            assert!(
                start.elapsed() < timeout,
                "{what} never happened in {container}; last output: {out}"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Removes every container and volume the test created, on every path.
impl Drop for Harness {
    fn drop(&mut self) {
        let containers = std::mem::take(&mut self.containers);
        let volumes = std::mem::take(&mut self.volumes);
        let docker = self.docker.clone();
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("could not build a runtime to clean up {containers:?}");
                return;
            };
            runtime.block_on(async move {
                for name in &containers {
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
                for volume in &volumes {
                    let _ = docker
                        .remove_volume(
                            volume,
                            None::<bollard::query_parameters::RemoveVolumeOptions>,
                        )
                        .await;
                }
            });
        });
        let _ = cleanup.join();
    }
}

/// `psql` from inside the data node as the application user, so the test
/// needs no route from the test process into the member's network.
fn app_query(sslmode: &str, extra: &str) -> String {
    format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -X -At \
         \"host=127.0.0.1 port=$NODE_PORT user=$POSTGRES_USER dbname=$POSTGRES_DB \
         sslmode={sslmode} {extra} connect_timeout=5\" \
         -c 'SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()'"
    )
}

/// Exits 0 when any file of the managed layout (or an earlier one) exists.
const ANY_TLS_FILES: &str = "for p in /var/lib/postgresql/tls /var/lib/postgresql/tls-current \
     /var/lib/postgresql/tls-a /var/lib/postgresql/tls-b /var/lib/postgresql/tls-staging; do \
     if [ -e \"$p\" ] || [ -L \"$p\" ]; then exit 0; fi; done; exit 1";

/// pg_autoctl's certificate and key settings, on one line.
const CONFIGURED_PAIR: &str = "echo $(gosu postgres pg_autoctl config get --pgdata /var/lib/postgresql/pgdata ssl.cert_file) \
     $(gosu postgres pg_autoctl config get --pgdata /var/lib/postgresql/pgdata ssl.key_file)";

const LINKED_PAIR: &str =
    "/var/lib/postgresql/tls-current/server.crt /var/lib/postgresql/tls-current/server.key";

/// How many of the two slot directories exist.
const SLOTS_PRESENT: &str =
    "ls -d /var/lib/postgresql/tls-a /var/lib/postgresql/tls-b 2>/dev/null | wc -l";

#[tokio::test]
async fn data_node_switches_to_the_managed_certificate_and_back_while_running() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let entropy = uuid::Uuid::new_v4();
    let name = format!("tls-ha-{}", &entropy.simple().to_string()[..8]);
    let base_port = 30000 + u16::try_from(entropy.as_u128() % 2000).unwrap_or(0) * 10;
    let docker = h.docker.clone();
    let cluster = PostgresClusterService::new(name, docker);
    let config: PostgresClusterConfig = serde_json::from_value(serde_json::json!({
        "password": "cluster-tls-test-password"
    }))
    .unwrap();
    let auth = ClusterAuthSecrets::generate();
    let spec = |role: &str, ordinal| ClusterMemberSpec {
        role: role.to_string(),
        node_id: None,
        ordinal,
        hostname: Some("127.0.0.1".to_string()),
    };

    let monitor = cluster.build_member_params(
        &spec("monitor", 0),
        &config,
        "127.0.0.1",
        base_port,
        base_port,
        Default::default(),
        &auth,
    );
    h.run_member(
        &monitor.container_name,
        &monitor.environment,
        monitor.command.clone().unwrap(),
    )
    .await;
    h.wait_for(
        "monitor accepting connections",
        Duration::from_secs(120),
        &monitor.container_name,
        &format!("gosu postgres pg_isready -q -p {base_port}"),
    )
    .await;

    let node = cluster.build_member_params(
        &spec("replica", 1),
        &config,
        "127.0.0.1",
        base_port,
        base_port + 1,
        Default::default(),
        &auth,
    );
    let node_name = node.container_name.clone();
    h.run_member(&node_name, &node.environment, node.command.clone().unwrap())
        .await;
    let node_is_single = format!(
        "[ \"$(gosu postgres psql -X -At -p {base_port} -d pg_auto_failover \
         -c \"SELECT reportedstate FROM pgautofailover.node WHERE nodename = '{node_name}'\")\" = single ]"
    );
    h.wait_for(
        "data node reaching `single`",
        Duration::from_secs(180),
        &monitor.container_name,
        &node_is_single,
    )
    .await;
    // The app role is created by a background loop of the entrypoint.
    h.wait_for(
        "app user accepting plaintext",
        Duration::from_secs(120),
        &node_name,
        &format!("[ \"$({})\" = f ]", app_query("disable", "")),
    )
    .await;
    eprintln!("{node_name} is `single` and accepts plaintext");

    let now = chrono::Utc::now();
    let ca = generate_managed_services_ca(now).unwrap();
    let leaf = issue_server_certificate(
        &ca.cert_pem,
        &ca.key_pem,
        &node_name,
        &["localhost".to_string(), "127.0.0.1".to_string()],
        now,
    )
    .unwrap();
    let ca_file = HashMap::from([("CA_PEM".to_string(), ca.cert_pem.clone())]);
    let (code, out) = h
        .exec(
            &node_name,
            vec![
                "bash".into(),
                "-c".into(),
                "printf '%s\\n' \"$CA_PEM\" > /tmp/ca.pem && chmod 644 /tmp/ca.pem".into(),
            ],
            &ca_file,
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let verified = app_query("verify-full", "sslrootcert=/tmp/ca.pem");

    // Before: the self-signed certificate cannot be verified.
    let (code, out) = h.sh(&node_name, &verified).await;
    assert_ne!(code, 0, "self-signed certificate was verified: {out}");

    let material = HashMap::from([
        (CLUSTER_TLS_CERT_ENV.to_string(), leaf.cert_pem.clone()),
        (CLUSTER_TLS_KEY_ENV.to_string(), leaf.key_pem.clone()),
    ]);

    // While a `pg_autoctl create` runs (a standby's first base backup), its
    // final config write would undo the change: refused, nothing touched.
    let (code, out) = h
        .sh(
            &node_name,
            "bash -c 'exec -a \"pg_autoctl create\" sleep 60' & echo $! > /tmp/fake-create.pid",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let (code, out) = h
        .exec(
            &node_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            &material,
        )
        .await;
    assert_eq!(code, NOT_INITIALIZED_EXIT_CODE, "{out}");
    assert!(out.contains("still creating"), "{out}");
    let (code, _) = h.sh(&node_name, ANY_TLS_FILES).await;
    assert_ne!(code, 0, "files written while pg_autoctl create ran");
    let (code, out) = h
        .sh(&node_name, "kill \"$(cat /tmp/fake-create.pid)\"")
        .await;
    assert_eq!(code, 0, "{out}");

    // Switch to the managed certificate.
    let (code, out) = h
        .exec(
            &node_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            &material,
        )
        .await;
    assert_eq!(code, 0, "apply managed certificate: {out}");
    assert_eq!(
        parse_served_fingerprint(&out).as_deref(),
        Some(leaf.fingerprint_sha256.as_str()),
        "{out}"
    );
    assert!(!out.contains("PRIVATE KEY"), "the key was echoed: {out}");
    eprintln!(
        "{node_name} serves the managed certificate {}",
        leaf.fingerprint_sha256
    );

    let (code, out) = h.sh(&node_name, &verified).await;
    assert_eq!((code, out.trim()), (0, "t"), "verify-full TLS: {out}");
    let (code, out) = h.sh(&node_name, &app_query("disable", "")).await;
    assert_eq!(
        (code, out.trim()),
        (0, "f"),
        "plaintext still accepted: {out}"
    );
    let (code, out) = h
        .sh(
            &node_name,
            "stat -L -c '%U %a' /var/lib/postgresql/tls-current/server.key /var/lib/postgresql/tls-current",
        )
        .await;
    assert_eq!(
        (code, out.trim()),
        (0, "postgres 600\npostgres 700"),
        "key ownership and mode"
    );
    let (code, out) = h.sh(&node_name, CONFIGURED_PAIR).await;
    assert_eq!(
        (code, out.trim()),
        (0, LINKED_PAIR),
        "pg_autoctl names the pair through the link"
    );
    let (code, out) = h.sh(&node_name, SLOTS_PRESENT).await;
    assert_eq!((code, out.trim()), (0, "1"), "only the slot in use is kept");
    // The monitor still drives the node: replication and failover use
    // pg_autoctl's own connections, unaffected by the served certificate.
    h.wait_for(
        "node still healthy on the monitor",
        Duration::from_secs(30),
        &monitor.container_name,
        &node_is_single,
    )
    .await;

    // Survives a restart: pg_autoctl keeps the paths on the volume.
    h.docker
        .restart_container(
            &node_name,
            None::<bollard::query_parameters::RestartContainerOptions>,
        )
        .await
        .unwrap();
    h.wait_for(
        "verified TLS after a restart",
        Duration::from_secs(120),
        &node_name,
        &format!("[ \"$({verified})\" = t ]"),
    )
    .await;
    let (code, out) = h
        .exec(&node_name, served_certificate_command(), &HashMap::new())
        .await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        parse_served_fingerprint(&out).as_deref(),
        Some(leaf.fingerprint_sha256.as_str())
    );

    // Applying again is a no-op that still succeeds.
    let (code, out) = h
        .exec(
            &node_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            &material,
        )
        .await;
    assert_eq!(code, 0, "re-apply: {out}");

    renewal_switches_slots(&h, &node_name, &ca, &leaf.fingerprint_sha256, &verified).await;
    unreadable_or_mismatched_settings_change_nothing(&h, &node_name, &material).await;

    // Back to pg_auto_failover's self-signed certificate.
    let (code, out) = h
        .exec(
            &node_name,
            server_tls_apply_command(ClusterServerCertificate::SelfSigned),
            &HashMap::new(),
        )
        .await;
    assert_eq!(code, 0, "restore self-signed: {out}");
    let served = parse_served_fingerprint(&out).unwrap();
    eprintln!("{node_name} serves its self-signed certificate {served} again");
    assert!(!served.is_empty());
    assert_ne!(served, leaf.fingerprint_sha256);
    let (code, _) = h.sh(&node_name, &verified).await;
    assert_ne!(code, 0, "managed certificate still served after off");
    let (code, out) = h.sh(&node_name, &app_query("require", "")).await;
    assert_eq!((code, out.trim()), (0, "t"), "self-signed TLS: {out}");
    let (code, _) = h.sh(&node_name, ANY_TLS_FILES).await;
    assert_ne!(code, 0, "managed files left behind");
    let (code, out) = h.sh(&node_name, CONFIGURED_PAIR).await;
    assert_eq!(
        (code, out.trim()),
        (
            0,
            "/var/lib/postgresql/pgdata/server.crt /var/lib/postgresql/pgdata/server.key"
        ),
        "pg_autoctl is back on its self-signed pair"
    );

    // The monitor is refused: it is not a data node.
    let (code, out) = h
        .exec(
            &monitor.container_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            &material,
        )
        .await;
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("not a pg_auto_failover data node"), "{out}");
}

/// A renewal switches both files together through the link, leaving
/// pg_autoctl's settings as they are, and keeps only the slot in use.
async fn renewal_switches_slots(
    h: &Harness,
    node_name: &str,
    ca: &temps_core::managed_service_pki::ManagedServicesCa,
    previous: &str,
    verified: &str,
) {
    let (_, before) = h
        .sh(node_name, "readlink /var/lib/postgresql/tls-current")
        .await;
    let renewed = match issue_server_certificate(
        &ca.cert_pem,
        &ca.key_pem,
        node_name,
        &["localhost".to_string(), "127.0.0.1".to_string()],
        chrono::Utc::now(),
    ) {
        Ok(renewed) => renewed,
        Err(e) => panic!("issuing the renewed certificate for {node_name} failed: {e}"),
    };
    assert_ne!(renewed.fingerprint_sha256, previous);
    let (code, out) = h
        .exec(
            node_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            &HashMap::from([
                (CLUSTER_TLS_CERT_ENV.to_string(), renewed.cert_pem.clone()),
                (CLUSTER_TLS_KEY_ENV.to_string(), renewed.key_pem.clone()),
            ]),
        )
        .await;
    assert_eq!(code, 0, "renew: {out}");
    assert_eq!(
        parse_served_fingerprint(&out).as_deref(),
        Some(renewed.fingerprint_sha256.as_str()),
        "{out}"
    );
    let (_, after) = h
        .sh(node_name, "readlink /var/lib/postgresql/tls-current")
        .await;
    assert_ne!(
        before.trim(),
        after.trim(),
        "the renewal went into the other slot"
    );
    let (code, out) = h.sh(node_name, CONFIGURED_PAIR).await;
    assert_eq!((code, out.trim()), (0, LINKED_PAIR));
    let (code, out) = h.sh(node_name, SLOTS_PRESENT).await;
    assert_eq!((code, out.trim()), (0, "1"), "the old slot is removed");
    let (code, out) = h.sh(node_name, verified).await;
    assert_eq!(
        (code, out.trim()),
        (0, "t"),
        "verify-full after renewal: {out}"
    );
}

/// What the node serves, as `<link target> <served fingerprint>`.
async fn served_state(h: &Harness, node_name: &str) -> String {
    let (_, link) = h
        .sh(node_name, "readlink /var/lib/postgresql/tls-current")
        .await;
    let (_, out) = h
        .exec(node_name, served_certificate_command(), &HashMap::new())
        .await;
    format!(
        "{} {}",
        link.trim(),
        parse_served_fingerprint(&out).unwrap_or_default()
    )
}

/// A setting pg_autoctl fails to report is never guessed, and a configured
/// pair whose halves differ is never copied behind the link: either way the
/// apply stops before changing anything.
async fn unreadable_or_mismatched_settings_change_nothing(
    h: &Harness,
    node_name: &str,
    material: &HashMap<String, String>,
) {
    let before = served_state(h, node_name).await;

    // `pg_autoctl config get ssl.key_file` fails (the keeper rewriting its
    // configuration); every other pg_autoctl call goes through.
    let (code, out) = h
        .sh(
            node_name,
            "mkdir -p /tmp/fakebin && REAL=$(command -v pg_autoctl) && \
             printf '#!/bin/sh\ncase \"$*\" in *\"config get\"*ssl.key_file*) exit 1 ;; esac\nexec %s \"$@\"\n' \"$REAL\" \
               > /tmp/fakebin/pg_autoctl && chmod 755 /tmp/fakebin/pg_autoctl && echo \"$PATH\"",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let mut env = material.clone();
    env.insert("PATH".to_string(), format!("/tmp/fakebin:{}", out.trim()));
    let (code, out) = h
        .exec(
            node_name,
            server_tls_apply_command(ClusterServerCertificate::SelfSigned),
            &env,
        )
        .await;
    assert_eq!(code, 6, "an unreadable setting stops the apply: {out}");
    assert!(out.contains("could not be read"), "{out}");
    assert_eq!(served_state(h, node_name).await, before, "nothing changed");

    // pg_autoctl names a key that does not belong to the certificate.
    let (code, out) = h
        .sh(
            node_name,
            "openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
               -out /var/lib/postgresql/other.key 2>&1 && \
             chown postgres:postgres /var/lib/postgresql/other.key && \
             chmod 600 /var/lib/postgresql/other.key && \
             gosu postgres pg_autoctl config set --pgdata /var/lib/postgresql/pgdata \
               ssl.key_file /var/lib/postgresql/other.key >/dev/null",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let (code, out) = h
        .exec(
            node_name,
            server_tls_apply_command(ClusterServerCertificate::SelfSigned),
            &HashMap::new(),
        )
        .await;
    assert_eq!(
        code, 4,
        "a mismatched configured pair stops the apply: {out}"
    );
    assert!(out.contains("does not match"), "{out}");
    let (_, link) = h
        .sh(node_name, "readlink /var/lib/postgresql/tls-current")
        .await;
    assert_eq!(
        before.split(' ').next(),
        Some(link.trim()),
        "the link was not switched"
    );

    // Put the node back on its managed pair.
    let (code, out) = h
        .sh(
            node_name,
            "gosu postgres pg_autoctl config set --pgdata /var/lib/postgresql/pgdata \
               ssl.key_file /var/lib/postgresql/tls-current/server.key >/dev/null && \
             rm -f /var/lib/postgresql/other.key /tmp/fakebin/pg_autoctl",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    let (code, out) = h
        .exec(
            node_name,
            server_tls_apply_command(ClusterServerCertificate::Managed),
            material,
        )
        .await;
    assert_eq!(code, 0, "re-apply after the refused ones: {out}");
    let (code, out) = h.sh(node_name, CONFIGURED_PAIR).await;
    assert_eq!((code, out.trim()), (0, LINKED_PAIR));
}
