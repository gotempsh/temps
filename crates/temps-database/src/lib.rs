//! Database connection and query utilities

pub use sea_orm;
mod connection;

pub use connection::{establish_connection, DbConnection};

// Export test utilities for use by other crates in their tests
pub mod test_utils;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn test_establish_connection() -> anyhow::Result<()> {
        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Wait a bit for the database to be ready, then connect with retries
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        let mut retries = 5;
        let db = loop {
            match Database::connect(&database_url).await {
                Ok(db) => break db,
                Err(e) if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    if retries == 0 {
                        return Err(anyhow::anyhow!(
                            "Failed to connect to database after retries: {}",
                            e
                        ));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to connect to database: {}", e)),
            }
        };

        // Test basic connectivity
        let result = sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT 1".to_owned(),
        );

        let query_result = db.query_one(result).await?;
        assert!(query_result.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_establish_connection_with_migrations() -> anyhow::Result<()> {
        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Wait a bit for the database to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Retry connection setup
        let mut retries = 5;
        let _connection = loop {
            match establish_connection(&database_url).await {
                Ok(conn) => break conn,
                Err(e) if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    if retries == 0 {
                        return Err(anyhow::anyhow!(
                            "Failed to establish connection after retries: {}",
                            e
                        ));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to establish connection: {}", e)),
            }
        };

        // If we get here, migrations ran successfully and connection is established
        println!("✅ Database connection with migrations established successfully");

        Ok(())
    }
}
