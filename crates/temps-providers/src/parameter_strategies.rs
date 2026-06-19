use crate::externalsvc::{mariadb::MariaDbSizeProfile, ServiceResourceLimits};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────
// Credential validators (defense in depth)
//
// These guard the boundary between user-supplied input and the bash
// scripts + psql `-d`/`-U` invocations that postgres_cluster.rs and
// services.rs build. The actual scripts use parameter binding where
// they can, but two paths are unsafe by design:
//
//   1. `node_command` interpolates `${POSTGRES_USER}` and
//      `${POSTGRES_PASSWORD}` into a SQL heredoc via shell variable
//      expansion. A password containing `'` breaks the SQL; a crafted
//      password ("'; ALTER ROLE postgres SUPERUSER PASSWORD 'pwn'; --")
//      injects DDL.
//   2. `enable_cluster_wal_archiving` calls `psql -d <database>`.
//      libpq treats `-d` values containing `=` as a connstring (not a
//      database name), so `database = "host=evil.com user=postgres"`
//      is a connection redirect.
//
// The cleanest fix would be parameter binding everywhere. That's a
// bigger refactor; this validator is the wide net that catches the
// payloads that exploit those holes today.
// ─────────────────────────────────────────────────────────────────────

/// Match a Postgres SQL identifier: starts with letter/underscore, then
/// letters/digits/underscores, max 63 bytes (Postgres `NAMEDATALEN-1`).
/// Deliberately stricter than what Postgres accepts (no quoting, no
/// dots, no `=`) so we never have to worry about libpq parsing the
/// value as a connstring or the bash scripts breaking on edge cases.
fn is_valid_pg_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reject characters that break the bash heredoc + SQL literal that
/// `node_command` builds:
///   - `'` and `\\` would terminate the SQL string literal early
///   - `\0` is rejected by libpq anyway and corrupts shell strings
///   - newlines could close a heredoc on certain inputs
///   - `$` triggers shell expansion inside the bash script
///
/// We do allow most printable special characters so users can pick
/// strong passwords (e.g. `!@#%^&*()_+-=[]{}|:,./?`). Auto-generated
/// passwords stay base64url-safe so they pass trivially.
fn is_valid_pg_password(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("password cannot be empty".to_string());
    }
    if s.len() > 256 {
        return Err("password too long (max 256 characters)".to_string());
    }
    for (i, c) in s.chars().enumerate() {
        match c {
            '\'' => return Err(format!("password contains a single quote at position {} — choose a password without ' or \\", i)),
            '\\' => return Err(format!("password contains a backslash at position {} — choose a password without ' or \\", i)),
            '\0' => return Err("password contains a null byte".to_string()),
            '\n' | '\r' => return Err("password contains a newline".to_string()),
            '$' => return Err(format!("password contains '$' at position {} — disallowed because the cluster startup script uses shell expansion", i)),
            c if c.is_control() => return Err(format!("password contains control character (U+{:04X}) at position {}", c as u32, i)),
            _ => {}
        }
    }
    Ok(())
}

/// Strict validator for the `username` and `database` params on
/// Postgres services (standalone and cluster). See module-level
/// rationale.
fn validate_postgres_credentials(params: &HashMap<String, JsonValue>) -> Result<(), String> {
    if let Some(JsonValue::String(user)) = params.get("username") {
        if !is_valid_pg_identifier(user) {
            return Err(format!(
                "invalid 'username' {:?}: must match ^[A-Za-z_][A-Za-z0-9_]{{0,62}}$ (Postgres identifier rules; we deliberately reject quoted/dotted names to keep the cluster startup script and psql -U safe)",
                user
            ));
        }
    }
    if let Some(JsonValue::String(db)) = params.get("database") {
        if !is_valid_pg_identifier(db) {
            return Err(format!(
                "invalid 'database' {:?}: must match ^[A-Za-z_][A-Za-z0-9_]{{0,62}}$ — values containing '=' would be parsed by libpq as a connstring, redirecting psql to a different host",
                db
            ));
        }
    }
    if let Some(JsonValue::String(pw)) = params.get("password") {
        // Empty string is a sentinel meaning "auto-generate me later" —
        // the `auto_generate_missing` step (which runs immediately after
        // validation) fills it with a `generate_secure_password()` value.
        // Treating empty as a validation failure here would block the
        // auto-generate flow that both the UI and the schema documentation
        // explicitly promise users.
        if !pw.is_empty() {
            is_valid_pg_password(pw).map_err(|reason| format!("invalid 'password': {}", reason))?;
        }
    }
    Ok(())
}

