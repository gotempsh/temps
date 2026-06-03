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

            // Step-wise apply so we can show the plan up front and a ✓/✗ line
            // per migration with its timing.
            let report = temps_database::run_migrations_reported(&db)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            print_migration_report(&report);

            // Stop here if a migration failed — do not run the backfill or
            // claim success. Surface which migration failed and why.
            if let Some(failed) = report.failed() {
                let reason = failed.error.as_deref().unwrap_or("unknown error");
                anyhow::bail!(
                    "Migration `{}` failed after {}: {}\n  \
                     The database is left at the last successfully-applied migration. \
                     Fix the cause and re-run `temps migrate`.",
                    failed.name,
                    format_elapsed(failed.elapsed),
                    reason
                );
            }

            // Continuous-aggregate backfill is idempotent and safe to run here
            // (the operator is already waiting on this command).
            if let Err(e) = temps_database::run_post_migration_backfill(&db).await {
                info!("Post-migration backfill skipped/failed (refresh policy will catch up): {e}");
            }

            if report.planned.is_empty() {
                println!(
                    "{}",
                    "✓ Database already up to date — no migrations to apply.".green()
                );
            } else {
                let total: std::time::Duration = report.results.iter().map(|r| r.elapsed).sum();
                println!(
                    "{}",
                    format!(
                        "✓ {} migration(s) applied in {}. Safe to restart the server.",
                        report.results.len(),
                        format_elapsed(total)
                    )
                    .green()
                );
            }
            Ok::<(), anyhow::Error>(())
        })
    }
}

/// Print the migration plan (what will be applied) and the per-migration
/// result lines (✓/✗ + timing). Called after a step-wise apply so the operator
/// sees both what was scheduled and how each step went.
fn print_migration_report(report: &temps_database::MigrationRunReport) {
    if report.planned.is_empty() {
        println!("{}", "No pending migrations.".dimmed());
        return;
    }

    println!();
    println!(
        "{}",
        format!("Pending migrations ({}):", report.planned.len()).bold()
    );
    for name in &report.planned {
        println!("  {} {}", "-".dimmed(), name);
    }
    println!();
    println!("{}", "Applying…".bold());

    for result in &report.results {
        if result.success {
            println!(
                "  {} {} {}",
                "✓".green(),
                result.name,
                format!("({})", format_elapsed(result.elapsed)).dimmed()
            );
        } else {
            println!(
                "  {} {} {}",
                "✗".red(),
                result.name.red(),
                format!("({})", format_elapsed(result.elapsed)).dimmed()
            );
            if let Some(err) = &result.error {
                println!("      {}", err.red());
            }
        }
    }

    // Anything planned but not attempted (everything after a failure) is
    // surfaced so the operator knows the migration set is incomplete.
    let attempted = report.results.len();
    if attempted < report.planned.len() {
        println!();
        println!(
            "{}",
            format!(
                "{} migration(s) not applied (stopped after the failure above):",
                report.planned.len() - attempted
            )
            .yellow()
        );
        for name in report.planned.iter().skip(attempted) {
            println!("  {} {}", "-".dimmed(), name.dimmed());
        }
    }
    println!();
}

/// Human-friendly elapsed time: `820ms`, `1.34s`, `2m 3s`.
fn format_elapsed(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else {
        let secs = d.as_secs();
        format!("{}m {}s", secs / 60, secs % 60)
    }
}
