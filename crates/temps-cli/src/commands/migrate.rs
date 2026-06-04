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
//!
//! ## Plan, confirm, apply
//!
//! By default, when run from an interactive terminal, `temps migrate` prints the
//! pending-migration plan and asks for confirmation before applying anything.
//! Two flags change that:
//!
//! - `--dry-run` prints the plan and exits without touching the database.
//! - `--yes` / `-y` skips the confirmation prompt (for CI/automation).
//!
//! When stdin is not a TTY (piped input, systemd, CI), the prompt is skipped
//! automatically and migrations apply directly — so existing non-interactive
//! callers keep working unchanged without needing `--yes`.

use clap::Args;
use colored::Colorize;
use std::io::{IsTerminal, Write};
use tracing::info;

#[derive(Args)]
pub struct MigrateCommand {
    /// Database connection URL
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// Show the pending migrations and exit without applying them.
    #[arg(long)]
    pub dry_run: bool,

    /// Apply without the interactive confirmation prompt (for CI/automation).
    #[arg(long, short = 'y')]
    pub yes: bool,

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
            // Connect WITHOUT auto-migrating; we run migrations explicitly so
            // any error surfaces here rather than during a server boot. The
            // connection is read-only until we actually apply, so it's safe to
            // use for the dry-run/plan preview as well.
            let db = temps_database::connect_without_migrations(&self.database_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

            // Read the plan up front (read-only) so we can show it before
            // applying anything — both for --dry-run and the confirmation gate.
            let pending = temps_database::get_pending_migration_names(&db)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            if pending.is_empty() {
                println!(
                    "{}",
                    "✓ Database already up to date — no migrations to apply.".green()
                );
                return Ok(());
            }

            print_pending_plan(&pending);

            // --dry-run: stop here, never touch the schema.
            if self.dry_run {
                println!(
                    "{}",
                    format!(
                        "Dry run — {} migration(s) would be applied. Nothing was changed.",
                        pending.len()
                    )
                    .yellow()
                );
                println!("{}", "Re-run without --dry-run to apply them.".dimmed());
                return Ok(());
            }

            // Confirmation gate. Skipped with --yes, or when stdin is not a TTY
            // (piped/CI/systemd) so existing automation never hangs on a prompt.
            if !self.yes && std::io::stdin().is_terminal() && !confirm_apply(pending.len())? {
                println!("{}", "Migration cancelled. Nothing was changed.".dimmed());
                return Ok(());
            }

            println!();
            println!("{}", "Applying…".bold());

            // Stream progress so each migration is reported the moment it starts
            // and finishes — a slow index build shows as the in-flight line
            // rather than a frozen "Running database migrations…".
            let report = temps_database::run_migrations_streaming(&db, print_progress)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            // Surface anything planned but not attempted (everything after a
            // failure) so the operator knows the set is incomplete.
            print_unattempted(&report);

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
            Ok::<(), anyhow::Error>(())
        })
    }
}

/// Print the pending-migration plan: a header plus one bullet per migration in
/// apply order. Shown up front before the confirmation gate, and reused by
/// `--dry-run`, so the operator sees exactly what would run before anything is
/// applied.
fn print_pending_plan(pending: &[String]) {
    println!();
    println!(
        "{}",
        format!("Pending migrations ({}):", pending.len()).bold()
    );
    for name in pending {
        println!("  {} {}", "-".dimmed(), name);
    }
    println!();
}

/// Prompt the operator to apply the pending migrations. Returns `true` to
/// proceed. Only called from an interactive terminal (TTY-gated by the caller).
fn confirm_apply(count: usize) -> anyhow::Result<bool> {
    print!("Apply {} migration(s)? [y/N] ", count);
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    Ok(input == "y" || input == "yes")
}

/// Live progress callback for [`temps_database::run_migrations_streaming`].
///
/// On `Started`, prints the in-flight line WITHOUT a newline (and flushes) so a
/// slow migration is visibly "the one running" while it works. On `Finished`,
/// overwrites that line in place with the ✓/✗ result and timing.
fn print_progress(progress: temps_database::MigrationProgress<'_>) {
    use temps_database::MigrationProgress;
    match progress {
        MigrationProgress::Started { index, total, name } => {
            // `\r` + no newline: the Finished branch rewrites this same line.
            print!("  {} [{}/{}] {} …", "→".cyan(), index, total, name);
            let _ = std::io::stdout().flush();
        }
        MigrationProgress::Finished {
            index,
            total,
            result,
        } => {
            let prefix = format!("[{}/{}]", index, total).dimmed();
            if result.success {
                // \r returns to column 0; the result line is at least as long
                // as the "…" line it replaces, so no leftover characters remain.
                println!(
                    "\r  {} {} {} {}",
                    "✓".green(),
                    prefix,
                    result.name,
                    format!("({})", format_elapsed(result.elapsed)).dimmed()
                );
            } else {
                println!(
                    "\r  {} {} {} {}",
                    "✗".red(),
                    prefix,
                    result.name.red(),
                    format!("({})", format_elapsed(result.elapsed)).dimmed()
                );
                if let Some(err) = &result.error {
                    println!("      {}", err.red());
                }
            }
        }
    }
}

/// After a streaming apply, surface any planned migrations that were never
/// attempted (everything after a failure) so the operator knows the set is
/// incomplete. No-op on a fully successful run.
fn print_unattempted(report: &temps_database::MigrationRunReport) {
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