fn is_valid_mariadb_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_mariadb_password(label: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Ok(());
    }
    if s.len() < 8 {
        return Err(format!("{} must be at least 8 characters", label));
    }
    if s.len() > 256 {
        return Err(format!("{} too long (max 256 characters)", label));
    }
    for (i, c) in s.chars().enumerate() {
        match c {
            '\'' => {
                return Err(format!(
                    "{} contains a single quote at position {}",
                    label, i
                ))
            }
            '\\' => return Err(format!("{} contains a backslash at position {}", label, i)),
            '\0' => return Err(format!("{} contains a null byte", label)),
            '\n' | '\r' => return Err(format!("{} contains a newline", label)),
            c if c.is_control() => {
                return Err(format!(
                    "{} contains control character (U+{:04X}) at position {}",
                    label, c as u32, i
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_mariadb_credentials(params: &HashMap<String, JsonValue>) -> Result<(), String> {
    if let Some(JsonValue::String(user)) = params.get("username") {
        if !user.is_empty() && !is_valid_mariadb_identifier(user) {
            return Err(format!(
                "invalid 'username' {:?}: must match ^[A-Za-z_][A-Za-z0-9_]{{0,62}}$",
                user
            ));
        }
    }
    if let Some(JsonValue::String(db)) = params.get("database") {
        if !db.is_empty() && !is_valid_mariadb_identifier(db) {
            return Err(format!(
                "invalid 'database' {:?}: must match ^[A-Za-z_][A-Za-z0-9_]{{0,62}}$",
                db
            ));
        }
    }
    if let Some(JsonValue::String(pw)) = params.get("password") {
        validate_mariadb_password("password", pw)?;
    }
    if let Some(JsonValue::String(pw)) = params.get("root_password") {
        validate_mariadb_password("root_password", pw)?;
    }
    Ok(())
}

fn mariadb_size_profile_from_params(
    params: &HashMap<String, JsonValue>,
) -> Result<MariaDbSizeProfile, String> {
    match params.get("size_profile") {
        value if is_empty_value(value) => Ok(MariaDbSizeProfile::Small),
        Some(JsonValue::String(profile)) => MariaDbSizeProfile::parse(profile).ok_or_else(|| {
            format!(
                "invalid 'size_profile' {:?}: expected one of small, standard, dedicated",
                profile
            )
        }),
        Some(other) => Err(format!(
            "invalid 'size_profile' {:?}: expected a string",
            other
        )),
        None => Ok(MariaDbSizeProfile::Small),
    }
}

fn validate_service_resource_limits(params: &HashMap<String, JsonValue>) -> Result<(), String> {
    let Some(resources) = params.get("resources") else {
        return Ok(());
    };
    if resources.is_null() {
        return Ok(());
    }
    let limits: ServiceResourceLimits = serde_json::from_value(resources.clone())
        .map_err(|e| format!("invalid 'resources' block: {}", e))?;
    limits
        .validate()
        .map_err(|e| format!("invalid 'resources' block: {}", e))
}

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
        // Password is optional - will be auto-generated if not provided.
        // When supplied, validate it can't break the cluster startup
        // script or be parsed as a libpq connstring (see module docs).
        validate_postgres_credentials(params)?;
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
                JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
            );
        }

        // Auto-generate password if not provided
        if is_empty_value(params.get("password")) {
            params.insert(
                "password".to_string(),
                JsonValue::String(generate_secure_password()),
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
            "required": ["database", "username"],
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
                    "description": "User password (read-only after creation, auto-generated if not provided)",
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
                    "description": "Docker image (updateable, e.g., gotempsh/postgres-walg:18-bookworm)",
                    "default": "gotempsh/postgres-walg:18-bookworm"
                }
            },
            "readonly": ["database", "username", "password", "host"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "PostgreSQL"
    }
}

/// MariaDB parameter strategy
pub struct MariaDbParameterStrategy;

impl ParameterStrategy for MariaDbParameterStrategy {
    fn validate_for_creation(&self, params: &HashMap<String, JsonValue>) -> Result<(), String> {
        validate_mariadb_credentials(params)?;
        mariadb_size_profile_from_params(params)?;
        validate_service_resource_limits(params)?;
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        if is_empty_value(params.get("host")) {
            params.insert(
                "host".to_string(),
                JsonValue::String("localhost".to_string()),
            );
        }

        if is_empty_value(params.get("database")) {
            params.insert("database".to_string(), JsonValue::String("app".to_string()));
        }

        if is_empty_value(params.get("username")) {
            params.insert("username".to_string(), JsonValue::String("app".to_string()));
        }

        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(3306) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("mariadb:lts".to_string()),
            );
        }

        let size_profile = mariadb_size_profile_from_params(params)?;
        if is_empty_value(params.get("size_profile")) {
            params.insert(
                "size_profile".to_string(),
                JsonValue::String(size_profile.as_str().to_string()),
            );
        }

        if is_empty_value(params.get("resources")) {
            let resources = serde_json::to_value(size_profile.default_resource_limits())
                .map_err(|e| format!("failed to serialize MariaDB default resources: {}", e))?;
            params.insert("resources".to_string(), resources);
        }

        if is_empty_value(params.get("password")) {
            params.insert(
                "password".to_string(),
                JsonValue::String(generate_secure_password()),
            );
        }

        if is_empty_value(params.get("root_password")) {
            params.insert(
                "root_password".to_string(),
                JsonValue::String(generate_secure_password()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for MariaDB. Read-only parameters: {}. Updateable parameters: {}",
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
        vec![
            "host",
            "database",
            "username",
            "password",
            "root_password",
            "size_profile",
            "resources",
        ]
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
            "title": "MariaDB Parameters",
            "properties": {
                "database": {
                    "type": "string",
                    "description": "Initial database name (read-only after creation)",
                    "default": "app"
                },
                "username": {
                    "type": "string",
                    "description": "Application database user (read-only after creation)",
                    "default": "app"
                },
                "password": {
                    "type": "string",
                    "description": "Application user password (read-only after creation, auto-generated if not provided)",
                    "example": "secure_password"
                },
                "root_password": {
                    "type": "string",
                    "description": "Root password used by Temps for provisioning (read-only after creation, auto-generated if not provided)",
                    "example": "secure_root_password"
                },
                "host": {
                    "type": "string",
                    "description": "Host address (read-only after creation)",
                    "default": "localhost"
                },
                "port": {
                    "type": "integer",
                    "description": "Port (updateable)",
                    "default": 3306
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable, e.g., mariadb:lts)",
                    "default": "mariadb:lts"
                },
                "size_profile": {
                    "type": "string",
                    "description": "Managed MariaDB resource/tuning profile. A MariaDB service is shared; linked projects get separate databases inside it.",
                    "default": "small",
                    "enum": ["small", "standard", "dedicated"]
                }
            },
            "readonly": ["host", "database", "username", "password", "root_password", "size_profile", "resources"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "MariaDB"
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
                JsonValue::String("gotempsh/redis-walg:8-bookworm".to_string()),
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
        vec!["port", "docker_image"]
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
                    "description": "Docker image (updateable, e.g., gotempsh/redis-walg:8-bookworm)",
                    "default": "gotempsh/redis-walg:8-bookworm"
                }
            },
            "readonly": ["password"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "Redis"
    }
}

/// S3 parameter strategy (RustFS-backed by default)
pub struct S3ParameterStrategy;

impl ParameterStrategy for S3ParameterStrategy {
    fn validate_for_creation(&self, _params: &HashMap<String, JsonValue>) -> Result<(), String> {
        // S3/RustFS doesn't require parameters for creation
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(9000) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Auto-assign console_port if not provided
        // IMPORTANT: Start search AFTER the API port to avoid assigning the same port
        if is_empty_value(params.get("console_port")) {
            let api_port: u16 = params
                .get("port")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(9000);
            // Start searching from max(api_port + 1, 9001) to ensure different port
            let console_start = std::cmp::max(api_port + 1, 9001);
            if let Some(port) = find_available_port(console_start) {
                params.insert(
                    "console_port".to_string(),
                    JsonValue::String(port.to_string()),
                );
            }
        }

        // Default docker_image if not provided (RustFS - high-performance S3-compatible storage)
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("rustfs/rustfs:1.0.0-alpha.98".to_string()),
            );
        }

        // Default host if not provided
        if is_empty_value(params.get("host")) {
            params.insert(
                "host".to_string(),
                JsonValue::String("localhost".to_string()),
            );
        }

        // Default region if not provided
        if is_empty_value(params.get("region")) {
            params.insert(
                "region".to_string(),
                JsonValue::String("us-east-1".to_string()),
            );
        }

        // Auto-generate access_key if not provided
        if is_empty_value(params.get("access_key")) {
            params.insert(
                "access_key".to_string(),
                JsonValue::String(generate_access_key()),
            );
        }

        // Auto-generate secret_key if not provided
        if is_empty_value(params.get("secret_key")) {
            params.insert(
                "secret_key".to_string(),
                JsonValue::String(generate_secret_key()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for S3. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "console_port", "docker_image"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["access_key", "secret_key", "host", "region"]
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
            "title": "S3 Parameters",
            "properties": {
                "access_key": {
                    "type": "string",
                    "description": "Access key (read-only after creation, auto-generated)",
                    "example": "AKIAIOSFODNN7EXAMPLE"
                },
                "secret_key": {
                    "type": "string",
                    "description": "Secret key (read-only after creation, auto-generated)",
                    "example": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
                },
                "host": {
                    "type": "string",
                    "description": "Host address (read-only after creation)",
                    "default": "localhost"
                },
                "region": {
                    "type": "string",
                    "description": "S3 region (read-only after creation)",
                    "default": "us-east-1"
                },
                "port": {
                    "type": "integer",
                    "description": "API port (updateable)",
                    "default": 9000
                },
                "console_port": {
                    "type": "integer",
                    "description": "Console port (updateable)",
                    "default": 9001
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable)",
                    "default": "rustfs/rustfs:1.0.0-alpha.98"
                }
            },
            "readonly": ["access_key", "secret_key", "host", "region"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "S3"
    }
}

/// MinIO parameter strategy (deprecated - kept for backward compatibility)
pub struct MinioParameterStrategy;

impl ParameterStrategy for MinioParameterStrategy {
    fn validate_for_creation(&self, _params: &HashMap<String, JsonValue>) -> Result<(), String> {
        // MinIO doesn't require parameters for creation
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(9000) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Default docker_image if not provided (pinned to specific version for reproducibility)
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("minio/minio:RELEASE.2025-09-07T16-13-09Z".to_string()),
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
                    "Cannot update parameter '{}' for MinIO. Read-only parameters: {}. Updateable parameters: {}",
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
            "title": "MinIO Parameters (Deprecated)",
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
                    "description": "Docker image (updateable)",
                    "default": "minio/minio:RELEASE.2025-09-07T16-13-09Z"
                }
            },
            "readonly": ["access_key", "secret_key"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "MinIO (Deprecated)"
    }
}

/// RustFS/Blob parameter strategy (high-performance S3-compatible storage)
pub struct RustfsParameterStrategy;

impl ParameterStrategy for RustfsParameterStrategy {
    fn validate_for_creation(&self, _params: &HashMap<String, JsonValue>) -> Result<(), String> {
        // RustFS doesn't require parameters for creation
        Ok(())
    }

    fn auto_generate_missing(&self, params: &mut HashMap<String, JsonValue>) -> Result<(), String> {
        // Auto-assign port if not provided
        if is_empty_value(params.get("port")) {
            if let Some(port) = find_available_port(9000) {
                params.insert("port".to_string(), JsonValue::String(port.to_string()));
            }
        }

        // Auto-assign console_port if not provided
        // IMPORTANT: Start search AFTER the API port to avoid assigning the same port
        if is_empty_value(params.get("console_port")) {
            let api_port: u16 = params
                .get("port")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(9000);
            // Start searching from max(api_port + 1, 9001) to ensure different port
            let console_start = std::cmp::max(api_port + 1, 9001);
            if let Some(port) = find_available_port(console_start) {
                params.insert(
                    "console_port".to_string(),
                    JsonValue::String(port.to_string()),
                );
            }
        }

        // Default docker_image if not provided
        if is_empty_value(params.get("docker_image")) {
            params.insert(
                "docker_image".to_string(),
                JsonValue::String("rustfs/rustfs:1.0.0-alpha.98".to_string()),
            );
        }

        // Default host if not provided
        if is_empty_value(params.get("host")) {
            params.insert(
                "host".to_string(),
                JsonValue::String("localhost".to_string()),
            );
        }

        // Default region if not provided
        if is_empty_value(params.get("region")) {
            params.insert(
                "region".to_string(),
                JsonValue::String("us-east-1".to_string()),
            );
        }

        // Auto-generate access_key if not provided
        if is_empty_value(params.get("access_key")) {
            params.insert(
                "access_key".to_string(),
                JsonValue::String(generate_access_key()),
            );
        }

        // Auto-generate secret_key if not provided
        if is_empty_value(params.get("secret_key")) {
            params.insert(
                "secret_key".to_string(),
                JsonValue::String(generate_secret_key()),
            );
        }

        Ok(())
    }

    fn validate_for_update(&self, updates: &HashMap<String, JsonValue>) -> Result<(), String> {
        for key in updates.keys() {
            if !self.updateable_keys().contains(&key.as_str()) {
                return Err(format!(
                    "Cannot update parameter '{}' for RustFS/Blob. Read-only parameters: {}. Updateable parameters: {}",
                    key,
                    self.readonly_keys().join(", "),
                    self.updateable_keys().join(", ")
                ));
            }
        }
        Ok(())
    }

    fn updateable_keys(&self) -> Vec<&'static str> {
        vec!["port", "console_port", "docker_image"]
    }

    fn readonly_keys(&self) -> Vec<&'static str> {
        vec!["access_key", "secret_key", "host", "region"]
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
            "title": "RustFS/Blob Parameters",
            "properties": {
                "access_key": {
                    "type": "string",
                    "description": "Access key (read-only after creation, auto-generated)",
                    "example": "AKIAIOSFODNN7EXAMPLE"
                },
                "secret_key": {
                    "type": "string",
                    "description": "Secret key (read-only after creation, auto-generated)",
                    "example": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
                },
                "host": {
                    "type": "string",
                    "description": "Host address (read-only after creation)",
                    "default": "localhost"
                },
                "region": {
                    "type": "string",
                    "description": "S3 region (read-only after creation)",
                    "default": "us-east-1"
                },
                "port": {
                    "type": "integer",
                    "description": "API port (updateable)",
                    "default": 9000
                },
                "console_port": {
                    "type": "integer",
                    "description": "Console port (updateable)",
                    "default": 9001
                },
                "docker_image": {
                    "type": "string",
                    "description": "Docker image (updateable)",
                    "default": "rustfs/rustfs:1.0.0-alpha.98"
                }
            },
            "readonly": ["access_key", "secret_key", "host", "region"]
        }))
    }

    fn service_name(&self) -> &'static str {
        "RustFS/Blob"
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
        // Password is optional - will be auto-generated if not provided
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

        // Auto-generate password if not provided
        if is_empty_value(params.get("password")) {
            params.insert(
                "password".to_string(),
                JsonValue::String(generate_secure_password()),
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
        vec!["port", "docker_image", "replica_set"]
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

        // replica_set is one-way: a standalone (None or empty) can be promoted
        // to a replica set, but unsetting or renaming an existing one would
        // strand the keyfile and orphan the local.system.replset config.
        if let Some(new_rs) = updates.get("replica_set") {
            let new_name = new_rs.as_str().unwrap_or("").trim();
            let existing_name = existing
                .get("replica_set")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if !existing_name.is_empty() && new_name != existing_name {
                return Err(format!(
                    "Cannot change 'replica_set' from '{}' to '{}': renaming or unsetting an existing replica set is not supported",
                    existing_name, new_name
                ));
            }
        }

        for (key, value) in updates {
            existing.insert(key, value);
        }
        Ok(())
    }

    fn get_schema(&self) -> Option<JsonValue> {
        Some(json!({
            "type": "object",
            "title": "MongoDB Parameters",
            "required": ["database", "username"],
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
                    "description": "User password (read-only after creation, auto-generated if not provided)",
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
        "mariadb" => Some(Box::new(MariaDbParameterStrategy)),
        "postgres" => Some(Box::new(PostgresParameterStrategy)),
        "redis" => Some(Box::new(RedisParameterStrategy)),
        // S3 now uses RustFS by default
        "s3" => Some(Box::new(S3ParameterStrategy)),
        "mongodb" => Some(Box::new(MongodbParameterStrategy)),
        // RustFS is used for both standalone rustfs and temps blob service
        "rustfs" | "blob" => Some(Box::new(RustfsParameterStrategy)),
        // KV service uses Redis backend
        "kv" => Some(Box::new(RedisParameterStrategy)),
        // MinIO is deprecated, kept for backward compatibility with existing services
        "minio" => Some(Box::new(MinioParameterStrategy)),
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

use crate::externalsvc::port_util::find_available_port;

fn generate_secure_password() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    // Charset must be a subset of what `is_valid_pg_password` accepts.
    // `$` is intentionally excluded because the cluster startup script
    // uses shell expansion on env-injected passwords — see the matching
    // rule in `is_valid_pg_password`. Including it caused ~35% of
    // auto-generated passwords to fail their own validation.
    let charset: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#%^&*_-+=";
    (0..32)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// Generate an S3-style access key (20 uppercase alphanumeric characters)
fn generate_access_key() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let charset: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..20)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// Generate an S3-style secret key (40 alphanumeric characters with special chars)
fn generate_secret_key() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let charset: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/";
    (0..40)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
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
            JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
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
    fn test_redis_updateable_docker_image() {
        let strategy = RedisParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "docker_image".to_string(),
            JsonValue::String("gotempsh/redis-walg:8-bookworm".to_string()),
        );

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mariadb_generates_defaults() {
        let strategy = MariaDbParameterStrategy;
        let mut params = HashMap::new();

        strategy
            .validate_for_creation(&params)
            .expect("empty MariaDB params should use defaults");
        strategy
            .auto_generate_missing(&mut params)
            .expect("defaults should generate");

        assert_eq!(
            params.get("database"),
            Some(&JsonValue::String("app".to_string()))
        );
        assert_eq!(
            params.get("username"),
            Some(&JsonValue::String("app".to_string()))
        );
        assert_eq!(
            params.get("docker_image"),
            Some(&JsonValue::String("mariadb:lts".to_string()))
        );
        assert_eq!(
            params.get("size_profile"),
            Some(&JsonValue::String("small".to_string()))
        );
        let resources: ServiceResourceLimits = serde_json::from_value(
            params
                .get("resources")
                .expect("MariaDB defaults should include resource limits")
                .clone(),
        )
        .expect("default MariaDB resources should deserialize");
        assert_eq!(resources.memory_mb, Some(512));
        assert_eq!(resources.memory_swap_mb, Some(768));
        assert_eq!(resources.nano_cpus, Some(750_000_000));
        assert!(params.get("password").and_then(|v| v.as_str()).is_some());
        assert!(params
            .get("root_password")
            .and_then(|v| v.as_str())
            .is_some());
    }

    #[test]
    fn test_mariadb_rejects_readonly_update() {
        let strategy = MariaDbParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "root_password".to_string(),
            JsonValue::String("new-secure-password".to_string()),
        );

        let result = strategy.validate_for_update(&updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_mariadb_rejects_invalid_size_profile() {
        let strategy = MariaDbParameterStrategy;
        let mut params = HashMap::new();
        params.insert(
            "size_profile".to_string(),
            JsonValue::String("oversized".to_string()),
        );

        let result = strategy.validate_for_creation(&params);
        assert!(result.is_err());
    }

    #[test]
    fn test_mongodb_updateable_docker_image() {
        let strategy = MongodbParameterStrategy;
        let mut updates = HashMap::new();
        updates.insert(
            "docker_image".to_string(),
            JsonValue::String("mongo:9.0".to_string()),
        );

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
            JsonValue::String("gotempsh/postgres-walg:17-bookworm".to_string()),
        );

        let mut updates = HashMap::new();
        updates.insert(
            "docker_image".to_string(),
            JsonValue::String("gotempsh/postgres-walg:18-bookworm".to_string()),
        );

        let result = strategy.merge_updates(&mut existing, updates);
        assert!(result.is_ok());
        assert_eq!(
            existing.get("docker_image").and_then(|v| v.as_str()),
            Some("gotempsh/postgres-walg:18-bookworm")
        );
    }

    // ─── credential validators ─────────────────────────────────────

    fn pg_params(user: &str, db: &str, password: Option<&str>) -> HashMap<String, JsonValue> {
        let mut p = HashMap::new();
        p.insert("username".to_string(), JsonValue::String(user.to_string()));
        p.insert("database".to_string(), JsonValue::String(db.to_string()));
        if let Some(pw) = password {
            p.insert("password".to_string(), JsonValue::String(pw.to_string()));
        }
        p
    }

    #[test]
    fn pg_identifier_accepts_normal_names() {
        for ok in ["postgres", "my_app", "Foo", "_underscore", "x", "a1b2c3"] {
            assert!(is_valid_pg_identifier(ok), "{ok} should be valid");
        }
    }

    #[test]
    fn pg_identifier_rejects_dangerous_chars() {
        for bad in [
            "",
            "1starts_with_digit",
            "has space",
            "has-dash",
            "has.dot",
            "has=equals", // libpq would parse as connstring
            "has;semi",
            "has'quote",
            "has\"quote",
            "drop table foo",
            "host=evil.com user=postgres",
        ] {
            assert!(!is_valid_pg_identifier(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn pg_identifier_rejects_too_long() {
        let too_long = "a".repeat(64);
        assert!(!is_valid_pg_identifier(&too_long));
        let just_right = "a".repeat(63);
        assert!(is_valid_pg_identifier(&just_right));
    }

    #[test]
    fn pg_password_accepts_strong_passwords() {
        for ok in [
            "p@ssw0rd!",
            "Tr0ub4dor&3",
            "correct horse battery staple",
            "AbCdEf123!@#%^&*()-+=[]{}|:,.<>/?~`",
            "πιοθβ", // unicode is fine
        ] {
            assert!(
                is_valid_pg_password(ok).is_ok(),
                "{ok:?} should be accepted, got {:?}",
                is_valid_pg_password(ok)
            );
        }
    }

    #[test]
    fn pg_password_rejects_sql_injection_payloads() {
        for bad in [
            "",
            "has'quote",
            "has\\backslash",
            "has\nnewline",
            "has\0null",
            "has$dollar",
            "'; DROP TABLE pg_authid; --",
            "x'; ALTER ROLE postgres SUPERUSER PASSWORD 'pwn'; --",
        ] {
            assert!(
                is_valid_pg_password(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn validate_credentials_accepts_clean_input() {
        let ok = pg_params("postgres", "myapp", Some("safe_password_123!"));
        assert!(validate_postgres_credentials(&ok).is_ok());
    }

    #[test]
    fn validate_credentials_rejects_libpq_redirect_in_database() {
        // psql -d "host=evil.com user=postgres" → libpq parses as connstring
        let bad = pg_params("postgres", "host=evil.com user=postgres", None);
        let err = validate_postgres_credentials(&bad).unwrap_err();
        assert!(err.contains("database"), "got: {err}");
    }

    #[test]
    fn validate_credentials_rejects_sql_injection_in_password() {
        let bad = pg_params(
            "postgres",
            "myapp",
            Some("x'; ALTER ROLE postgres SUPERUSER PASSWORD 'pwn'; --"),
        );
        let err = validate_postgres_credentials(&bad).unwrap_err();
        assert!(err.contains("password"), "got: {err}");
    }

    #[test]
    fn validate_credentials_rejects_quoted_username() {
        let bad = pg_params("evil\"user", "myapp", None);
        let err = validate_postgres_credentials(&bad).unwrap_err();
        assert!(err.contains("username"), "got: {err}");
    }

    #[test]
    fn postgres_strategy_full_creation_validation() {
        // End-to-end: validate_for_creation must fail on a bad password
        // even when database+username are otherwise fine.
        let strategy = PostgresParameterStrategy;
        let bad = pg_params("postgres", "myapp", Some("with'quote"));
        assert!(strategy.validate_for_creation(&bad).is_err());

        let ok = pg_params("postgres", "myapp", Some("strong_password_456!"));
        assert!(strategy.validate_for_creation(&ok).is_ok());
    }

    /// Regression: the UI and the parameter schema both promise users that
    /// leaving the password field empty triggers auto-generation. That only
    /// works if validation does not reject empty passwords — the
    /// auto-generator runs in `auto_generate_missing` AFTER
    /// `validate_for_creation`. An earlier bug returned 400 "password
    /// cannot be empty" before the generator could run.
    #[test]
    fn validate_credentials_accepts_empty_password_for_auto_generation() {
        let mut params = pg_params("postgres", "myapp", None);
        params.insert("password".into(), JsonValue::String(String::new()));
        assert!(
            validate_postgres_credentials(&params).is_ok(),
            "empty password must be allowed; the auto-generator fills it"
        );

        // The full strategy run must also accept empty and produce a non-empty
        // password after auto_generate_missing has run.
        let strategy = PostgresParameterStrategy;
        let mut full = pg_params("postgres", "myapp", None);
        full.insert("password".into(), JsonValue::String(String::new()));
        assert!(strategy.validate_for_creation(&full).is_ok());
        strategy.auto_generate_missing(&mut full).unwrap();
        let generated = full
            .get("password")
            .and_then(|v| v.as_str())
            .expect("password must be set after auto_generate_missing");
        assert!(
            !generated.is_empty(),
            "auto_generate_missing must fill an empty password"
        );
        assert!(
            is_valid_pg_password(generated).is_ok(),
            "auto-generated password must itself pass validation"
        );
    }
}
