use crate::error::{DataError, Result};
use crate::traits::DataSource;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Connection configuration for creating data sources
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// Backend type identifier (postgres, mongodb, redis, s3, etc.)
    pub backend: String,
    /// Host or connection endpoint
    pub host: Option<String>,
    /// Port number
    pub port: Option<u16>,
    /// Username or access key
    pub username: Option<String>,
    /// Password or secret key
    pub password: Option<String>,
    /// Database name or bucket
    pub database: Option<String>,
    /// Additional options as key-value pairs
    pub options: HashMap<String, String>,
}

impl ConnectionConfig {
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            host: None,
            port: None,
            username: None,
            password: None,
            database: None,
            options: HashMap::new(),
        }
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn with_username(mut self, username: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    pub fn with_database(mut self, database: impl Into<String>) -> Self {
        self.database = Some(database.into());
        self
    }

    pub fn with_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.insert(key.into(), value.into());
        self
    }

    /// Get connection string for display purposes (without password)
    pub fn connection_string(&self) -> String {
        let mut parts = vec![self.backend.clone()];

        if let Some(username) = &self.username {
            parts.push(format!("{}@", username));
        }

        if let Some(host) = &self.host {
            parts.push(host.clone());

            if let Some(port) = self.port {
                parts.push(format!(":{}", port));
            }
        }

        if let Some(database) = &self.database {
            parts.push(format!("/{}", database));
        }

        parts.join("")
    }
}

/// Factory trait for creating data sources from configurations
pub trait DataSourceFactory: Send + Sync {
    /// Get the backend type this factory handles
    fn backend_type(&self) -> &'static str;

    /// Create a data source from configuration
    fn create_source(&self, config: ConnectionConfig) -> Result<Arc<dyn DataSource>>;
}

/// Registry for managing data sources and their factories
pub struct QueryRegistry {
    factories: Arc<RwLock<HashMap<String, Arc<dyn DataSourceFactory>>>>,
    sources: Arc<RwLock<HashMap<String, Arc<dyn DataSource>>>>,
}

impl QueryRegistry {
    pub fn new() -> Self {
        Self {
            factories: Arc::new(RwLock::new(HashMap::new())),
            sources: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a factory for a backend type
    pub async fn register_factory(&self, factory: Arc<dyn DataSourceFactory>) -> Result<()> {
        let backend = factory.backend_type();
        let mut factories = self.factories.write().await;

        if factories.contains_key(backend) {
            warn!("Overwriting existing factory for backend: {}", backend);
        }

        factories.insert(backend.to_string(), factory);
        debug!("Registered factory for backend: {}", backend);
        Ok(())
    }

    /// Create a new data source connection
    pub async fn create_source(
        &self,
        source_id: &str,
        config: ConnectionConfig,
    ) -> Result<Arc<dyn DataSource>> {
        let factories = self.factories.read().await;

        let factory = factories
            .get(&config.backend)
            .ok_or_else(|| {
                DataError::InvalidConfiguration(format!(
                    "No factory registered for backend: {}",
                    config.backend
                ))
            })?
            .clone();

        drop(factories);

        debug!(
            "Creating source {} for backend: {}",
            source_id, config.backend
        );

        let source = factory.create_source(config)?;

        // Cache the source
        let mut sources = self.sources.write().await;
        sources.insert(source_id.to_string(), source.clone());

        Ok(source)
    }

    /// Get a cached data source
    pub async fn get_source(&self, source_id: &str) -> Result<Option<Arc<dyn DataSource>>> {
        let sources = self.sources.read().await;
        Ok(sources.get(source_id).cloned())
    }

    /// Remove a cached source
    pub async fn remove_source(&self, source_id: &str) -> Result<()> {
        let mut sources = self.sources.write().await;

        if let Some(source) = sources.remove(source_id) {
            debug!("Closing source: {}", source_id);
            source.close().await?;
        }

        Ok(())
    }

    /// List all cached sources
    pub async fn list_sources(&self) -> Result<Vec<String>> {
        let sources = self.sources.read().await;
        Ok(sources.keys().cloned().collect())
    }

    /// Clear all cached sources
    pub async fn clear_sources(&self) -> Result<()> {
        let mut sources = self.sources.write().await;

        for (_, source) in sources.drain() {
            let _ = source.close().await;
        }

        Ok(())
    }

    /// List registered backend types
    pub async fn list_backends(&self) -> Result<Vec<String>> {
        let factories = self.factories.read().await;
        Ok(factories.keys().cloned().collect())
    }

    /// Check if a backend is registered
    pub async fn has_backend(&self, backend: &str) -> Result<bool> {
        let factories = self.factories.read().await;
        Ok(factories.contains_key(backend))
    }
}

impl Default for QueryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_config_creation() {
        let config = ConnectionConfig::new("postgres")
            .with_host("localhost")
            .with_port(5432)
            .with_database("mydb");

        assert_eq!(config.backend, "postgres");
        assert_eq!(config.host, Some("localhost".to_string()));
        assert_eq!(config.port, Some(5432));
        assert_eq!(config.database, Some("mydb".to_string()));
    }

    #[test]
    fn test_connection_string() {
        let config = ConnectionConfig::new("postgres")
            .with_host("localhost")
            .with_port(5432)
            .with_database("mydb");

        let conn_str = config.connection_string();
        assert!(conn_str.contains("postgres"));
        assert!(conn_str.contains("localhost"));
        assert!(conn_str.contains("5432"));
        assert!(conn_str.contains("mydb"));
    }

    #[tokio::test]
    async fn test_registry_creation() {
        let registry = QueryRegistry::new();
        let sources = registry.list_sources().await.unwrap();
        assert!(sources.is_empty());
    }
}
