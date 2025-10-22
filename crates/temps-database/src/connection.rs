//! Database connection management

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use std::sync::Arc;
use temps_core::{ServiceError, ServiceResult};
use temps_migrations::{Migrator, MigratorTrait};

pub type DbConnection = DatabaseConnection;

pub async fn establish_connection(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    let mut opt = ConnectOptions::new(database_url);
    opt.max_connections(100).min_connections(5);

    let db = Database::connect(opt)
        .await
        .map_err(|e| ServiceError::Database(e.to_string()))?;

    // Run migrations
    Migrator::up(&db, None)
        .await
        .map_err(|e| ServiceError::Database(e.to_string()))?;

    Ok(Arc::new(db))
}
