//! Database connection management

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use temps_migrations::{Migrator, MigratorTrait};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub type DbConnection = DatabaseConnection;

/// Default timeout for database connectivity check (5 seconds)
const CONNECTIVITY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for database connection establishment (30 seconds)
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for running migrations (120 seconds)
const MIGRATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Parse database URL and extract host and port
fn parse_database_url(database_url: &str) -> Result<(String, u16), String> {
    // Handle postgres:// or postgresql:// URLs
    let url =
        if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
            database_url.to_string()
        } else {
            return Err("Database URL must start with postgres:// or postgresql://".to_string());
        };

    // Parse the URL to extract host and port
    // Format: postgres://user:password@host:port/database
    let without_scheme = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or("Invalid database URL scheme")?;

    // Find the @ separator (after credentials)
    let host_part = if let Some(at_pos) = without_scheme.rfind('@') {
        &without_scheme[at_pos + 1..]
    } else {
        without_scheme
    };

    // Remove database name (everything after /)
    let host_port = if let Some(slash_pos) = host_part.find('/') {
        &host_part[..slash_pos]
    } else {
        host_part
    };

    // Remove query parameters (everything after ?)
    let host_port = if let Some(query_pos) = host_port.find('?') {
        &host_port[..query_pos]
    } else {
        host_port
    };

    // Parse host and port
    // Handle IPv6 addresses like [::1]:5432
    let (host, port) = if host_port.starts_with('[') {
        // IPv6 address
        if let Some(bracket_end) = host_port.find(']') {
            let ipv6_host = &host_port[1..bracket_end];
            let port_part = &host_port[bracket_end + 1..];
            let port = if port_part.starts_with(':') {
                port_part[1..].parse::<u16>().unwrap_or(5432)
            } else {
                5432
            };
            (ipv6_host.to_string(), port)
        } else {
            return Err("Invalid IPv6 address format in database URL".to_string());
        }
    } else if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port = host_port[colon_pos + 1..].parse::<u16>().unwrap_or(5432);
        (host.to_string(), port)
    } else {
        (host_port.to_string(), 5432)
    };

    if host.is_empty() {
        return Err("Empty host in database URL".to_string());
    }

    Ok((host, port))
}

/// Check if the database host:port is reachable via TCP
async fn check_database_connectivity(host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{}:{}", host, port);

    match timeout(CONNECTIVITY_CHECK_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("Cannot connect to database at {}: {}", addr, e)),
        Err(_) => Err(format!(
            "Connection to database at {} timed out after {} seconds",
            addr,
            CONNECTIVITY_CHECK_TIMEOUT.as_secs()
        )),
    }
}

pub async fn establish_connection(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    // Parse the database URL to extract host and port
    let (host, port) = parse_database_url(database_url)
        .map_err(|e| ServiceError::Database(format!("Invalid database URL: {}", e)))?;

    // Check if the database is reachable before attempting to connect
    check_database_connectivity(&host, port)
        .await
        .map_err(|e| ServiceError::Database(e))?;

    let mut opt = ConnectOptions::new(database_url);
    opt.max_connections(100)
        .min_connections(5)
        .connect_timeout(CONNECTION_TIMEOUT)
        .sqlx_logging(false); // Disable verbose SQL query logging

    // Connect with timeout
    let db = match timeout(CONNECTION_TIMEOUT, Database::connect(opt)).await {
        Ok(Ok(db)) => db,
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to connect to database: {}",
                e
            )));
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database connection timed out after {} seconds",
                CONNECTION_TIMEOUT.as_secs()
            )));
        }
    };

    // Run migrations with timeout
    match timeout(MIGRATION_TIMEOUT, Migrator::up(&db, None)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(ServiceError::Database(format!(
                "Failed to run migrations: {}",
                e
            )));
        }
        Err(_) => {
            return Err(ServiceError::Database(format!(
                "Database migrations timed out after {} seconds",
                MIGRATION_TIMEOUT.as_secs()
            )));
        }
    }

    Ok(Arc::new(db))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_database_url_basic() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@localhost/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_custom_port() {
        let (host, port) =
            parse_database_url("postgresql://user:pass@db.example.com:5433/mydb").unwrap();
        assert_eq!(host, "db.example.com");
        assert_eq!(port, 5433);
    }

    #[test]
    fn test_parse_database_url_with_query_params() {
        let (host, port) =
            parse_database_url("postgres://user:pass@localhost:5432/db?sslmode=require").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_no_credentials() {
        let (host, port) = parse_database_url("postgres://localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]:5432/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_ipv6_default_port() {
        let (host, port) = parse_database_url("postgres://user:pass@[::1]/db").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 5432);
    }

    #[test]
    fn test_parse_database_url_invalid_scheme() {
        let result = parse_database_url("mysql://user:pass@localhost:3306/db");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_database_url_special_chars_in_password() {
        // Password with @ symbol should still work (using rfind for @)
        let (host, port) = parse_database_url("postgres://user:p%40ss@localhost:5432/db").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 5432);
    }
}
