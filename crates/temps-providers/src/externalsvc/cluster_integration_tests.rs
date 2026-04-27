//! Integration tests for service clusters.
//!
//! These tests require Docker and the `gotempsh/postgres-ha:18-bookworm` image.
//! They test the full lifecycle: cluster creation, data replication, failover, and recovery.
//!
//! Run unit tests:   `cargo test --lib -p temps-providers -- cluster_integration --nocapture`
//! Run Docker tests: `cargo test --lib -p temps-providers --features docker-tests -- cluster_integration --nocapture`

#[cfg(test)]
mod tests {
    use bollard::Docker;
    use std::sync::Arc;

    use crate::externalsvc::postgres_cluster::PostgresClusterService;
    use crate::externalsvc::{
        ClusterMemberInfo, ClusterMemberSpec, ExternalService, ServiceConfig, ServiceType,
    };

    // -----------------------------------------------------------------------
    // Trait-level tests (no Docker required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_postgres_cluster_supports_cluster() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));
        assert!(service.supports_cluster());
    }

    #[test]
    fn test_postgres_cluster_roles() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));
        let roles = service.valid_cluster_roles();
        assert!(roles.contains(&"monitor"));
        assert!(roles.contains(&"primary"));
        assert!(roles.contains(&"replica"));
    }

    #[tokio::test]
    async fn test_init_cluster_requires_monitor() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));

        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({}),
        };

        let result = service
            .init_cluster(
                config,
                vec![ClusterMemberSpec {
                    role: "primary".to_string(),
                    node_id: None,
                    ordinal: 0,
                    hostname: None,
                }],
            )
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("monitor"));
    }

    #[tokio::test]
    async fn test_init_cluster_returns_correct_members() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("mydb".to_string(), Arc::new(docker));

        let config = ServiceConfig {
            name: "mydb".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "database": "myapp",
                "username": "admin",
                "password": "secret"
            }),
        };

        let members = vec![
            ClusterMemberSpec {
                role: "monitor".to_string(),
                node_id: None,
                ordinal: 0,
                hostname: Some("10.0.0.1".to_string()),
            },
            ClusterMemberSpec {
                role: "primary".to_string(),
                node_id: Some(1),
                ordinal: 1,
                hostname: Some("10.0.0.2".to_string()),
            },
            ClusterMemberSpec {
                role: "replica".to_string(),
                node_id: Some(2),
                ordinal: 2,
                hostname: Some("10.0.0.3".to_string()),
            },
        ];

        let results = service.init_cluster(config, members).await.unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].role, "monitor");
        assert_eq!(results[0].container_name, "postgres-mydb-monitor");
        assert_eq!(results[0].ordinal, 0);
        assert_eq!(results[1].role, "primary");
        assert_eq!(results[1].container_name, "postgres-mydb-1");
        assert_eq!(results[1].ordinal, 1);
        assert_eq!(results[2].role, "replica");
        assert_eq!(results[2].container_name, "postgres-mydb-2");
        assert_eq!(results[2].ordinal, 2);
    }

    #[test]
    fn test_cluster_connection_string_excludes_monitor() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));

        let members = vec![
            ClusterMemberInfo {
                role: "monitor".to_string(),
                hostname: "10.0.0.1".to_string(),
                port: 5432,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "primary".to_string(),
                hostname: "10.0.0.2".to_string(),
                port: 5432,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "replica".to_string(),
                hostname: "10.0.0.3".to_string(),
                port: 5433,
                status: "running".to_string(),
            },
        ];

        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "database": "mydb",
                "username": "user",
                "password": "pass"
            }),
        };

        let conn = service
            .cluster_connection_string(&members, &config)
            .unwrap();

        assert!(
            !conn.contains("10.0.0.1"),
            "Monitor should not be in connection string"
        );
        assert!(conn.contains("10.0.0.2:5432"));
        assert!(conn.contains("10.0.0.3:5433"));
        assert!(conn.contains(","));
        assert!(conn.contains("target_session_attrs=read-write"));
    }

    #[test]
    fn test_cluster_connection_string_uses_fqdn_vip_when_all_members_are_fqdn() {
        // ADR-011: when every data node carries an FQDN under .temps.local,
        // emit the per-service VIP form so apps' next connection lands on
        // whatever the current primary is via the per-node DNS resolver.
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("orders".to_string(), Arc::new(docker));

        let members = vec![
            ClusterMemberInfo {
                role: "monitor".to_string(),
                hostname: "orders-0.orders.temps.local".to_string(),
                port: 6000,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "primary".to_string(),
                hostname: "orders-1.orders.temps.local".to_string(),
                port: 6001,
                status: "running".to_string(),
            },
            ClusterMemberInfo {
                role: "replica".to_string(),
                hostname: "orders-2.orders.temps.local".to_string(),
                port: 6001,
                status: "running".to_string(),
            },
        ];

        let config = ServiceConfig {
            name: "orders".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({
                "database": "shop",
                "username": "app",
                "password": "secret"
            }),
        };

        let conn = service
            .cluster_connection_string(&members, &config)
            .unwrap();

        // Single VIP host, no comma-separated multi-host fallback.
        assert!(
            !conn.contains(','),
            "FQDN branch must collapse to single host, got: {conn}"
        );
        assert!(
            conn.contains("@orders.temps.local:6001/shop"),
            "expected VIP with member port, got: {conn}"
        );
        assert!(conn.contains("target_session_attrs=read-write"));
        assert!(
            !conn.contains("orders-0"),
            "Monitor's FQDN must not leak into the connection string"
        );
    }

    #[test]
    fn test_cluster_connection_string_no_running_nodes() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("test".to_string(), Arc::new(docker));

        let members = vec![ClusterMemberInfo {
            role: "primary".to_string(),
            hostname: "10.0.0.2".to_string(),
            port: 5432,
            status: "stopped".to_string(),
        }];

        let config = ServiceConfig {
            name: "test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: serde_json::json!({"database": "db", "username": "u", "password": "p"}),
        };

        let result = service.cluster_connection_string(&members, &config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No running data nodes"));
    }

    #[test]
    fn test_build_member_params_generates_correct_monitor_env() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("ha-test".to_string(), Arc::new(docker));

        let config = crate::externalsvc::postgres_cluster::PostgresClusterConfig {
            database: "mydb".to_string(),
            username: "admin".to_string(),
            password: Some("secret".to_string()),
            max_connections: 100,
            replicas: 1,
            docker_image: None,
            ssl_mode: "prefer".to_string(),
        };

        let monitor_spec = ClusterMemberSpec {
            role: "monitor".to_string(),
            node_id: None,
            ordinal: 0,
            hostname: Some("10.0.0.1".to_string()),
        };

        let params = service.build_member_params(&monitor_spec, &config, "10.0.0.1", 6100, 6100);

        assert!(
            !params.environment.contains_key("MONITOR_URI"),
            "Monitor should not have MONITOR_URI"
        );
        assert_eq!(params.container_name, "postgres-ha-test-monitor");
    }

    #[test]
    fn test_build_member_params_data_node_has_monitor_uri() {
        let docker = Docker::connect_with_defaults()
            .unwrap_or_else(|_| Docker::connect_with_local_defaults().unwrap());
        let service = PostgresClusterService::new("ha-test".to_string(), Arc::new(docker));

        let config = crate::externalsvc::postgres_cluster::PostgresClusterConfig {
            database: "mydb".to_string(),
            username: "admin".to_string(),
            password: Some("secret".to_string()),
            max_connections: 100,
            replicas: 1,
            docker_image: None,
            ssl_mode: "prefer".to_string(),
        };

        let primary_spec = ClusterMemberSpec {
            role: "primary".to_string(),
            node_id: Some(1),
            ordinal: 1,
            hostname: Some("10.0.0.2".to_string()),
        };

        let params = service.build_member_params(&primary_spec, &config, "10.0.0.1", 6100, 6101);

        assert!(params.environment.contains_key("MONITOR_URI"));
        assert!(params
            .environment
            .get("MONITOR_URI")
            .unwrap()
            .contains("10.0.0.1:6100"));
        assert_eq!(params.environment.get("NODE_HOSTNAME").unwrap(), "10.0.0.2");
        assert_eq!(params.container_name, "postgres-ha-test-1");
    }

    // -----------------------------------------------------------------------
    // Docker integration tests — full cluster lifecycle
    // -----------------------------------------------------------------------

    #[cfg(feature = "docker-tests")]
    mod docker_tests {
        use bollard::exec::CreateExecOptions;
        use bollard::models::*;
        use bollard::query_parameters::*;
        use bollard::Docker;
        use futures::StreamExt;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use crate::externalsvc::postgres_cluster::PostgresClusterService;
        use crate::externalsvc::ExternalService;

        const TEST_IMAGE: &str = "gotempsh/postgres-ha:18-bookworm";
        const NETWORK_NAME: &str = "pg-cluster-integration-test";
        const TEST_DB: &str = "testdb";
        const TEST_USER: &str = "testuser";
        const TEST_PASSWORD: &str = "testpassword123";

        async fn connect_docker() -> Option<Arc<Docker>> {
            let docker = match Docker::connect_with_local_defaults() {
                Ok(d) => Arc::new(d),
                Err(e) => {
                    println!("Docker not available, skipping: {}", e);
                    return None;
                }
            };
            if docker.ping().await.is_err() {
                println!("Docker daemon not responding, skipping");
                return None;
            }
            Some(docker)
        }

        async fn image_available(docker: &Docker) -> bool {
            match docker.inspect_image(TEST_IMAGE).await {
                Ok(_) => true,
                Err(_) => {
                    println!(
                        "Image {} not found. Build it first:\n  docker build -t {} ~/poc/postgres-poc/",
                        TEST_IMAGE, TEST_IMAGE
                    );
                    false
                }
            }
        }

        async fn create_network(docker: &Docker) -> anyhow::Result<()> {
            let _ = docker.remove_network(NETWORK_NAME).await;
            docker
                .create_network(NetworkCreateRequest {
                    name: NETWORK_NAME.to_string(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create network: {}", e))?;
            Ok(())
        }

        async fn create_container(
            docker: &Docker,
            name: &str,
            env: Vec<String>,
            cmd: Vec<String>,
        ) -> anyhow::Result<String> {
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

            let config = ContainerCreateBody {
                image: Some(TEST_IMAGE.to_string()),
                cmd: Some(cmd),
                env: Some(env),
                hostname: Some(name.to_string()),
                user: Some("postgres".to_string()),
                host_config: Some(HostConfig {
                    network_mode: Some(NETWORK_NAME.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let container = docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(name).build()),
                    config,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create container {}: {}", name, e))?;

            docker
                .start_container(&container.id, None::<StartContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start container {}: {}", name, e))?;

            println!("  Started container: {} ({})", name, &container.id[..12]);
            Ok(container.id)
        }

        async fn wait_for_postgres(
            docker: &Docker,
            container_id: &str,
            container_name: &str,
            timeout_secs: u64,
        ) -> anyhow::Result<()> {
            let start = Instant::now();
            let timeout = Duration::from_secs(timeout_secs);

            loop {
                if start.elapsed() > timeout {
                    let mut log_stream = docker.logs(
                        container_id,
                        Some(LogsOptions {
                            tail: "30".to_string(),
                            stdout: true,
                            stderr: true,
                            ..Default::default()
                        }),
                    );
                    let mut log_text = String::new();
                    while let Some(Ok(chunk)) = log_stream.next().await {
                        log_text.push_str(&chunk.to_string());
                    }
                    return Err(anyhow::anyhow!(
                        "Container {} did not become healthy within {}s. Last logs:\n{}",
                        container_name,
                        timeout_secs,
                        log_text
                    ));
                }

                let exec = docker
                    .create_exec(
                        container_id,
                        CreateExecOptions {
                            cmd: Some(vec!["pg_isready", "-h", "localhost", "-p", "5432"]),
                            attach_stdout: Some(true),
                            attach_stderr: Some(true),
                            ..Default::default()
                        },
                    )
                    .await;

                if let Ok(exec) = exec {
                    let _ = docker.start_exec(&exec.id, None).await;
                    if let Ok(inspect) = docker.inspect_exec(&exec.id).await {
                        if inspect.exit_code == Some(0) {
                            println!("  {} is ready", container_name);
                            return Ok(());
                        }
                    }
                }

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }

        async fn exec_sql(
            docker: &Docker,
            container_id: &str,
            sql: &str,
            db: &str,
            user: &str,
        ) -> anyhow::Result<String> {
            let exec = docker
                .create_exec(
                    container_id,
                    CreateExecOptions {
                        cmd: Some(vec!["psql", "-U", user, "-d", db, "-t", "-A", "-c", sql]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create exec: {}", e))?;

            let output = docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start exec: {}", e))?;

            let mut result = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                while let Some(Ok(chunk)) = output.next().await {
                    result.push_str(&chunk.to_string());
                }
            }

            // Check exit code
            let inspect = docker.inspect_exec(&exec.id).await?;
            if let Some(code) = inspect.exit_code {
                if code != 0 {
                    return Err(anyhow::anyhow!(
                        "SQL command failed (exit code {}): {}",
                        code,
                        result.trim()
                    ));
                }
            }

            Ok(result.trim().to_string())
        }

        async fn get_cluster_state(docker: &Docker, monitor_id: &str) -> anyhow::Result<String> {
            let exec = docker
                .create_exec(
                    monitor_id,
                    CreateExecOptions {
                        cmd: Some(vec![
                            "pg_autoctl",
                            "show",
                            "state",
                            "--pgdata",
                            "/var/lib/postgresql/monitor",
                        ]),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create exec: {}", e))?;

            let output = docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start exec: {}", e))?;

            let mut result = String::new();
            if let bollard::exec::StartExecResults::Attached { mut output, .. } = output {
                while let Some(Ok(chunk)) = output.next().await {
                    result.push_str(&chunk.to_string());
                }
            }
            Ok(result)
        }

        async fn wait_for_replication(
            docker: &Docker,
            monitor_id: &str,
            timeout_secs: u64,
        ) -> anyhow::Result<()> {
            let start = Instant::now();
            let timeout = Duration::from_secs(timeout_secs);

            loop {
                if start.elapsed() > timeout {
                    let state = get_cluster_state(docker, monitor_id)
                        .await
                        .unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "Replication not established within {}s. State:\n{}",
                        timeout_secs,
                        state
                    ));
                }

                let state = get_cluster_state(docker, monitor_id)
                    .await
                    .unwrap_or_default();
                if state.contains("primary") && state.contains("secondary") {
                    println!("  Replication established:\n{}", state);
                    return Ok(());
                }

                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }

        async fn wait_for_cluster_state(
            docker: &Docker,
            monitor_id: &str,
            expected_pattern: &str,
            timeout_secs: u64,
        ) -> anyhow::Result<String> {
            let start = Instant::now();
            let timeout = Duration::from_secs(timeout_secs);

            loop {
                if start.elapsed() > timeout {
                    let state = get_cluster_state(docker, monitor_id)
                        .await
                        .unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "Expected state '{}' not reached within {}s. State:\n{}",
                        expected_pattern,
                        timeout_secs,
                        state
                    ));
                }

                let state = get_cluster_state(docker, monitor_id)
                    .await
                    .unwrap_or_default();
                if state.contains(expected_pattern) {
                    return Ok(state);
                }

                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }

        async fn cleanup(docker: &Docker, container_ids: &[&str]) {
            println!("\n  Cleaning up...");
            for id in container_ids {
                let _ = docker
                    .stop_container(
                        id,
                        Some(StopContainerOptions {
                            t: Some(5),
                            signal: None,
                        }),
                    )
                    .await;
                let _ = docker
                    .remove_container(
                        id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            let _ = docker.remove_network(NETWORK_NAME).await;
            println!("  Cleanup complete");
        }

        fn monitor_cmd(hostname: &str) -> Vec<String> {
            vec![
                "bash".to_string(),
                "-c".to_string(),
                format!(
                    concat!(
                        "PGDATA=/var/lib/postgresql/monitor\n",
                        "if [ ! -f \"$PGDATA/pg_autoctl.cfg\" ]; then\n",
                        "  pg_autoctl create monitor \\\n",
                        "    --pgdata \"$PGDATA\" \\\n",
                        "    --pgport 5432 \\\n",
                        "    --hostname {} \\\n",
                        "    --auth trust \\\n",
                        "    --ssl-self-signed;\n",
                        "fi\n",
                        "rm -f /tmp/pg_autoctl/*.pid /tmp/pg_autoctl/*/*.pid\n",
                        "exec pg_autoctl run --pgdata \"$PGDATA\""
                    ),
                    hostname
                ),
            ]
        }

        fn node_cmd() -> Vec<String> {
            vec![
                "bash".to_string(),
                "-c".to_string(),
                [
                    "PGDATA=/var/lib/postgresql/pgdata",
                    "if [ ! -f \"$PGDATA/pg_autoctl.cfg\" ]; then",
                    "  pg_autoctl create postgres \\",
                    "    --pgdata \"$PGDATA\" \\",
                    "    --pgport 5432 \\",
                    "    --hostname \"$NODE_HOSTNAME\" \\",
                    "    --name \"$NODE_NAME\" \\",
                    "    --dbname testdb \\",
                    "    --auth trust \\",
                    "    --ssl-self-signed \\",
                    "    --monitor \"$MONITOR_URI\";",
                    "fi",
                    "rm -f /tmp/pg_autoctl/*.pid /tmp/pg_autoctl/*/*.pid",
                    "exec pg_autoctl run --pgdata \"$PGDATA\"",
                ]
                .join("\n"),
            ]
        }

        // =================================================================
        // TEST: Full cluster lifecycle — replication + failover + recovery
        // =================================================================

        #[tokio::test]
        async fn test_cluster_integration_docker_lifecycle() {
            let docker = match connect_docker().await {
                Some(d) => d,
                None => return,
            };

            if !image_available(&docker).await {
                return;
            }

            println!("\n=== PostgreSQL HA Cluster Integration Test ===\n");

            create_network(&docker)
                .await
                .expect("Failed to create network");

            let monitor_name = "pg-itest-monitor";
            let node1_name = "pg-itest-node1";
            let node2_name = "pg-itest-node2";

            // Start monitor
            println!("1. Starting monitor...");
            let monitor_id =
                create_container(&docker, monitor_name, vec![], monitor_cmd(monitor_name))
                    .await
                    .expect("Failed to create monitor");

            wait_for_postgres(&docker, &monitor_id, monitor_name, 60)
                .await
                .expect("Monitor did not become healthy");

            // Start node1 (will become primary)
            println!("\n2. Starting node1 (primary)...");
            let node1_id = create_container(
                &docker,
                node1_name,
                vec![
                    format!(
                        "MONITOR_URI=postgresql://autoctl_node@{}:5432/pg_auto_failover",
                        monitor_name
                    ),
                    format!("NODE_HOSTNAME={}", node1_name),
                    "NODE_NAME=node-1".to_string(),
                    format!("POSTGRES_USER={}", TEST_USER),
                    format!("POSTGRES_PASSWORD={}", TEST_PASSWORD),
                    format!("POSTGRES_DB={}", TEST_DB),
                ],
                node_cmd(),
            )
            .await
            .expect("Failed to create node1");

            wait_for_postgres(&docker, &node1_id, node1_name, 90)
                .await
                .expect("Node1 did not become healthy");

            // Start node2 (will become replica/secondary)
            println!("\n3. Starting node2 (replica)...");
            let node2_id = create_container(
                &docker,
                node2_name,
                vec![
                    format!(
                        "MONITOR_URI=postgresql://autoctl_node@{}:5432/pg_auto_failover",
                        monitor_name
                    ),
                    format!("NODE_HOSTNAME={}", node2_name),
                    "NODE_NAME=node-2".to_string(),
                    format!("POSTGRES_USER={}", TEST_USER),
                    format!("POSTGRES_PASSWORD={}", TEST_PASSWORD),
                    format!("POSTGRES_DB={}", TEST_DB),
                ],
                node_cmd(),
            )
            .await
            .expect("Failed to create node2");

            wait_for_postgres(&docker, &node2_id, node2_name, 90)
                .await
                .expect("Node2 did not become healthy");

            // Wait for replication
            println!("\n4. Waiting for replication...");
            wait_for_replication(&docker, &monitor_id, 120)
                .await
                .expect("Replication not established");

            // Create table and insert 100k rows on primary
            println!("\n5. Creating table and inserting 100k rows on primary...");

            // pg_autoctl creates the db via --dbname, but create it explicitly in case
            let _ = exec_sql(
                &docker,
                &node1_id,
                &format!("CREATE DATABASE {}", TEST_DB),
                "postgres",
                "postgres",
            )
            .await;

            tokio::time::sleep(Duration::from_secs(2)).await;

            exec_sql(
                &docker,
                &node1_id,
                "CREATE TABLE IF NOT EXISTS test_data (id SERIAL PRIMARY KEY, value TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW())",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to create table");

            exec_sql(
                &docker,
                &node1_id,
                "INSERT INTO test_data (value) SELECT 'row-' || generate_series(1, 100000)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to insert rows");

            let primary_count = exec_sql(
                &docker,
                &node1_id,
                "SELECT COUNT(*) FROM test_data",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count rows on primary");

            println!("  Primary row count: {}", primary_count);
            assert_eq!(primary_count, "100000", "Primary should have 100k rows");

            // Verify replication
            println!("\n6. Verifying replication to node2...");
            tokio::time::sleep(Duration::from_secs(5)).await;

            let replica_count = exec_sql(
                &docker,
                &node2_id,
                "SELECT COUNT(*) FROM test_data",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count rows on replica");

            println!("  Replica row count: {}", replica_count);
            assert_eq!(
                replica_count, "100000",
                "Replica should have 100k rows via replication"
            );

            // Failover: Stop the primary
            println!("\n7. Stopping primary (node1) to trigger failover...");
            docker
                .stop_container(
                    &node1_id,
                    Some(StopContainerOptions {
                        t: Some(5),
                        signal: None,
                    }),
                )
                .await
                .expect("Failed to stop node1");

            println!("  Waiting for failover...");
            let state = wait_for_cluster_state(&docker, &monitor_id, "wait_primary", 90)
                .await
                .expect("Failover did not complete");
            println!("  Post-failover state:\n{}", state);

            // Verify node2 is writable
            println!("\n8. Verifying node2 is writable after failover...");
            exec_sql(
                &docker,
                &node2_id,
                "INSERT INTO test_data (value) SELECT 'failover-row-' || generate_series(1, 1000)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Should be able to write to new primary after failover");

            let new_count = exec_sql(
                &docker,
                &node2_id,
                "SELECT COUNT(*) FROM test_data",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count after failover writes");

            println!("  New primary total rows: {}", new_count);
            assert_eq!(
                new_count, "101000",
                "New primary should have 101k rows (100k + 1k failover)"
            );

            // Recovery: Restart old primary
            println!("\n9. Restarting old primary (node1) — should rejoin as secondary...");
            docker
                .start_container(&node1_id, None::<StartContainerOptions>)
                .await
                .expect("Failed to restart node1");

            println!("  Waiting for node1 to rejoin...");
            let state = wait_for_cluster_state(&docker, &monitor_id, "secondary", 120)
                .await
                .expect("Node1 did not rejoin as secondary");
            println!("  Post-recovery state:\n{}", state);

            // Verify data integrity on recovered node
            println!("\n10. Verifying data integrity on recovered node...");
            wait_for_postgres(&docker, &node1_id, node1_name, 60)
                .await
                .expect("Recovered node1 not healthy");

            tokio::time::sleep(Duration::from_secs(5)).await;

            let recovered_count = exec_sql(
                &docker,
                &node1_id,
                "SELECT COUNT(*) FROM test_data",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count rows on recovered node");

            println!("  Recovered node row count: {}", recovered_count);
            assert_eq!(
                recovered_count, "101000",
                "Recovered node should have all 101k rows"
            );

            // Verify PostgresClusterService integration
            println!("\n11. Verifying PostgresClusterService integration...");
            let service = PostgresClusterService::new("itest".to_string(), docker.clone());
            assert!(service.supports_cluster());
            let roles = service.valid_cluster_roles();
            assert!(roles.contains(&"monitor"));
            assert!(roles.contains(&"primary"));
            assert!(roles.contains(&"replica"));

            println!("\n=== All cluster integration tests passed! ===\n");

            cleanup(&docker, &[&monitor_id, &node1_id, &node2_id]).await;
        }

        // =================================================================
        // TEST: Member recovery — kill and restart a replica
        // =================================================================

        async fn create_container_in_network(
            docker: &Docker,
            name: &str,
            env: Vec<String>,
            cmd: Vec<String>,
            network: &str,
        ) -> anyhow::Result<String> {
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

            let config = ContainerCreateBody {
                image: Some(TEST_IMAGE.to_string()),
                cmd: Some(cmd),
                env: Some(env),
                hostname: Some(name.to_string()),
                user: Some("postgres".to_string()),
                host_config: Some(HostConfig {
                    network_mode: Some(network.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            let container = docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(name).build()),
                    config,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create container {}: {}", name, e))?;

            docker
                .start_container(&container.id, None::<StartContainerOptions>)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to start container {}: {}", name, e))?;

            println!("  Started container: {} ({})", name, &container.id[..12]);
            Ok(container.id)
        }

        #[tokio::test]
        async fn test_cluster_integration_docker_member_recovery() {
            let docker = match connect_docker().await {
                Some(d) => d,
                None => return,
            };

            if !image_available(&docker).await {
                return;
            }

            println!("\n=== PostgreSQL HA Member Recovery Test ===\n");

            let network = "pg-cluster-recovery-test";
            let _ = docker.remove_network(network).await;
            docker
                .create_network(NetworkCreateRequest {
                    name: network.to_string(),
                    driver: Some("bridge".to_string()),
                    ..Default::default()
                })
                .await
                .expect("Failed to create network");

            let monitor_name = "pg-itest2-monitor";
            let node1_name = "pg-itest2-node1";
            let node2_name = "pg-itest2-node2";

            println!("1. Starting cluster...");

            let monitor_id = create_container_in_network(
                &docker,
                monitor_name,
                vec![],
                monitor_cmd(monitor_name),
                network,
            )
            .await
            .expect("Failed to create monitor");
            wait_for_postgres(&docker, &monitor_id, monitor_name, 60)
                .await
                .expect("Monitor not healthy");

            let node1_id = create_container_in_network(
                &docker,
                node1_name,
                vec![
                    format!(
                        "MONITOR_URI=postgresql://autoctl_node@{}:5432/pg_auto_failover",
                        monitor_name
                    ),
                    format!("NODE_HOSTNAME={}", node1_name),
                    "NODE_NAME=node-1".to_string(),
                    format!("POSTGRES_USER={}", TEST_USER),
                    format!("POSTGRES_PASSWORD={}", TEST_PASSWORD),
                    format!("POSTGRES_DB={}", TEST_DB),
                ],
                node_cmd(),
                network,
            )
            .await
            .expect("Failed to create node1");
            wait_for_postgres(&docker, &node1_id, node1_name, 90)
                .await
                .expect("Node1 not healthy");

            let node2_id = create_container_in_network(
                &docker,
                node2_name,
                vec![
                    format!(
                        "MONITOR_URI=postgresql://autoctl_node@{}:5432/pg_auto_failover",
                        monitor_name
                    ),
                    format!("NODE_HOSTNAME={}", node2_name),
                    "NODE_NAME=node-2".to_string(),
                    format!("POSTGRES_USER={}", TEST_USER),
                    format!("POSTGRES_PASSWORD={}", TEST_PASSWORD),
                    format!("POSTGRES_DB={}", TEST_DB),
                ],
                node_cmd(),
                network,
            )
            .await
            .expect("Failed to create node2");
            wait_for_postgres(&docker, &node2_id, node2_name, 90)
                .await
                .expect("Node2 not healthy");

            wait_for_replication(&docker, &monitor_id, 120)
                .await
                .expect("Replication not established");

            // Insert initial data
            println!("\n2. Inserting test data...");
            let _ = exec_sql(
                &docker,
                &node1_id,
                &format!("CREATE DATABASE {}", TEST_DB),
                "postgres",
                "postgres",
            )
            .await;
            tokio::time::sleep(Duration::from_secs(2)).await;

            exec_sql(
                &docker,
                &node1_id,
                "CREATE TABLE IF NOT EXISTS recovery_test (id SERIAL PRIMARY KEY, value TEXT)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to create table");

            exec_sql(
                &docker,
                &node1_id,
                "INSERT INTO recovery_test (value) SELECT 'initial-' || generate_series(1, 10000)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to insert initial data");

            tokio::time::sleep(Duration::from_secs(3)).await;

            // Kill replica
            println!("\n3. Killing replica (node2)...");
            docker
                .stop_container(
                    &node2_id,
                    Some(StopContainerOptions {
                        t: Some(2),
                        signal: None,
                    }),
                )
                .await
                .expect("Failed to stop node2");

            // Insert more data while replica is down
            println!("  Inserting data while replica is down...");
            exec_sql(
                &docker,
                &node1_id,
                "INSERT INTO recovery_test (value) SELECT 'during-outage-' || generate_series(1, 5000)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to insert during outage");

            // Restart replica
            println!("\n4. Restarting replica (node2)...");
            docker
                .start_container(&node2_id, None::<StartContainerOptions>)
                .await
                .expect("Failed to restart node2");

            wait_for_postgres(&docker, &node2_id, node2_name, 90)
                .await
                .expect("Restarted node2 not healthy");

            println!("  Waiting for replica to catch up...");
            wait_for_replication(&docker, &monitor_id, 120)
                .await
                .expect("Replication not re-established after recovery");

            tokio::time::sleep(Duration::from_secs(5)).await;

            // Verify data on recovered replica
            let recovered_count = exec_sql(
                &docker,
                &node2_id,
                "SELECT COUNT(*) FROM recovery_test",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count on recovered replica");

            println!(
                "  Recovered replica row count: {} (expected 15000)",
                recovered_count
            );
            assert_eq!(
                recovered_count, "15000",
                "Recovered replica should have all 15k rows (10k + 5k during outage)"
            );

            // Verify ongoing replication
            println!("\n5. Verifying ongoing replication...");
            exec_sql(
                &docker,
                &node1_id,
                "INSERT INTO recovery_test (value) SELECT 'post-recovery-' || generate_series(1, 2000)",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to insert post-recovery data");

            tokio::time::sleep(Duration::from_secs(3)).await;

            let final_count = exec_sql(
                &docker,
                &node2_id,
                "SELECT COUNT(*) FROM recovery_test",
                TEST_DB,
                TEST_USER,
            )
            .await
            .expect("Failed to count final rows");

            println!(
                "  Final replica row count: {} (expected 17000)",
                final_count
            );
            assert_eq!(
                final_count, "17000",
                "Replica should have all 17k rows after recovery + continued replication"
            );

            println!("\n=== Member recovery test passed! ===\n");

            // Cleanup
            for id in [&monitor_id, &node1_id, &node2_id] {
                let _ = docker
                    .stop_container(
                        id,
                        Some(StopContainerOptions {
                            t: Some(5),
                            signal: None,
                        }),
                    )
                    .await;
                let _ = docker
                    .remove_container(
                        id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
            let _ = docker.remove_network(network).await;
        }
    }
}
