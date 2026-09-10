// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database migration command.
//!
//! Applies pending schema migrations explicitly, decoupled from `temps serve`.
//!
//! This is the RECOMMENDED upgrade flow for production / enterprise installs:
//!
//! ```text
//!   1. Download the new temps binary
//!   2. TEMPS_DATABASE_URL=... temps migrate   # apply schema changes, server down
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
use std::future::Future;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use tracing::info;

#[derive(Args)]
pub struct MigrateCommand {
    /// Database connection URL (set via TEMPS_DATABASE_URL env var; not accepted as a flag to prevent credentials leaking into process listings)
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
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

    /// Progress output format.
    ///
    /// When set to `json`, one NDJSON line is written to stdout per migration
    /// event instead of the human-readable text output. Designed for use by a
    /// parent process (e.g. the "Update Now" orchestration worker) that spawns
    /// `temps migrate --progress-format=json` as a child and parses its output
    /// line-by-line to track progress. The human-readable format remains the
    /// default for interactive use.
    ///
    /// JSON line shapes:
    ///
    /// ```json
    /// {"event":"started","index":1,"total":5,"name":"m20260811_..."}
    /// {"event":"finished","index":1,"total":5,"name":"...","success":true,"elapsed_ms":123}
    /// {"event":"finished","index":1,"total":5,"name":"...","success":false,"elapsed_ms":123,"error":"..."}
    /// ```
    #[arg(long, value_name = "FORMAT")]
    pub progress_format: Option<String>,
}

enum MaintenanceRace<T> {
    Completed(T),
    Interrupted,
}

async fn race_maintenance<M, I, T>(maintenance: M, interrupt: I) -> MaintenanceRace<T>
where
    M: Future<Output = T>,
    I: Future,
{
    tokio::select! {
        result = maintenance => MaintenanceRace::Completed(result),
        _ = interrupt => MaintenanceRace::Interrupted,
    }
}

async fn interrupted_maintenance_error(
    database_url: &str,
    backend_pid: i32,
    phase: &str,
) -> anyhow::Error {
    println!();
    println!("{}", format!("Interrupted — cancelling {phase}…").yellow());
    match temps_database::cancel_migration_backend(database_url, backend_pid).await {
        Ok(()) => anyhow::anyhow!(
            "{phase} was interrupted and PostgreSQL confirmed the statement on backend \
             {backend_pid} is no longer active. \
             Re-run `temps migrate` to repair any invalid concurrent index and continue."
        ),
        Err(error) => anyhow::anyhow!(
            "{phase} was interrupted, but PostgreSQL did not confirm that backend {backend_pid} \
             stopped: {error}. Do not rerun migrations until that PID is no longer active; check \
             `SELECT pid, state, query FROM pg_stat_activity WHERE pid = {backend_pid}`."
        ),
    }
}

