//! Database migration command.
//!
//! Applies pending schema migrations explicitly, decoupled from `temps serve`.
//!
//! This is the RECOMMENDED upgrade flow for production / enterprise installs:
//!
//! ```text
//!   1. Download the new temps binary
//!   2. temps migrate --database-url=...   # apply schema changes, server down
//!   3. Restart the server with the new binary
//! ```
//!
//! Running migrations as a separate step means a slow migration (e.g. building
//! an index on a multi-million-row table) can never put the API server into a
//! crash/restart loop — the operator runs it with no time pressure and watches
//! it complete before bringing traffic back.
//!
//! `temps serve` still applies pending migrations automatically so simple
//! single-node installs keep their zero-step upgrade; this command is for
//! operators who want explicit control.

use clap::Args;
use colored::Colorize;
use tracing::info;

#[derive(Args)]
pub struct MigrateCommand {
    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, env = "TEMPS_LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    /// Log format: compact, full
    #[arg(long, env = "TEMPS_LOG_FORMAT", default_value = "compact")]
    pub log_format: String,
}

impl MigrateCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        // Local runtime — keep execute() synchronous (pingora compatibility).
        let rt = tokio::runtime::Runtime::new()?;

        rt.block_on(async {
            println!("{}", "Running database migrations…".bold());

            // Connect WITHOUT auto-migrating; we run migrations explicitly so
            // any error surfaces here rather than during a server boot.
            let db = temps_database::connect_without_migrations(&self.database_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

            temps_database::run_migrations(&db)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            // Continuous-aggregate backfill is idempotent and safe to run here
            // (the operator is already waiting on this command).
            if let Err(e) = temps_database::run_post_migration_backfill(&db).await {
                info!("Post-migration backfill skipped/failed (refresh policy will catch up): {e}");
            }

            println!(
                "{}",
                "✓ Migrations applied. Safe to restart the server.".green()
            );
            Ok::<(), anyhow::Error>(())
        })
    }
}
