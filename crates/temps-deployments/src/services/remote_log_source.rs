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
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tokio::io::AsyncBufReadExt;
use tokio_util::io::StreamReader;

use temps_log_aggregator::{
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogSourceError, RemoteLogStream,
};

/// Adapter that lets the log-aggregator collect logs from remote worker nodes.
pub struct RemoteLogSourceImpl {
    db: Arc<DatabaseConnection>,
    config_service: Arc<temps_config::ConfigService>,
    encryption_service: Arc<temps_core::EncryptionService>,
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
        }
    }
}

#[async_trait]
impl RemoteContainerLogSource for RemoteLogSourceImpl {
    async fn list_remote_containers(
        &self,
    ) -> Result<Vec<RemoteContainerInfo>, RemoteLogSourceError> {
        use temps_entities::{deployment_containers, deployments, nodes};

        // Currently-tracked containers that run on a remote worker node.
        let containers = deployment_containers::Entity::find()
            .filter(deployment_containers::Column::NodeId.is_not_null())
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?;

        if containers.is_empty() {
            return Ok(Vec::new());
        }

        // Batch-resolve deployment → (project_id, environment_id) and
        // node_id → node_name, avoiding per-container N+1 queries.
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

        let node_ids: Vec<i32> = containers
            .iter()
            .filter_map(|c| c.node_id)
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

        let mut out = Vec::with_capacity(containers.len());
        for c in containers {
            let Some(node_id) = c.node_id else {
                continue;
            };
            // Skip containers whose deployment we can't resolve (shouldn't
            // happen, but a dangling row must not crash the collector).
            let Some(&(project_id, environment_id)) = dep_map.get(&c.deployment_id) else {
                continue;
            };
            let node_name = node_map
                .get(&node_id)
                .cloned()
                .unwrap_or_else(|| format!("node-{node_id}"));
            // `service` mirrors the local collector's `sh.temps.service` label
            // semantics: compose service name when present, else the container
            // name. Container-level filtering (by container_id) is the precise
            // mechanism; `service` is the coarse grouping.
            let service = c
                .service_name
                .clone()
                .unwrap_or_else(|| c.container_name.clone());

            out.push(RemoteContainerInfo {
                node_id,
                node_name,
                container_id: c.container_id,
                project_id,
                env: environment_id.to_string(),
                service,
                deploy_id: Some(c.deployment_id),
            });
        }

        Ok(out)
    }

    async fn open_log_stream(
        &self,
        node_id: i32,
        container_id: &str,
        since_unix: i64,
    ) -> Result<RemoteLogStream, RemoteLogSourceError> {
        use temps_entities::nodes;

        let node = nodes::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| RemoteLogSourceError::Database(e.to_string()))?
            .ok_or_else(|| RemoteLogSourceError::NotFound {
                node_id,
                container_id: container_id.to_string(),
            })?;

        let encrypted_token =
            node.token_encrypted
                .as_ref()
                .ok_or_else(|| RemoteLogSourceError::Source {
                    node_id,
                    reason: "node has no agent token; cannot stream remote logs".to_string(),
                })?;
        let token = self
            .encryption_service
            .decrypt_string(encrypted_token)
            .map_err(|e| RemoteLogSourceError::Source {
                node_id,
                reason: format!("failed to decrypt agent token: {e}"),
            })?;

        // follow + timestamps so we get a live, RFC3339-prefixed line stream.
        // `start_date` maps to Docker's `since` (seconds) for resume.
        let mut url = format!(
            "{}/agent/containers/{}/logs/stream",
            node.address.trim_end_matches('/'),
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
            &node.address,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            None,
        )
        .await
        .map_err(|e| RemoteLogSourceError::Source {
            node_id,
            reason: format!("failed to build HTTP client: {e}"),
        })?;

        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| RemoteLogSourceError::Source {
                node_id,
                reason: format!("failed to reach agent at {url}: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Err(RemoteLogSourceError::NotFound {
                    node_id,
                    container_id: container_id.to_string(),
                });
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteLogSourceError::Source {
                node_id,
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
        let lines = tokio::io::BufReader::new(reader).lines();

        let stream =
            futures_util::stream::unfold((lines, node_id), |(mut lines, node_id)| async move {
                loop {
                    match lines.next_line().await {
                        Ok(Some(raw)) => {
                            let cleaned: String = raw.chars().filter(|&c| c != '\0').collect();
                            if cleaned.trim().is_empty() {
                                continue;
                            }
                            return Some((Ok(cleaned), (lines, node_id)));
                        }
                        Ok(None) => return None,
                        Err(e) => {
                            return Some((
                                Err(RemoteLogSourceError::Source {
                                    node_id,
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
