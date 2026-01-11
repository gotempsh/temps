//! Database module for LocalTemps
//!
//! Provides SQLite database initialization and migrations using SeaORM.

pub mod migrations;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info};

use migrations::Migrator;

/// Initialize the SQLite database in the app data directory.
///
/// This creates the database file if it doesn't exist and runs all pending migrations.
pub async fn init_database(app_data_dir: PathBuf) -> anyhow::Result<Arc<DatabaseConnection>> {
    // Ensure the directory exists
    tokio::fs::create_dir_all(&app_data_dir).await?;

    // Build the database path
    let db_path = app_data_dir.join("localtemps.db");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    debug!("Connecting to database at: {}", db_path.display());

    // Configure connection options
    let mut opt = ConnectOptions::new(&database_url);
    opt.max_connections(5)
        .min_connections(1)
        .sqlx_logging(false); // Disable verbose SQL logging

    // Connect to database
    let db = Database::connect(opt)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

    // Run migrations
    debug!("Running database migrations...");
    Migrator::up(&db, None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

    info!("Database initialized at: {}", db_path.display());

    Ok(Arc::new(db))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_init_database() {
        let temp_dir = TempDir::new().unwrap();
        let result = init_database(temp_dir.path().to_path_buf()).await;
        assert!(result.is_ok());

        // Check that the database file was created
        let db_path = temp_dir.path().join("localtemps.db");
        assert!(db_path.exists());
    }
}
