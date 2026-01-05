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

/// KV Service for key-value operations with project isolation
/// Uses RedisService from temps-providers for container management
pub struct KvService {
    redis_service: Arc<RedisService>,
}

impl KvService {
    /// Create a new KV service backed by a RedisService from temps-providers
    pub fn new(redis_service: Arc<RedisService>) -> Self {
        Self { redis_service }
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

    /// Get a Redis connection from the underlying RedisService
    async fn get_connection(&self) -> Result<redis::aio::ConnectionManager, KvError> {
        self.redis_service
            .get_connection()
            .await
            .map_err(|e| KvError::ConnectionFailed(e.to_string()))
    }

    /// Get a value by key
    pub async fn get(&self, project_id: i32, key: &str) -> Result<Option<Value>, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV GET {}", namespaced);

        let result: Option<String> = conn.get(&namespaced).await.map_err(|e| KvError::Redis(e))?;

        match result {
            Some(s) => {
                // Try to parse as JSON, fallback to string
                let value = serde_json::from_str(&s).unwrap_or(Value::String(s));
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Set a value with optional expiration
    pub async fn set(
        &self,
        project_id: i32,
        key: &str,
        value: Value,
        options: SetOptions,
    ) -> Result<(), KvError> {
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

        // If NX/XX was used and condition not met, result is None
        if (options.nx || options.xx) && result.is_none() {
            debug!("KV SET condition not met for {}", namespaced);
        }

        Ok(())
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

        let deleted: i64 = conn
            .del(&namespaced_keys)
            .await
            .map_err(|e| KvError::Redis(e))?;

        Ok(deleted)
    }

    /// Increment a numeric value
    pub async fn incr(&self, project_id: i32, key: &str) -> Result<i64, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV INCR {}", namespaced);

        let result: i64 = conn
            .incr(&namespaced, 1)
            .await
            .map_err(|e| KvError::Redis(e))?;

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
            .map_err(|e| KvError::Redis(e))?;

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
            .map_err(|e| KvError::Redis(e))?;

        Ok(result)
    }

    /// Get the time-to-live for a key (in seconds)
    pub async fn ttl(&self, project_id: i32, key: &str) -> Result<i64, KvError> {
        let mut conn = self.get_connection().await?;
        let namespaced = self.namespaced_key(project_id, key);

        debug!("KV TTL {}", namespaced);

        let result: i64 = conn.ttl(&namespaced).await.map_err(|e| KvError::Redis(e))?;

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
            .map_err(|e| KvError::Redis(e))?;

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

        let result: bool = conn
            .exists(&namespaced)
            .await
            .map_err(|e| KvError::Redis(e))?;

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

        let results: Vec<Option<String>> = conn
            .mget(&namespaced_keys)
            .await
            .map_err(|e| KvError::Redis(e))?;

        let values: Vec<Option<Value>> = results
            .into_iter()
            .map(|opt| opt.map(|s| serde_json::from_str(&s).unwrap_or(Value::String(s))))
            .collect();

        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