async fn run_post_migration_maintenance(
    db: &Arc<temps_database::DbConnection>,
    database_url: &str,
    backend_pid: i32,
    use_json: bool,
) -> anyhow::Result<()> {
    // Announce the phase BEFORE any work starts. Concurrent index builds and
    // continuous-aggregate refreshes routinely take minutes on a large install;
    // without a header and per-step lines the operator stares at a terminal that
    // looks hung after the last migration line and has no way to tell that
    // anything is still running.
    if !use_json {
        println!();
        println!("{}", "Post-migration maintenance…".bold());
        println!(
            "{}",
            "  Non-transactional work that runs outside the migrations above. \
             Ctrl+C cancels it safely; re-run `temps migrate` to resume."
                .dimmed()
        );
    }

    // In-place `\r` rewriting only reads correctly on a terminal. Piped into a
    // log file or journald it concatenates every step onto one unreadable line,
    // so non-TTY callers get one complete line per event instead.
    let interactive = std::io::stdout().is_terminal();

    // Track the backend each maintenance phase actually runs on. The index phase
    // uses a disposable connection, so by the time the backfill starts, the
    // connect-time `backend_pid` is a dead PID — cancelling it on Ctrl+C would
    // report "PID N is not a PostgreSQL backend process" while the refresh it was
    // meant to stop kept running. Phases announce their own PID; cancel that one.
    let phase_pid = Arc::new(std::sync::atomic::AtomicI32::new(backend_pid));

    let report_progress = {
        let phase_pid = phase_pid.clone();
        move |progress: temps_database::MaintenanceProgress<'_>| {
            if let temps_database::MaintenanceProgress::PhaseBackend { pid } = &progress {
                phase_pid.store(*pid, std::sync::atomic::Ordering::SeqCst);
            }
            if use_json {
                print_maintenance_json(progress);
            } else {
                print_maintenance(progress, interactive);
            }
        }
    };
    let current_pid = || phase_pid.load(std::sync::atomic::Ordering::SeqCst);

    match race_maintenance(
        temps_database::run_post_migration_indexes_streaming(db, &report_progress),
        tokio::signal::ctrl_c(),
    )
    .await
    {
        MaintenanceRace::Completed(result) => result?,
        MaintenanceRace::Interrupted => {
            return Err(interrupted_maintenance_error(
                database_url,
                current_pid(),
                "post-migration index creation",
            )
            .await)
        }
    }

    match race_maintenance(
        temps_database::run_post_migration_backfill_streaming(db, &report_progress),
        tokio::signal::ctrl_c(),
    )
    .await
    {
        MaintenanceRace::Completed(Ok(())) => {}
        MaintenanceRace::Completed(Err(error)) => {
            info!("Post-migration backfill skipped/failed (refresh policy will catch up): {error}");
            if !use_json {
                println!(
                    "  {} {}",
                    "!".yellow(),
                    format!("Backfill skipped: {error} (the refresh policy will catch up)")
                        .yellow()
                );
            }
        }
        MaintenanceRace::Interrupted => {
            return Err(interrupted_maintenance_error(
                database_url,
                current_pid(),
                "post-migration backfill",
            )
            .await)
        }
    }

    Ok(())
}

