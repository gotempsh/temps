use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;

/// Strategy for validating and managing parameters for a specific service type
pub trait ParameterStrategy: Send + Sync {
    /// Validate parameters for service creation - ensures all required parameters are present
    fn validate_for_creation(&self, params: &HashMap<String, JsonValue>) -> Result<(), String>;

    /// Auto-generate missing optional parameters (port, docker_image, etc.)
    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String>;

    /// Validate parameters for update - ensures only updateable parameters are being changed
    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String>;

    /// List of parameter keys that can be updated after service creation
    fn updateable_keys(&self) -> Vec<&'static str>;

    /// List of parameter keys that are read-only after service creation
    fn readonly_keys(&self) -> Vec<&'static str>;

    /// Merge updates into existing parameters, rejecting any readonly parameter changes
    fn merge_updates(
        &self,
        existing: &mut HashMap<String, JsonValue>,
        updates: HashMap<String, JsonValue>,
    ) -> Result<(), String>;

    /// Get JSON schema for this service's parameters (for UI validation)
    fn get_schema(&self) -> Option<JsonValue>;

    /// Friendly name for error messages
    fn service_name(&self) -> &'static str;
}

/// PostgreSQL parameter strategy
pub struct PostgresParameterStrategy;

impl ParameterStrategy for PostgresParameterStrategy {
    fn validate_for_creation(&self, params: &HashMap<String, JsonValue>) -> Result<(), String> {
        if !params.contains_key("database") || is_empty_value(params.get("database")) {
            return Err("'database' is required for PostgreSQL".to_string());
        }
        if !params.contains_key("username") || is_empty_value(params.get("username")) {
            return Err("'username' is required for PostgreSQL".to_string());
        }
        if !params.contains_key("password") || is_empty_value(params.get("password")) {
            return Err("'password' is required for PostgreSQL".to_string());
        }
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(5432) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Default docker_image if not provided
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("postgres:17-alpine".to_string()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for PostgreSQL. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "docker_image", "max_connections", "ssl_mode"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["database", "username", "password", "host"]
    }

    fn merge_updates(
        &self,
        existing: &mut HashMap<String, JsonValue>,
        updates: HashMap<String, JsonValue>,
    ) -> Result<(), String> {
        self.validate_for_update(&updates)?;

        for (key, value) in updates {
            existing.insert(key, value);
        }
        Ok(())
    }

    fn get_schema(&self) -> Option<JsonValue> {
        Some(json!({
            "type": "object",
            "title": "PostgreSQL Parameters",
            "required": ["database", "username", "password"],
            "properties": {
                "database": {
                    "type": "string",
                    "description": "Database name (read-only after creation)",
                    "example": "myapp_db"
                },
                "username": {
                    "type": "string",
                    "description": "Database user (read-only after creation)",
                    "example": "postgres"
                },
                "password": {
                    "type": "string",
                    "description": "User password (read-only after creation)",
                    "example": "secure_password"
                },
                "host": {
                    "type": "string",
                    "description": "Host address (read-only after creation)",
                    "default": "localhost"
                },
                "port": {
                    "type": "integer",
                    "description": "Port (updateable)",
                    "default": 5432
                },
                "max_connections": {
                    "type": "integer",
                    "description": "Maximum connections (updateable)",
                    "default": 100
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable, e.g., postgres:17-alpine)",
                    "default": "postgres:17-alpine"
                }
            },
            "readonly": ["database", "username", "password", "host"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "PostgreSQL"
    }
}

/// Redis parameter strategy
pub struct RedisParameterStrategy;

impl ParameterStrategy for RedisParameterStrategy {
    fn validate_for_creation(&self, _params: &HashMap<String, JsonValue>) -> Result<(), String> {
        // Redis doesn't require parameters for creation
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(6379) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Default docker_image if not provided
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("redis:7-alpine".to_string()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for Redis. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "docker_image", "image", "version"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["password"]
    }

    fn merge_updates(
        &self,
        existing: &mut HashMap<String, JsonValue>,
        updates: HashMap<String, JsonValue>,
    ) -> Result<(), String> {
        self.validate_for_update(&updates)?;

        for (key, value) in updates {
            existing.insert(key, value);
        }
        Ok(())
    }

    fn get_schema(&self) -> Option<JsonValue> {
        Some(json!({
            "type": "object",
            "title": "Redis Parameters",
            "properties": {
                "password": {
                    "type": "string",
                    "description": "Redis password (read-only after creation)",
                    "example": "secure_password"
                },
                "port": {
                    "type": "integer",
                    "description": "Port (updateable)",
                    "default": 6379
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable, e.g., redis:7-alpine)",
                    "default": "redis:7-alpine"
                }
            },
            "readonly": ["password"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "Redis"
    }
}

/// S3/MinIO parameter strategy
pub struct S3ParameterStrategy;

impl ParameterStrategy for S3ParameterStrategy {
    fn validate_for_creation(&self, _params: &HashMap<String, JsonValue>) -> Result<(), String> {
        // S3/MinIO doesn't require parameters for creation
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(9000) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Default docker_image if not provided
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("minio/minio:latest".to_string()),
            );
        }

        // Auto-generate access_key if not provided
        if is_empty_value(params.get("access_key")) {
            params.insert(
                "access_key".to_string(),
                JsonValue::String("minioadmin".to_string()),
            );
        }

        // Auto-generate secret_key if not provided
        if is_empty_value(params.get("secret_key")) {
            params.insert(
                "secret_key".to_string(),
                JsonValue::String("minioadmin".to_string()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for S3/MinIO. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "docker_image"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["access_key", "secret_key"]
    }

    fn merge_updates(
        &self,
        existing: &mut HashMap<String, JsonValue>,
        updates: HashMap<String, JsonValue>,
    ) -> Result<(), String> {
        self.validate_for_update(&updates)?;

        for (key, value) in updates {
            existing.insert(key, value);
        }
        Ok(())
    }

    fn get_schema(&self) -> Option<JsonValue> {
        Some(json!({
            "type": "object",
            "title": "S3/MinIO Parameters",
            "properties": {
                "access_key": {
                    "type": "string",
                    "description": "Access key (read-only after creation)",
                    "example": "minioadmin"
                },
                "secret_key": {
                    "type": "string",
                    "description": "Secret key (read-only after creation)",
                    "example": "minioadmin"
                },
                "port": {
                    "type": "integer",
                    "description": "Port (updateable)",
                    "default": 9000
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable, e.g., minio/minio:latest)",
                    "default": "minio/minio:latest"
                }
            },
            "readonly": ["access_key", "secret_key"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "S3/MinIO"
    }
}

/// MongoDB parameter strategy
pub struct MongodbParameterStrategy;

impl ParameterStrategy for MongodbParameterStrategy {
    fn validate_for_creation(&self, params: &HashMap<String, JsonValue>) -> Result<(), String> {
        if !params.contains_key("database") || is_empty_value(params.get("database")) {
            return Err("'database' is required for MongoDB".to_string());
        }
        if !params.contains_key("username") || is_empty_value(params.get("username")) {
            return Err("'username' is required for MongoDB".to_string());
        }
        if !params.contains_key("password") || is_empty_value(params.get("password")) {
            return Err("'password' is required for MongoDB".to_string());
        }
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(27017) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Default docker_image if not provided
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("mongo:latest".to_string()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for MongoDB. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "docker_image", "image", "version"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["database", "username", "password"]
    }

    fn merge_updates(
        &self,
        existing: &mut HashMap<String, JsonValue>,
        updates: HashMap<String, JsonValue>,
    ) -> Result<(), String> {
        self.validate_for_update(&updates)?;

        for (key, value) in updates {
            existing.insert(key, value);
        }
        Ok(())
    }

    fn get_schema(&self) -> Option<JsonValue> {
        Some(json!({
            "type": "object",
            "title": "MongoDB Parameters",
            "required": ["database", "username", "password"],
            "properties": {
                "database": {
                    "type": "string",
                    "description": "Database name (read-only after creation)",
                    "example": "myapp_db"
                },
                "username": {
                    "type": "string",
                    "description": "Database user (read-only after creation)",
                    "example": "mongoadmin"
                },
                "password": {
                    "type": "string",
                    "description": "User password (read-only after creation)",
                    "example": "secure_password"
                },
                "port": {
                    "type": "integer",
                    "description": "Port (updateable)",
                    "default": 27017
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable, e.g., mongo:latest)",
                    "default": "mongo:latest"
                }
            },
            "readonly": ["database", "username", "password"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "MongoDB"
    }
}

/// Helper: Get strategy for a service type
pub fn get_strategy(service_type: &str) -> Option<Box<dyn ParameterStrategy>> {
    match service_type {
        "postgres" => Some(Box::new(PostgresParameterStrategy)),
        "redis" => Some(Box::new(RedisParameterStrategy)),
        "s3" => Some(Box::new(S3ParameterStrategy)),
        "mongodb" => Some(Box::new(MongodbParameterStrategy)),
        _ => None,
    }
}

// ============= Helper Functions =============

fn is_empty_value(value: Option<&JsonValue>) -> bool {
    match value {
        None => true,
        Some(JsonValue::Null) => true,
        Some(JsonValue::String(s)) => s.is_empty(),
        _ => false,
    }
}

fn find_available_port(start_port: u16) -> Option<u16> {
    use std::net::TcpListener;
    (start_port..start_port + 100).find(|&port| TcpListener::bind(("0.0.0.0", port)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_postgres_readonly_parameters() {
        let strategy = PostgresParameterStrategy;
        assert!(strategy.readonly_keys().contains(&"database"));
        assert!(strategy.readonly_keys().contains(&"username"));
        assert!(strategy.readonly_keys().contains(&"password"));
        assert!(strategy.readonly_keys().contains(&"host"));
    }

    #[test]
    fn test_postgres_updateable_parameters() {
        let strategy = PostgresParameterStrategy;
        assert!(strategy.updateable_keys().contains(&"docker_image"));
        assert!(strategy.updateable_keys().contains(&"port"));
        assert!(strategy.updateable_keys().contains(&"max_connections"));
        assert!(strategy.updateable_keys().contains(&"ssl_mode"));
    }

    #[test]
    fn test_postgres_rejects_readonly_update() {
        let strategy = PostgresParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "username".to_string(),
            JsonValue::String("newuser".to_string()),
        );

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Cannot update parameter 'username'"));
    }

    #[test]
    fn test_postgres_allows_updateable_parameters() {
        let strategy = PostgresParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:17-alpine".to_string()),
        );
        updates.insert("port".to_string(), JsonValue::String("5433".to_string()));

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_ok());
    }

    #[test]
    fn test_redis_readonly_password() {
        let strategy = RedisParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "password".to_string(),
            JsonValue::String("newpass".to_string()),
        );

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_redis_updateable_image_version() {
        let strategy = RedisParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "image".to_string(),
            JsonValue::String("redis:8-alpine".to_string()),
        );
        updates.insert("version".to_string(), JsonValue::String("8".to_string()));

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mongodb_updateable_image_version() {
        let strategy = MongodbParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "image".to_string(),
            JsonValue::String("mongo:9".to_string()),
        );
        updates.insert("version".to_string(), JsonValue::String("9".to_string()));

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mongodb_validation_requires_database() {
        let strategy = MongodbParameterStrategy;
        let params = HashMap::new();

        let result = strategy.validate_for_creation(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("database"));
    }

    #[test]
    fn test_merge_updates_rejects_readonly() {
        let strategy = PostgresParameterStrategy;
        let mut existing = HashMap::new();
        existing.insert(
            "database".to_string(),
            JsonValue::String("mydb".to_string()),
        );
        existing.insert(
            "username".to_string(),
            JsonValue::String("user".to_string()),
        );

        let mut updates = HashMap::new();
        updates.insert(
            "username".to_string(),
            JsonValue::String("newuser".to_string()),
        );

        let result = strategy.merge_updates(&mut existing, updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_updates_allows_updateable() {
        let strategy = PostgresParameterStrategy;
        let mut existing = HashMap::new();
        existing.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:16-alpine".to_string()),
        );

        let mut updates = HashMap::new();
        updates.insert(
            "docker_image".to_string(),
            JsonValue::String("postgres:17-alpine".to_string()),
        );

        let result = strategy.merge_updates(&mut existing, updates);
        assert!(result.is_ok());
        assert_eq!(
            existing.get("docker_image").and_then(|v| v.as_str()),
            Some("postgres:17-alpine")
        );
    }
}
