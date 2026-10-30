// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Remote container log source — the `temps-deployments` adapter for the
//! log-aggregator's [`RemoteContainerLogSource`] port.
//!
//! The log-aggregator owns the chunk pipeline but knows nothing about nodes,
//! tokens, or mTLS. This adapter supplies both halves it needs:
//! - which remote containers are running (from `deployment_containers` joined to
//!   `deployments`/`nodes`), and
//! - a line stream for each one, opened over the agent's existing
//!   `/agent/containers/{id}/logs/stream` endpoint with the per-node mTLS client
//!   and decrypted bearer token (the same machinery the live log path uses).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures_util::{StreamExt, TryStreamExt};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::io::StreamReader;

use temps_log_aggregator::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogSourceError, RemoteLogStream,
};
use temps_workload_plane::WorkloadPlane;

/// Maximum bytes a single remote log line may occupy before the stream is
/// failed.
///
/// The control plane opens a follow stream for every tracked remote
/// container, so this buffer exists once per container and is held for the
/// lifetime of the stream. 1 MiB matches the downstream chunk limit, which is
/// the largest line the pipeline can usefully carry anyway.
const MAX_REMOTE_LOG_LINE_BYTES: usize = 1024 * 1024;

/// Adapter that lets the log-aggregator collect logs from remote worker nodes.
pub struct RemoteLogSourceImpl {
    db: Arc<DatabaseConnection>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// The workload plane, once it is up. microVM replicas run on the control
    /// plane but outside its Docker daemon, so their output is followed
    /// through the plane's driver; until the plane starts there are none.
    plane: OnceLock<Arc<WorkloadPlane>>,
}

impl RemoteLogSourceImpl {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<temps_config::ConfigService>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            config_service,
            encryption_service,
            plane: OnceLock::new(),
        }
    }

    /// Attach the started workload plane. The first call wins.
    pub fn set_plane(&self, plane: Arc<WorkloadPlane>) {
        let _ = self.plane.set(plane);
    }

    /// The running microVM replicas, which no Docker daemon can read.
    ///
    /// One query finds the control plane's running instances of
    /// `micro_vm` workloads; the deployment rows they belong to are resolved
    /// in two batch queries (no per-replica lookups).
    async fn list_micro_vm_replicas(
        &self,
    ) -> Result<Vec<RemoteContainerInfo>, RemoteLogSourceError> {
        use temps_entities::{deployment_containers, deployments, workload_instances, workloads};

        if self.plane.get().is_none() {
            return Ok(Vec::new());
        }
        let database = |e: sea_orm::DbErr| RemoteLogSourceError::Database(e.to_string());

        let instances = workload_instances::Entity::find()
            .find_also_related(workloads::Entity)
            .filter(workload_instances::Column::NodeId.is_null())
            .filter(workload_instances::Column::Phase.eq("running"))
            .filter(workloads::Column::DeletedAt.is_null())
            .filter(Expr::cust("workloads.spec->>'isolation' = 'micro_vm'"))
            .all(self.db.as_ref())
            .await
            .map_err(database)?;
        let runtime_ids: Vec<String> = instances
            .iter()
            .filter_map(|(instance, _)| instance.handle.get("id")?.as_str().map(str::to_string))
            .collect();
        if runtime_ids.is_empty() {
            return Ok(Vec::new());
        }

        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::ContainerId.is_in(runtime_ids))
            .filter(deployment_containers::Column::NodeId.is_null())
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(database)?;
        let deployment_ids: Vec<i32> = containers
            .iter()
            .map(|c| c.deployment_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let deployment_map: HashMap<i32, (i32, i32)> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deployment_ids))
            .all(self.db.as_ref())
            .await
            .map_err(database)?
            .into_iter()
            .map(|d| (d.id, (d.project_id, d.environment_id)))
            .collect();

        Ok(containers
            .into_iter()
            .filter_map(|c| {
                let &(project_id, environment_id) = deployment_map.get(&c.deployment_id)?;
                let service = c
                    .service_name
                    .clone()
                    .unwrap_or_else(|| c.container_name.clone());
                Some(RemoteContainerInfo {
                    node_id: None,
                    node_name: None,
                    container_id: c.container_id,
                    project_id,
                    env: environment_id.to_string(),
                    service,
                    deploy_id: Some(c.deployment_id),
                    external_service_id: None,
                })
            })
            .collect())
    }

    /// Follow a microVM replica's output through the plane.
    async fn open_micro_vm_stream(
        &self,
        container_id: &str,
    ) -> Result<RemoteLogStream, RemoteLogSourceError> {
        let not_found = || RemoteLogSourceError::NotFound {
            node_id: None,
            container_id: container_id.to_string(),
        };
        let source_error = |reason: String| RemoteLogSourceError::Source {
            node_id: None,
            reason,
        };
        let plane = self.plane.get().ok_or_else(|| {
            source_error(format!(
                "the workload plane is not running; cannot follow the logs of {container_id}"
            ))
        })?;
        let (workload_id, _driver) = plane
            .workload_for_instance(container_id)
            .await
            .map_err(|e| source_error(format!("looking up instance {container_id}: {e}")))?
            .ok_or_else(not_found)?;
        let stream = plane
            .follow_instance_logs(workload_id)
            .await
            .map_err(|e| source_error(e.to_string()))?
            .ok_or_else(not_found)?;
        Ok(stamp_driver_lines(stream, chrono::Utc::now))
    }
}

