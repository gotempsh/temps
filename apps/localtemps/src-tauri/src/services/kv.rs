//! KV Service for LocalTemps
//!
//! Provides key-value operations on top of Redis with project isolation.

use anyhow::Result;
use redis::AsyncCommands;
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;

use super::RedisService;

/// Options for SET operations
#[derive(Debug, Clone, Default)]
pub struct SetOptions {
    /// Expiration in seconds
    pub ex: Option<i64>,
    /// Expiration in milliseconds
    pub px: Option<i64>,
    /// Only set if key doesn't exist
    pub nx: bool,
    /// Only set if key exists
    pub xx: bool,
}

/// KV Service - provides key-value operations with project isolation
pub struct KvService {
    redis: Arc<RedisService>,
}

impl KvService {
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }

    /// Get project-scoped key
    fn scoped_key(&self, project_id: i32, key: &str) -> String {
        format!("project:{}:{}", project_id, key)
    }

    /// Get a value by key
    pub async fn get(&self, project_id: i32, key: &str) -> Result<Option<Value>> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_key = self.scoped_key(project_id, key);

        let value: Option<String> = conn.get(&scoped_key).await?;

        match value {
            Some(v) => {
                let parsed: Value = serde_json::from_str(&v)?;
                Ok(Some(parsed))
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
    ) -> Result<()> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_key = self.scoped_key(project_id, key);
        let serialized = serde_json::to_string(&value)?;

        // Handle NX/XX options
        if options.nx {
            let exists: bool = conn.exists(&scoped_key).await?;
            if exists {
                return Ok(()); // Key exists, NX means don't set
            }
        }
        if options.xx {
            let exists: bool = conn.exists(&scoped_key).await?;
            if !exists {
                return Ok(()); // Key doesn't exist, XX means don't set
            }
        }

        // Set the value
        let _: () = conn.set(&scoped_key, &serialized).await?;

        // Handle expiration
        if let Some(ex) = options.ex {
            let _: () = conn.expire(&scoped_key, ex).await?;
        } else if let Some(px) = options.px {
            let _: () = conn.pexpire(&scoped_key, px).await?;
        }

        debug!("KV SET {} = {} (project {})", key, serialized, project_id);
        Ok(())
    }

    /// Delete keys
    pub async fn del(&self, project_id: i32, keys: Vec<String>) -> Result<i64> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_keys: Vec<String> = keys
            .iter()
            .map(|k| self.scoped_key(project_id, k))
            .collect();

        let deleted: i64 = conn.del(&scoped_keys[..]).await?;
        debug!(
            "KV DEL {:?} = {} deleted (project {})",
            keys, deleted, project_id
        );
        Ok(deleted)
    }

    /// Increment a numeric value
    pub async fn incr(&self, project_id: i32, key: &str) -> Result<i64> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_key = self.scoped_key(project_id, key);

        let value: i64 = conn.incr(&scoped_key, 1).await?;
        debug!("KV INCR {} = {} (project {})", key, value, project_id);
        Ok(value)
    }

    /// Set expiration on a key
    pub async fn expire(&self, project_id: i32, key: &str, seconds: i64) -> Result<bool> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_key = self.scoped_key(project_id, key);

        let result: bool = conn.expire(&scoped_key, seconds).await?;
        debug!(
            "KV EXPIRE {} = {} seconds, result={} (project {})",
            key, seconds, result, project_id
        );
        Ok(result)
    }

    /// Get time to live for a key
    pub async fn ttl(&self, project_id: i32, key: &str) -> Result<i64> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_key = self.scoped_key(project_id, key);

        let ttl: i64 = conn.ttl(&scoped_key).await?;
        debug!("KV TTL {} = {} (project {})", key, ttl, project_id);
        Ok(ttl)
    }

    /// Find keys matching a pattern
    pub async fn keys(&self, project_id: i32, pattern: &str) -> Result<Vec<String>> {
        let mut conn = self.redis.get_connection().await?;
        let scoped_pattern = self.scoped_key(project_id, pattern);

        let keys: Vec<String> = conn.keys(&scoped_pattern).await?;

        // Remove the project prefix from returned keys
        let prefix = format!("project:{}:", project_id);
        let cleaned: Vec<String> = keys
            .iter()
            .map(|k| k.strip_prefix(&prefix).unwrap_or(k).to_string())
            .collect();

        debug!(
            "KV KEYS {} = {:?} (project {})",
            pattern, cleaned, project_id
        );
        Ok(cleaned)
    }
}
