// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! KV Service implementation with Redis backend from temps-providers

use std::sync::Arc;

use redis::AsyncCommands;
use serde_json::Value;
use temps_providers::externalsvc::RedisService;
use tracing::debug;

use crate::error::KvError;

/// Options for SET operations
#[derive(Debug, Clone, Default)]
pub struct SetOptions {
    /// Expire in seconds
    pub ex: Option<i64>,
    /// Expire in milliseconds
    pub px: Option<i64>,
    /// Only set if key does not exist
    pub nx: bool,
    /// Only set if key exists
    pub xx: bool,
}

/// Where the KV service gets its Redis connections. Production uses the
/// managed [`RedisService`]; tests connect to a Redis of their own.
#[async_trait::async_trait]
pub trait KvConnectionSource: Send + Sync {
    async fn connection(&self) -> Result<redis::aio::ConnectionManager, KvError>;
}

#[async_trait::async_trait]
impl KvConnectionSource for RedisService {
    async fn connection(&self) -> Result<redis::aio::ConnectionManager, KvError> {
        self.get_connection().await.map_err(|e| {
            KvError::ConnectionFailed(format!("Redis service '{}': {}", self.instance_name(), e))
        })
    }
}

/// KV Service for key-value operations with project isolation
/// Uses RedisService from temps-providers for container management
pub struct KvService {
    connections: Arc<dyn KvConnectionSource>,
}

impl KvService {
    /// Create a new KV service backed by a RedisService from temps-providers
    pub fn new(redis_service: Arc<RedisService>) -> Self {
        Self::with_connection_source(redis_service)
    }

    /// Create a KV service that takes its connections from `connections`.
    pub fn with_connection_source(connections: Arc<dyn KvConnectionSource>) -> Self {
        Self { connections }
    }

    /// Build the namespaced key for project isolation
    /// Keys are prefixed with "kv:p{project_id}:" to:
    /// 1. Isolate KV data from other Redis uses
    /// 2. Isolate projects from each other
    fn namespaced_key(&self, project_id: i32, key: &str) -> String {
        format!("kv:p{}:{}", project_id, key)
    }

    /// Strip the namespace prefix from a key
    fn strip_namespace(&self, project_id: i32, key: &str) -> String {
        let prefix = format!("kv:p{}:", project_id);
        key.strip_prefix(&prefix).unwrap_or(key).to_string()
    }

    /// Get a Redis connection from the connection source
    async fn get_connection(&self) -> Result<redis::aio::ConnectionManager, KvError> {
        self.connections.connection().await
    }

    /// Get a value by key
    pub async fn get(&self, project_id: i32, key: &str) -> Result<Option<Value>, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV GET {}", namespaced);

        let result: Option<String> = conn.get(&namespaced).await.map_err(KvError::Redis)?;

        match result {
            Some(s) => {
                // Try to parse as JSON, fallback to string
                let value = serde_json::from_str(&s).unwrap_or(Value::String(s));
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Set a value with optional expiration.
    ///
    /// Returns `true` when the value was written and `false` when an NX/XX
    /// condition did not hold (Redis answers `nil`, and nothing changes).
    pub async fn set(
        &self,
        project_id: i32,
        key: &str,
        value: Value,
        options: SetOptions,
    ) -> Result<bool, KvError> {
        debug!("KvService::set - getting connection...");
        let mut conn = self.get_connection().await?;
        debug!("KvService::set - got connection");

        let namespaced = self.namespaced_key(project_id, key);

        // Serialize value to JSON string
        let serialized =
            serde_json::to_string(&value).map_err(|e| KvError::Serialization(e.to_string()))?;

        debug!("KV SET {} = {}", namespaced, serialized);

        // Build the SET command with options
        let mut cmd = redis::cmd("SET");
        cmd.arg(&namespaced).arg(&serialized);

        // Add expiration options
        if let Some(ex) = options.ex {
            cmd.arg("EX").arg(ex);
        } else if let Some(px) = options.px {
            cmd.arg("PX").arg(px);
        }

        // Add NX/XX options
        if options.nx {
            cmd.arg("NX");
        } else if options.xx {
            cmd.arg("XX");
        }

        let result: Option<String> = cmd.query_async(&mut conn).await?;

        // Redis answers nil, not an error, when NX/XX does not hold.
        let written = result.is_some();
        if !written {
            debug!("KV SET condition not met for {}", namespaced);
        }

        Ok(written)
    }

    /// Delete one or more keys
    pub async fn del(&self, project_id: i32, keys: Vec<String>) -> Result<i64, KvError> {
        if keys.is_empty() {
            return Ok(0);
        }

        let mut conn = self.get_connection().await?;

        let namespaced_keys: Vec<String> = keys
            .iter()
            .map(|k| self.namespaced_key(project_id, k))
            .collect();

        debug!("KV DEL {:?}", namespaced_keys);

        let deleted: i64 = conn.del(&namespaced_keys).await.map_err(KvError::Redis)?;

        Ok(deleted)
    }

    /// Increment a numeric value
    pub async fn incr(&self, project_id: i32, key: &str) -> Result<i64, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV INCR {}", namespaced);

        let result: i64 = conn.incr(&namespaced, 1).await.map_err(KvError::Redis)?;

        Ok(result)
    }

    /// Increment a numeric value by a specific amount
    pub async fn incrby(&self, project_id: i32, key: &str, amount: i64) -> Result<i64, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV INCRBY {} {}", namespaced, amount);

        let result: i64 = conn
            .incr(&namespaced, amount)
            .await
            .map_err(KvError::Redis)?;

        Ok(result)
    }