/// A driver's lines as the collector's wire form, `<rfc3339> <message>`.
///
/// A line keeps the time the guest read it from the process; one from a
/// guest agent that predates dated logs is dated when it arrives here,
/// within about a second of when it was written. Blank lines are dropped,
/// as they are for lines from a worker node.
fn stamp_driver_lines(
    stream: temps_workload::LogStream,
    now: impl Fn() -> chrono::DateTime<chrono::Utc> + Send + 'static,
) -> RemoteLogStream {
    Box::pin(stream.filter_map(move |item| {
        let mapped = match item {
            Ok(line) => {
                let text = String::from_utf8_lossy(&line.line);
                let text = text.trim_end_matches(['\n', '\r']);
                if text.trim().is_empty() {
                    None
                } else {
                    // The guest's time when it dated the line, else now.
                    let at = line.timestamp.unwrap_or_else(&now);
                    Some(Ok(format!(
                        "{} {}",
                        at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                        text
                    )))
                }
            }
            Err(error) => Some(Err(RemoteLogSourceError::Source {
                node_id: None,
                reason: error.to_string(),
            })),
        };
        futures_util::future::ready(mapped)
    }))
}

#[async_trait]
impl RemoteContainerLogSource for RemoteLogSourceImpl {
    async fn list_remote_containers(
        &self,
    ) -> Result<Vec<RemoteContainerInfo>, RemoteLogSourceError> {
        use temps_entities::{deployment_containers, deployments, nodes, service_members};

        // Currently-tracked deployment containers that run on a remote worker node.
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::NodeId.is_not_null())
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?;

