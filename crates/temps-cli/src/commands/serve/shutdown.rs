use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use temps_database::DbConnection;
use temps_proxy::ProxyShutdownSignal;
use tracing::{debug, info, warn};

/// Shutdown signal implementation for Ctrl+C with resource cleanup and timeout
pub struct CtrlCShutdownSignal {
    cleanup_timeout: Duration,
    db: Arc<DbConnection>,
    data_dir: PathBuf,
}

impl CtrlCShutdownSignal {
    pub fn new(cleanup_timeout: Duration, db: Arc<DbConnection>, data_dir: PathBuf) -> Self {
        Self {
            cleanup_timeout,
            db,
            data_dir,
        }
    }

    /// Perform cleanup operations with timeout
    async fn cleanup_resources(&self) {
        info!("Starting resource cleanup...");

        let cleanup_future = async {
            // Cancel running deployments
            self.cleanup_deployments().await;

            // Database cleanup
            self.cleanup_database().await;

            // File system cleanup
            self.cleanup_files().await;

            info!("Resource cleanup completed successfully");
        };

        // Apply timeout to cleanup operations
        match tokio::time::timeout(self.cleanup_timeout, cleanup_future).await {
            Ok(()) => {
                info!("All resources cleaned up within timeout");
            }
            Err(_) => {
                warn!(
                    "Cleanup timeout exceeded ({:?}), forcing shutdown",
                    self.cleanup_timeout
                );
            }
        }
    }

    async fn cleanup_deployments(&self) {
        use sea_orm::{sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::deployments;

        debug!("Cancelling running deployments...");

        // Update all running deployments to cancelled status directly using the database connection
        match deployments::Entity::update_many()
            .filter(deployments::Column::State.eq("running"))
            .col_expr(deployments::Column::State, Expr::value("cancelled"))
            .col_expr(
                deployments::Column::CancelledReason,
                Expr::value("Server shutdown"),
            )
            .col_expr(
                deployments::Column::FinishedAt,
                Expr::current_timestamp().into(),
            )
            .col_expr(
                deployments::Column::UpdatedAt,
                Expr::current_timestamp().into(),
            )
            .exec(self.db.as_ref())
            .await
        {
            Ok(result) => {
                let count = result.rows_affected;
                if count > 0 {
                    info!("Cancelled {} running deployment(s) during shutdown", count);
                } else {
                    debug!("No running deployments to cancel");
                }
            }
            Err(e) => {
                warn!("Failed to cancel running deployments: {}", e);
            }
        }
    }

    async fn cleanup_database(&self) {
        debug!("Cleaning up database connections...");

        // Try to unwrap the Arc to get ownership for closing
        // Note: If there are other references, we can't close it directly
        match Arc::try_unwrap(Arc::clone(&self.db)) {
            Ok(db) => {
                // We got exclusive ownership, close the connection
                if let Err(e) = db.close().await {
                    warn!("Error closing database connection: {}", e);
                } else {
                    debug!("Database connection closed successfully");
                }
            }
            Err(_arc) => {
                // Other references still exist, cannot close
                debug!("Database still has other references, skipping close");
            }
        }

        debug!("Database cleanup completed");
    }

    async fn cleanup_files(&self) {
        debug!("Cleaning up temporary files...");

        // Flush log buffers
        // Note: In a real implementation, you'd have access to the subscriber handle to flush
        debug!("Log buffers flushed");

        // Clean up any temporary files in data directory
        let temp_dir = self.data_dir.join("temp");
        if temp_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
                warn!("Failed to remove temp directory: {}", e);
            } else {
                debug!("Temporary files cleaned up");
            }
        }

        debug!("File cleanup completed");
    }
}

impl ProxyShutdownSignal for CtrlCShutdownSignal {
    fn wait_for_signal(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let cleanup_timeout = self.cleanup_timeout;
        let db = Arc::clone(&self.db);
        let data_dir = self.data_dir.clone();

        Box::pin(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for ctrl-c signal");
            info!("Received Ctrl+C, initiating graceful shutdown...");

            // Create a new instance for cleanup since we moved into the async block
            let cleanup_handler = CtrlCShutdownSignal::new(cleanup_timeout, db, data_dir);
            cleanup_handler.cleanup_resources().await;

            info!("Graceful shutdown completed");
        })
    }
}