    /// Set expiration on a key (in seconds)
    pub async fn expire(&self, project_id: i32, key: &str, seconds: i64) -> Result<bool, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV EXPIRE {} {}", namespaced, seconds);

        let result: bool = conn
            .expire(&namespaced, seconds)
            .await
            .map_err(KvError::Redis)?;

        Ok(result)
    }

    /// Get the time-to-live for a key (in seconds)
    pub async fn ttl(&self, project_id: i32, key: &str) -> Result<i64, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV TTL {}", namespaced);

        let result: i64 = conn.ttl(&namespaced).await.map_err(KvError::Redis)?;

        Ok(result)
    }

    /// Get keys matching a pattern (within project namespace)
    pub async fn keys(&self, project_id: i32, pattern: &str) -> Result<Vec<String>, KvError> {
        let mut conn = self.get_connection().await?;

        // Prefix the pattern with project namespace
        let namespaced_pattern = self.namespaced_key(project_id, pattern);

        debug!("KV KEYS {}", namespaced_pattern);

        let keys: Vec<String> = conn
            .keys(&namespaced_pattern)
            .await
            .map_err(KvError::Redis)?;

        // Strip namespace from results
        let stripped: Vec<String> = keys
            .into_iter()
            .map(|k| self.strip_namespace(project_id, &k))
            .collect();

        Ok(stripped)
    }

    /// Check if a key exists
    pub async fn exists(&self, project_id: i32, key: &str) -> Result<bool, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV EXISTS {}", namespaced);

        let result: bool = conn.exists(&namespaced).await.map_err(KvError::Redis)?;

        Ok(result)
    }

    /// Get multiple values by keys
    pub async fn mget(
        &self,
        project_id: i32,
        keys: Vec<String>,
    ) -> Result<Vec<Option<Value>>, KvError> {
        if keys.is_empty() {
            return Ok(vec![]);
        }

        let mut conn = self.get_connection().await?;

        let namespaced_keys: Vec<String> = keys
            .iter()
            .map(|k| self.namespaced_key(project_id, k))
            .collect();

        debug!("KV MGET {:?}", namespaced_keys);

        let results: Vec<Option<String>> =
            conn.mget(&namespaced_keys).await.map_err(KvError::Redis)?;

        let values: Vec<Option<Value>> = results
            .into_iter()
            .map(|opt| opt.map(|s| serde_json::from_str(&s).unwrap_or(Value::String(s))))
            .collect();

        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    #[test]
    fn test_namespaced_key() {
        // Create a mock test - we can't easily test with real RedisService
        // but we can test the key formatting logic
        let prefix = format!("kv:p{}:", 123);
        let key = "user:1";
        let namespaced = format!("{}{}", prefix, key);
        assert_eq!(namespaced, "kv:p123:user:1");
    }

    #[test]
    fn test_strip_namespace() {
        let prefix = format!("kv:p{}:", 123);
        let namespaced = format!("{}user:1", prefix);
        let stripped = namespaced
            .strip_prefix(&prefix)
            .unwrap_or(&namespaced)
            .to_string();
        assert_eq!(stripped, "user:1");
    }

    /// `KvService::set` against a real Redis: NX/XX report whether they
    /// wrote, and a rejected condition leaves the stored value alone.
    /// Skips when Docker (or the Redis image) is unavailable.
    #[tokio::test]
    async fn set_reports_nx_and_xx_outcomes_against_redis() {
        use std::collections::HashMap;
        use std::time::Duration;

        use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptions,
        };
        use bollard::Docker;
        use futures::StreamExt;

        use super::{KvConnectionSource, KvService, SetOptions};
        use crate::error::KvError;

        struct DirectRedis(redis::Client);

        #[async_trait::async_trait]
        impl KvConnectionSource for DirectRedis {
            async fn connection(&self) -> Result<redis::aio::ConnectionManager, KvError> {
                redis::aio::ConnectionManager::new(self.0.clone())
                    .await
                    .map_err(|e| KvError::ConnectionFailed(e.to_string()))
            }
        }

        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) if docker.ping().await.is_ok() => docker,
            _ => {
                println!("Docker is unavailable; skipping the KV NX/XX Redis test");
                return;
            }
        };
        const IMAGE: &str = "redis:7-alpine";
        if docker.inspect_image(IMAGE).await.is_err() {
            let mut pull = docker.create_image(
                Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()),
                None,
                None,
            );
            while let Some(step) = pull.next().await {
                if let Err(e) = step {
                    println!("Could not pull {IMAGE} ({e}); skipping the KV NX/XX Redis test");
                    return;
                }
            }
        }

        let name = format!("temps-test-kv-nx-xx-{}", uuid::Uuid::new_v4().simple());
        let container = docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    host_config: Some(HostConfig {
                        port_bindings: Some(HashMap::from([(
                            "6379/tcp".to_string(),
                            Some(vec![PortBinding {
                                host_ip: Some("127.0.0.1".to_string()),
                                host_port: Some(String::new()),
                            }]),
                        )])),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test Redis container");

        // Run the checks, then remove the container whatever happened.
        let outcome: Result<(), String> = async {
            docker
                .start_container(
                    &container.id,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(|e| format!("start {name}: {e}"))?;
            let port = docker
                .inspect_container(
                    &container.id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .map_err(|e| format!("inspect {name}: {e}"))?
                .network_settings
                .and_then(|n| n.ports)
                .and_then(|ports| ports.get("6379/tcp").cloned().flatten())
                .and_then(|bindings| bindings.into_iter().find_map(|b| b.host_port))
                .ok_or_else(|| format!("{name} has no host port for 6379/tcp"))?;
            let client = redis::Client::open(format!("redis://127.0.0.1:{port}"))
                .map_err(|e| format!("redis client: {e}"))?;

            // Wait (bounded) for Redis to accept commands.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            loop {
                if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
                    let pong: redis::RedisResult<String> =
                        redis::cmd("PING").query_async(&mut conn).await;
                    if pong.is_ok() {
                        break;
                    }
                }
                if tokio::time::Instant::now() > deadline {
                    return Err(format!("{name} did not answer PING within 30s"));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }

            let kv = KvService::with_connection_source(Arc::new(DirectRedis(client)));
            let nx = SetOptions {
                nx: true,
                ..Default::default()
            };
            let xx = SetOptions {
                xx: true,
                ..Default::default()
            };
            let get = |key: &'static str| {
                let kv = &kv;
                async move { kv.get(1, key).await.map_err(|e| format!("get {key}: {e}")) }
            };
            let set = |key: &'static str, value: serde_json::Value, options: SetOptions| {
                let kv = &kv;
                async move {
                    kv.set(1, key, value, options)
                        .await
                        .map_err(|e| format!("set {key}: {e}"))
                }
            };

            // NX on a new key writes.
            assert!(
                set("fresh", json!("first"), nx.clone()).await?,
                "NX on a new key must write"
            );
            assert_eq!(get("fresh").await?, Some(json!("first")));
            // NX on an existing key is rejected and keeps the old value.
            assert!(
                !set("fresh", json!("second"), nx.clone()).await?,
                "NX on an existing key must not write"
            );
            assert_eq!(get("fresh").await?, Some(json!("first")));
            // XX on a missing key is rejected and stores nothing.
            assert!(
                !set("absent", json!("x"), xx.clone()).await?,
                "XX on a missing key must not write"
            );
            assert_eq!(get("absent").await?, None);
            // XX on an existing key writes.
            assert!(
                set("fresh", json!("third"), xx.clone()).await?,
                "XX on an existing key must write"
            );
            assert_eq!(get("fresh").await?, Some(json!("third")));
            // A plain SET always writes.
            assert!(set("plain", json!(null), SetOptions::default()).await?);
            assert_eq!(get("plain").await?, Some(serde_json::Value::Null));
            Ok(())
        }
        .await;

        let _ = docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
        if let Err(e) = outcome {
            panic!("KV NX/XX test failed: {e}");
        }
    }
}