        // Cluster members (Postgres HA monitor/primary/replica, etc.) placed on
        // a remote node. Their logs are invisible to the local collector too,
        // so we pull them the same way — but they key on the owning external
        // service rather than a project, so a remote replica's output surfaces
        // under `/storage/{id}/logs` alongside control-plane-local members.
        let members = service_members::Entity::find()
            .filter(service_members::Column::NodeId.is_not_null())
            .filter(service_members::Column::ContainerId.is_not_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?;

        if containers.is_empty() && members.is_empty() {
            return self.list_micro_vm_replicas().await;
        }

        // Batch-resolve deployment → (project_id, environment_id), avoiding
        // per-container N+1 queries.
        let deploy_ids: Vec<i32> = containers
            .iter()
            .map(|c| c.deployment_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let dep_map: HashMap<i32, (i32, i32)> = deployments::Entity::find()
            .filter(deployments::Column::Id.is_in(deploy_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?
            .into_iter()
            .map(|d| (d.id, (d.project_id, d.environment_id)))
            .collect();

        // node_id → node_name for both deployment containers and cluster members.
        let node_ids: Vec<i32> = containers
            .iter()
            .filter_map(|c| c.node_id)
            .chain(members.iter().filter_map(|m| m.node_id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let node_map: HashMap<i32, String> = nodes::Entity::find()
            .filter(nodes::Column::Id.is_in(node_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?
            .into_iter()
            .map(|n| (n.id, n.name))
            .collect();

        let node_name_for = |node_id: i32| {
            node_map
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| format!("node-{node_id}"))
        };

        let mut out = Vec::with_capacity(containers.len() + members.len());
        for c in containers {
            let Some(node_id) = c.node_id else {
                continue;
            };
            // Skip containers whose deployment we can't resolve (shouldn't
            // happen, but a dangling row must not crash the collector).
            let Some(&(project_id, environment_id)) = dep_map.get(&c.deployment_id) else {
                continue;
            };
            // `service` mirrors the local collector's `sh.temps.service` label
            // semantics: compose service name when present, else the container
            // name. Container-level filtering (by container_id) is the precise
            // mechanism; `service` is the coarse grouping.
            let service = c
                .service_name
                .clone()
                .unwrap_or_else(|| c.container_name.clone());

            out.push(RemoteContainerInfo {
                node_id: Some(node_id),
                node_name: Some(node_name_for(node_id)),
                container_id: c.container_id,
                project_id,
                env: environment_id.to_string(),
                service,
                deploy_id: Some(c.deployment_id),
                external_service_id: None,
            });
        }

        for m in members {
            let (Some(node_id), Some(container_id)) = (m.node_id, m.container_id) else {
                continue;
            };
            out.push(RemoteContainerInfo {
                node_id: Some(node_id),
                node_name: Some(node_name_for(node_id)),
                container_id,
                // Sentinel: external-service chunks key on external_service_id,
                // not project_id (see the local collector's resolution).
                project_id: 0,
                env: "default".to_string(),
                // Per-member container name so members stay distinguishable
                // within the service's log scope.
                service: m.container_name,
                deploy_id: None,
                external_service_id: Some(m.service_id),
            });
        }

        out.extend(self.list_micro_vm_replicas().await?);
        Ok(out)
    }

    async fn open_log_stream(
        &self,
        node_id: Option<i32>,
        container_id: &str,
        since_unix: i64,
    ) -> Result<RemoteLogStream, RemoteLogSourceError> {
        use temps_entities::nodes;

        // No node: a replica on the control plane that Docker cannot see.
        // Its position is the driver's own checkpoint, not `since_unix`.
        let Some(node_id) = node_id else {
            return self.open_micro_vm_stream(container_id).await;
        };

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?
            .ok_or_else(|| RemoteLogSourceError::NotFound {
                node_id: Some(node_id),
                container_id: container_id.to_string(),
            })?;

        let encrypted_token =
            node.token_encrypted
                .as_ref()
                .ok_or_else(|| RemoteLogSourceError::Source {
                    node_id: Some(node_id),
                    reason: "node has no agent token; cannot stream remote logs".to_string(),
                })?;
        let token = self
            .encryption_service
            .decrypt_string(encrypted_token)
            .map_err(|e| RemoteLogSourceError::Source {
                node_id: Some(node_id),
                reason: format!("failed to decrypt agent token: {e}"),
            })?;

        // follow + timestamps so we get a live, RFC3339-prefixed line stream.
        // `start_date` maps to Docker's `since` (seconds) for resume.
        let endpoint = crate::cluster_ca::node_agent_endpoint(&node);
        let mut url = format!(
            "{}/agent/containers/{}/logs/stream",
            endpoint.dial_url.trim_end_matches('/'),
            container_id,
        );
        let mut query: Vec<(&str, String)> = vec![
            ("timestamps", "true".to_string()),
            ("follow", "true".to_string()),
        ];
        if since_unix > 0 {
            query.push(("start_date", since_unix.to_string()));
        }
        let qs = query
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(&v)))
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&qs);

        // mTLS for https:// nodes (ADR-020), plain HTTP otherwise. No top-level
        // timeout — log follow streams are long-lived by design.
        let client = crate::cluster_ca::build_node_http_client(
            &endpoint,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            None,
        )
        .await
        .map_err(|e| RemoteLogSourceError::Source {
            node_id: Some(node_id),
            reason: format!("failed to build HTTP client: {e}"),
        })?;

        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| RemoteLogSourceError::Source {
                node_id: Some(node_id),
                reason: format!("failed to reach agent at {url}: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Err(RemoteLogSourceError::NotFound {
                    node_id: Some(node_id),
                    container_id: container_id.to_string(),
                });
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteLogSourceError::Source {
                node_id: Some(node_id),
                reason: format!("agent returned {status} for log stream: {body}"),
            });
        }

        // Turn the chunked HTTP body into clean, newline-delimited lines. The
        // agent interleaves NUL keepalive bytes; we strip them and skip the
        // resulting empty lines so the collector only sees real log lines.
        let byte_stream = Box::pin(
            resp.bytes_stream()
                .map_err(|e| std::io::Error::other(e.to_string())),
        );
        let reader = StreamReader::new(byte_stream);
        // `BufReader::lines()` has no maximum line length: it keeps appending
        // bytes until a newline or EOF. These streams are opened with
        // follow=true for *every* tracked remote container by a background
        // collector, so a deployed app that writes one very long line — or
        // simply never writes a newline — would make the control plane buffer
        // it indefinitely. The 1 MiB chunk limit downstream never applies,
        // because it only sees whole lines. `LinesCodec` bounds the in-flight
        // line instead, and errors out once the limit is passed.
        let lines = FramedRead::new(
            reader,
            LinesCodec::new_with_max_length(MAX_REMOTE_LOG_LINE_BYTES),
        );

        let stream =
            futures_util::stream::unfold((lines, node_id), |(mut lines, node_id)| async move {
                loop {
                    match lines.next().await {
                        Some(Ok(raw)) => {
                            let cleaned: String = raw.chars().filter(|&c| c != '\0').collect();
                            if cleaned.trim().is_empty() {
                                continue;
                            }
                            return Some((Ok(cleaned), (lines, node_id)));
                        }
                        None => return None,
                        Some(Err(e)) => {
                            // Includes LinesCodecError::MaxLineLengthExceeded.
                            // Surfacing it ends this container's stream rather
                            // than silently truncating, so the operator sees
                            // why the tail stopped; the collector reopens the
                            // stream on its next reconcile pass.
                            return Some((
                                Err(RemoteLogSourceError::Source {
                                    node_id: Some(node_id),
                                    reason: e.to_string(),
                                }),
                                (lines, node_id),
                            ));
                        }
                    }
                }
            });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_workload::{DriverError, LogLine};

    struct NoAudit;

    #[async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::anyhow::Result<()> {
            Ok(())
        }
    }