impl MigrateCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        // Local runtime — keep execute() synchronous (pingora compatibility).
        let rt = tokio::runtime::Runtime::new()?;

        let use_json = self
            .progress_format
            .as_deref()
            .map(|f| f.eq_ignore_ascii_case("json"))
            .unwrap_or(false);

        rt.block_on(async {
            // Connect WITHOUT auto-migrating; we run migrations explicitly so
            // any error surfaces here rather than during a server boot. The
            // connection is read-only until we actually apply, so it's safe to
            // use for the dry-run/plan preview as well.
            //
            // A single, stable backend (whose PID we capture) lets us cancel the
            // in-flight migration server-side on Ctrl+C — otherwise the Postgres
            // backend keeps running the DDL after the CLI exits, and a re-run
            // would race a second backend against it.
            let (db, backend_pid) = temps_database::connect_for_migrate(&self.database_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

            // Read the plan up front (read-only) so we can show it before
            // applying anything — both for --dry-run and the confirmation gate.
            let pending = temps_database::get_pending_migration_names(&db)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            if pending.is_empty() {
                if use_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "up_to_date",
                            "message": "Database already up to date — no migrations to apply."
                        })
                    );
                    let _ = std::io::stdout().flush();
                } else {
                    println!(
                        "{}",
                        "✓ Database already up to date — no migrations to apply.".green()
                    );
                }
                if !self.dry_run {
                    // Non-transactional maintenance must remain retryable even
                    // after every schema migration is already recorded.
                    run_post_migration_maintenance(&db, &self.database_url, backend_pid, use_json)
                        .await?;
                    if !use_json {
                        println!("{}", "✓ Post-migration maintenance complete.".green());
                    }
                }
                return Ok(());
            }

            if !use_json {
                print_pending_plan(&pending);
            }

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

            // Confirmation gate. Skipped with --yes, in JSON mode (machine-driven),
            // or when stdin is not a TTY (piped/CI/systemd) so existing automation
            // never hangs on a prompt.
            if !self.yes
                && !use_json
                && std::io::stdin().is_terminal()
                && !confirm_apply(pending.len())?
            {
                println!("{}", "Migration cancelled. Nothing was changed.".dimmed());
                return Ok(());
            }

            if !use_json {
                println!();
                println!("{}", "Applying…".bold());
            }

            // Stream progress so each migration is reported the moment it starts
            // and finishes — a slow index build shows as the in-flight line
            // rather than a frozen "Running database migrations…".
            //
            // Race the apply against Ctrl+C. On interrupt we cancel the migrating
            // backend SERVER-SIDE (the captured PID) — dropping the client future
            // alone would leave Postgres running the DDL, so a re-run could put a
            // second backend on the same schema. The cancelled statement's
            // transaction rolls back, leaving the DB at the last applied step.
            let report = if use_json {
                tokio::select! {
                    res = temps_database::run_migrations_streaming(&db, print_progress_json) => {
                        res.map_err(|e| anyhow::anyhow!("{}", e))?
                    }
                    _ = tokio::signal::ctrl_c() => {
                        return Err(interrupted_maintenance_error(
                            &self.database_url,
                            backend_pid,
                            "migration execution",
                        ).await);
                    }
                }
            } else {
                tokio::select! {
                    res = temps_database::run_migrations_streaming(&db, print_progress) => {
                        res.map_err(|e| anyhow::anyhow!("{}", e))?
                    }
                    _ = tokio::signal::ctrl_c() => {
                        return Err(interrupted_maintenance_error(
                            &self.database_url,
                            backend_pid,
                            "migration execution",
                        ).await);
                    }
                }
            };

            // Surface anything planned but not attempted (everything after a
            // failure) so the operator knows the set is incomplete.
            if !use_json {
                print_unattempted(&report);
            }

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

            // Keep Ctrl+C active through non-transactional maintenance too.
            // Tokio's process-wide signal handler remains installed after the
            // migration select above, so awaiting these phases directly would
            // swallow a later SIGINT and leave the operator unable to cancel.
            run_post_migration_maintenance(&db, &self.database_url, backend_pid, use_json).await?;

            let total: std::time::Duration = report.results.iter().map(|r| r.elapsed).sum();
            if use_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "complete",
                        "migrations_applied": report.results.len(),
                        "elapsed_ms": total.as_millis() as u64,
                    })
                );
                let _ = std::io::stdout().flush();
            } else {
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

/// Machine-readable progress callback for `--progress-format=json`.
///
/// Emits one NDJSON line per event to stdout so a parent process can parse
/// migration progress without screen-scraping human text. Each line is a
/// self-contained JSON object terminated by `\n`.
///
/// Line shapes:
/// ```json
/// {"event":"started","index":1,"total":5,"name":"m20260811_..."}
/// {"event":"finished","index":1,"total":5,"name":"...","success":true,"elapsed_ms":123}
/// {"event":"finished","index":1,"total":5,"name":"...","success":false,"elapsed_ms":123,"error":"..."}
/// ```
fn print_progress_json(progress: temps_database::MigrationProgress<'_>) {
    use temps_database::MigrationProgress;
    match progress {
        MigrationProgress::Started { index, total, name } => {
            // Use serde_json::json! to avoid any escaping bugs in hand-rolled JSON.
            let line = serde_json::json!({
                "event": "started",
                "index": index,
                "total": total,
                "name": name,
            });
            println!("{line}");
        }
        MigrationProgress::Finished {
            index,
            total,
            result,
        } => {
            let elapsed_ms = result.elapsed.as_millis() as u64;
            let mut obj = serde_json::json!({
                "event": "finished",
                "index": index,
                "total": total,
                "name": result.name,
                "success": result.success,
                "elapsed_ms": elapsed_ms,
            });
            if let Some(err) = &result.error {
                obj["error"] = serde_json::Value::String(err.clone());
            }
            println!("{obj}");
        }
    }
    // Flush so the parent sees the line immediately — unbuffered stdout is
    // not guaranteed inside `tokio::spawn`, and the parent blocks on each line.
    let _ = std::io::stdout().flush();
}

/// Live progress callback for post-migration maintenance (human output).
///
/// On a terminal this uses the same in-place convention as [`print_progress`]:
/// the in-flight line is written without a newline and rewritten via `\r` as
/// work advances, so a 31-window backfill reports `[7/31]` as it goes instead
/// of printing nothing for several minutes. Each rewritten line is at least as
/// long as the one it replaces, so no characters are left behind.
///
/// When `interactive` is false (piped output, CI, systemd) every event gets its
/// own complete line — `\r` rewriting is unreadable in a log file.
fn print_maintenance(progress: temps_database::MaintenanceProgress<'_>, interactive: bool) {
    use temps_database::MaintenanceProgress;
    match progress {
        // Bookkeeping for the interrupt path, not something to show an operator
        // mid-run; the PID is surfaced in the interruption error if it matters.
        MaintenanceProgress::PhaseBackend { .. } => {}
        MaintenanceProgress::IndexStarted { name } => {
            if interactive {
                print!("  {} index {} …", "→".cyan(), name);
                let _ = std::io::stdout().flush();
            } else {
                println!("  {} index {} …", "→".cyan(), name);
            }
        }
        MaintenanceProgress::IndexFinished { name, elapsed } => {
            println!(
                "{}  {} index {} {}",
                if interactive { "\r" } else { "" },
                "✓".green(),
                name,
                format!("({})", format_elapsed(elapsed)).dimmed()
            );
        }
        MaintenanceProgress::AggregateStarted { view, windows } => {
            if interactive {
                print!("  {} {} refresh [0/{}] …", "→".cyan(), view, windows);
                let _ = std::io::stdout().flush();
            } else {
                println!(
                    "  {} {} refresh: {} window(s) to process …",
                    "→".cyan(),
                    view,
                    windows
                );
            }
        }
        MaintenanceProgress::AggregateWindow {
            view,
            index,
            total,
            elapsed,
            error,
        } => {
            if let Some(error) = error {
                // A failed window gets its own permanent line — the operator
                // must be able to see WHICH window failed and why, even though
                // the backfill deliberately continues.
                println!(
                    "{}  {} {} window {}/{} failed: {}",
                    if interactive { "\r" } else { "" },
                    "!".yellow(),
                    view,
                    index,
                    total,
                    error.yellow()
                );
            }
            if interactive {
                print!(
                    "\r  {} {} refresh [{}/{}] … {}",
                    "→".cyan(),
                    view,
                    index,
                    total,
                    format!("(last window {})", format_elapsed(elapsed)).dimmed()
                );
                let _ = std::io::stdout().flush();
            } else {
                println!(
                    "  {} {} refresh [{}/{}] {}",
                    "·".dimmed(),
                    view,
                    index,
                    total,
                    format!("({})", format_elapsed(elapsed)).dimmed()
                );
            }
        }
        MaintenanceProgress::AggregateFinished {
            view,
            windows,
            failed,
            elapsed,
        } => {
            let suffix = if failed == 0 {
                format!("({})", format_elapsed(elapsed))
                    .dimmed()
                    .to_string()
            } else {
                format!("({}, {failed} window(s) failed)", format_elapsed(elapsed))
                    .yellow()
                    .to_string()
            };
            println!(
                "{}  {} {} refresh [{}/{}] {}          ",
                if interactive { "\r" } else { "" },
                if failed == 0 {
                    "✓".green()
                } else {
                    "!".yellow()
                },
                view,
                windows,
                windows,
                suffix
            );
        }
    }
}

/// Machine-readable maintenance progress for `--progress-format=json`.
///
/// Mirrors [`print_maintenance`] as NDJSON so the "Update Now" orchestration
/// worker can show the same phase to the user instead of a stalled progress
/// bar between the last migration and completion.
fn print_maintenance_json(progress: temps_database::MaintenanceProgress<'_>) {
    println!("{}", maintenance_json(progress));
    let _ = std::io::stdout().flush();
}

/// The NDJSON line for one maintenance event. Split out from
/// [`print_maintenance_json`] so the wire contract is unit-testable.
fn maintenance_json(progress: temps_database::MaintenanceProgress<'_>) -> serde_json::Value {
    use temps_database::MaintenanceProgress;
    match progress {
        // Emitted so a parent process can report/kill the exact backend if the
        // child is terminated out from under it.
        MaintenanceProgress::PhaseBackend { pid } => serde_json::json!({
            "event": "maintenance_backend",
            "pid": pid,
        }),
        MaintenanceProgress::IndexStarted { name } => serde_json::json!({
            "event": "maintenance_index_started",
            "name": name,
        }),
        MaintenanceProgress::IndexFinished { name, elapsed } => serde_json::json!({
            "event": "maintenance_index_finished",
            "name": name,
            "elapsed_ms": elapsed.as_millis() as u64,
        }),
        MaintenanceProgress::AggregateStarted { view, windows } => serde_json::json!({
            "event": "maintenance_backfill_started",
            "view": view,
            "windows": windows,
        }),
        MaintenanceProgress::AggregateWindow {
            view,
            index,
            total,
            elapsed,
            error,
        } => serde_json::json!({
            "event": "maintenance_backfill_window",
            "view": view,
            "index": index,
            "total": total,
            "elapsed_ms": elapsed.as_millis() as u64,
            "success": error.is_none(),
            "error": error,
        }),
        MaintenanceProgress::AggregateFinished {
            view,
            windows,
            failed,
            elapsed,
        } => serde_json::json!({
            "event": "maintenance_backfill_finished",
            "view": view,
            "windows": windows,
            "failed": failed,
            "elapsed_ms": elapsed.as_millis() as u64,
        }),
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

#[cfg(test)]
mod tests {
    use super::{race_maintenance, MaintenanceRace};

    #[tokio::test]
    async fn maintenance_race_observes_interrupts_after_migrations_finish() {
        let result = race_maintenance(std::future::pending::<()>(), std::future::ready(())).await;
        assert!(matches!(result, MaintenanceRace::Interrupted));
    }

    #[test]
    fn maintenance_backfill_window_json_reports_progress_and_failure() {
        use temps_database::MaintenanceProgress;

        let ok = super::maintenance_json(MaintenanceProgress::AggregateWindow {
            view: "proxy_logs_stats_1m",
            index: 7,
            total: 31,
            elapsed: std::time::Duration::from_millis(1_200),
            error: None,
        });
        assert_eq!(ok["event"], "maintenance_backfill_window");
        assert_eq!(ok["view"], "proxy_logs_stats_1m");
        assert_eq!(ok["index"], 7);
        assert_eq!(ok["total"], 31);
        assert_eq!(ok["elapsed_ms"], 1_200);
        assert_eq!(ok["success"], true);
        assert!(ok["error"].is_null());

        let failed = super::maintenance_json(MaintenanceProgress::AggregateWindow {
            view: "events_hourly",
            index: 1,
            total: 1,
            elapsed: std::time::Duration::from_millis(5),
            error: Some("lock timeout".to_string()),
        });
        assert_eq!(failed["success"], false);
        assert_eq!(failed["error"], "lock timeout");
    }

    #[test]
    fn maintenance_index_json_events_are_distinguishable() {
        use temps_database::MaintenanceProgress;

        let started = super::maintenance_json(MaintenanceProgress::IndexStarted { name: "idx_a" });
        assert_eq!(started["event"], "maintenance_index_started");
        assert_eq!(started["name"], "idx_a");

        let finished = super::maintenance_json(MaintenanceProgress::IndexFinished {
            name: "idx_a",
            elapsed: std::time::Duration::from_secs(3),
        });
        assert_eq!(finished["event"], "maintenance_index_finished");
        assert_eq!(finished["elapsed_ms"], 3_000);
    }

    #[tokio::test]
    async fn maintenance_race_returns_completed_work() {
        let result = race_maintenance(std::future::ready(42), std::future::pending::<()>()).await;
        assert!(matches!(result, MaintenanceRace::Completed(42)));
    }
}