    fn driver_stream(items: Vec<Result<&'static str, DriverError>>) -> temps_workload::LogStream {
        futures::stream::iter(items.into_iter().map(|item| {
            item.map(|text| LogLine {
                stderr: false,
                line: bytes::Bytes::from(text),
                timestamp: None,
            })
        }))
        .boxed()
    }

    fn fixed_clock() -> impl Fn() -> chrono::DateTime<chrono::Utc> + Send + 'static {
        || {
            chrono::Utc
                .with_ymd_and_hms(2026, 10, 25, 10, 0, 0)
                .single()
                .expect("a valid instant")
        }
    }

    #[tokio::test]
    async fn driver_lines_are_dated_on_arrival_and_blanks_dropped() {
        let stream = stamp_driver_lines(
            driver_stream(vec![
                Ok("listening on :3000\n"),
                Ok("\n"),
                Ok("   \r\n"),
                Ok("no newline at the end"),
            ]),
            fixed_clock(),
        );
        let lines: Vec<String> = stream
            .map(|item| item.expect("a line"))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            lines,
            vec![
                "2026-10-25T10:00:00.000000000Z listening on :3000".to_string(),
                "2026-10-25T10:00:00.000000000Z no newline at the end".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn a_line_the_guest_dated_keeps_its_time() {
        let written = chrono::Utc
            .with_ymd_and_hms(2026, 10, 25, 9, 59, 58)
            .single()
            .expect("a valid instant");
        let stream = futures::stream::iter([Ok(LogLine {
            stderr: true,
            line: bytes::Bytes::from_static(b"warn: slow\n"),
            timestamp: Some(written),
        })])
        .boxed();
        let lines: Vec<String> = stamp_driver_lines(stream, fixed_clock())
            .map(|item| item.expect("a line"))
            .collect()
            .await;
        assert_eq!(lines, ["2026-10-25T09:59:58.000000000Z warn: slow"]);
    }

    #[tokio::test]
    async fn a_driver_error_ends_up_as_a_control_plane_source_error() {
        let stream = stamp_driver_lines(
            driver_stream(vec![
                Ok("before\n"),
                Err(DriverError::Runtime {
                    driver: "firecracker".into(),
                    operation: "read the logs of",
                    instance: "vm-1".into(),
                    reason: "guest unreachable".into(),
                }),
            ]),
            fixed_clock(),
        );
        let items: Vec<_> = stream.collect().await;
        assert!(items[0].is_ok());
        let message = items[1].as_ref().expect_err("the error").to_string();
        assert!(message.contains("the control plane"), "{message}");
        assert!(message.contains("vm-1"), "{message}");
        assert!(message.contains("guest unreachable"), "{message}");
    }

    async fn seed_workload_instance(
        db: &sea_orm::DatabaseConnection,
        name: &str,
        isolation: &str,
        runtime_id: &str,
        phase: &str,
    ) {
        let now = chrono::Utc::now();
        let workload = temps_entities::workloads::ActiveModel {
            owner_kind: Set("deployment".into()),
            owner_id: Set(1),
            name: Set(name.into()),
            spec: Set(serde_json::json!({ "isolation": isolation })),
            generation: Set(1),
            observed_generation: Set(1),
            change_seq: Set(0),
            status: Set(serde_json::json!({})),
            phase: Set("running".into()),
            owner_workload_id: Set(None),
            finalizers: Set(Vec::new()),
            expires_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("workload");
        temps_entities::workload_instances::ActiveModel {
            workload_id: Set(workload.id),
            revision: Set(1),
            attempt: Set(1),
            node_id: Set(None),
            driver: Set("firecracker".into()),
            handle: Set(serde_json::json!({
                "driver": "firecracker", "id": runtime_id, "name": name,
            })),
            address: Set(None),
            phase: Set(phase.into()),
            ready: Set(true),
            conditions: Set(serde_json::json!([])),
            exit_code: Set(None),
            published_ports: Set(serde_json::json!([])),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            change_seq: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("instance");
    }

    async fn seed_container(
        db: &sea_orm::DatabaseConnection,
        deployment_id: i32,
        runtime_id: &str,
    ) {
        temps_entities::deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            node_id: Set(None),
            container_id: Set(runtime_id.into()),
            container_name: Set(format!("name-{runtime_id}")),
            container_port: Set(Some(8080)),
            status: Set(Some("running".into())),
            created_at: Set(chrono::Utc::now()),
            deployed_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("container");
    }

    /// Against Postgres: only a running, undeleted microVM instance that a
    /// deployment container row points at is collected, and it is tagged
    /// with its project and environment but no node.
    #[tokio::test]
    async fn only_running_micro_vm_replicas_are_listed_without_a_node() {
        use temps_entities::{
            deployments, environments, preset::Preset, projects, upstream_config::UpstreamList,
        };

        let Ok(test_db) = temps_database::test_utils::TestDatabase::with_migrations().await else {
            println!("Docker not available, skipping");
            return;
        };
        let db = test_db.connection_arc();
        let project = projects::ActiveModel {
            name: Set("Shop".into()),
            slug: Set("shop".into()),
            repo_owner: Set("owner".into()),
            repo_name: Set("repo".into()),
            main_branch: Set("main".into()),
            preset: Set(Preset::NextJs),
            directory: Set("/".into()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            is_deleted: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("project");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("prod".into()),
            slug: Set("prod".into()),
            host: Set("app.example.test".into()),
            subdomain: Set("app.example.test".into()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("environment");
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("d1".into()),
            state: Set("running".into()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("deployment");

        // The one that must be listed.
        seed_workload_instance(db.as_ref(), "a", "micro_vm", "vm-running", "running").await;
        seed_container(db.as_ref(), deployment.id, "vm-running").await;
        // A container Docker reads: not this source's business.
        seed_workload_instance(db.as_ref(), "b", "process", "c-docker", "running").await;
        seed_container(db.as_ref(), deployment.id, "c-docker").await;
        // A microVM that has stopped.
        seed_workload_instance(db.as_ref(), "c", "micro_vm", "vm-stopped", "failed").await;
        seed_container(db.as_ref(), deployment.id, "vm-stopped").await;
        // A running microVM no deployment owns.
        seed_workload_instance(db.as_ref(), "d", "micro_vm", "vm-orphan", "running").await;

        let source = RemoteLogSourceImpl::new(
            db.clone(),
            Arc::new(temps_config::ConfigService::new(
                Arc::new(
                    temps_config::ServerConfig::new(
                        "127.0.0.1:3000".to_string(),
                        "postgresql://unused".to_string(),
                        None,
                        None,
                    )
                    .expect("config"),
                ),
                db.clone(),
            )),
            Arc::new(temps_core::EncryptionService::new(&"0".repeat(64)).expect("encryption")),
        );
        // Without a plane there is nothing to follow them through.
        assert!(source
            .list_micro_vm_replicas()
            .await
            .expect("list")
            .is_empty());

        source.set_plane(Arc::new(WorkloadPlane::new(
            db.clone(),
            temps_workload_plane::NodeDriver(None),
            Arc::new(temps_workload_plane::NoEnvSource),
            Arc::new(NoAudit),
            temps_workload_plane::PlaneConfig::new("remote-log-source-test"),
        )));
        let listed = source.list_micro_vm_replicas().await.expect("list");
        assert_eq!(listed.len(), 1, "{listed:?}");
        let replica = &listed[0];
        assert_eq!(replica.container_id, "vm-running");
        assert_eq!(replica.node_id, None);
        assert_eq!(replica.node_name, None);
        assert_eq!(replica.project_id, project.id);
        assert_eq!(replica.env, environment.id.to_string());
        assert_eq!(replica.deploy_id, Some(deployment.id));
    }
}
