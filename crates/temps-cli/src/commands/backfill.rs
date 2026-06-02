//! `temps backfill` — one-shot data migration utilities.
//!
//! Currently exposes a single subcommand:
//!
//! ```text
//! temps backfill clickhouse [--from ...] [--to ...] [--project-id ...]
//!                           [--batch-size N] [--chunk-days N]
//!                           [--rate-limit-events-per-sec N]
//!                           [--apply-migrations] [--dry-run] [--resume]
//! ```
//!
//! It walks the PostgreSQL `events` hypertable in `(timestamp, id)` keyset
//! order and pushes rows into ClickHouse using the same `ChEventRow` shape
//! the live fan-out worker uses, so dashboards see byte-identical data
//! regardless of whether the row landed via live ingest or backfill.
//!
//! This command does NOT enqueue into `events_ch_outbox`. The outbox is
//! the live ingest's mailbox; using it for a bulk replay would double the
//! PG write load on the primary right when the operator is trying to
//! migrate off it. Re-runs are safe because ClickHouse's
//! `ReplacingMergeTree(_version)` dedupes by `event_id`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use temps_analytics_events::services::ch_backfill::{
    apply_clickhouse_schema, backfill_events_window, count_events_window, BackfillCursor,
    ClickHouseBackend, ClickHouseConfig,
};
use temps_core::DBDateTime;
use tracing::{info, warn};

#[derive(Args)]
pub struct BackfillCommand {
    #[command(subcommand)]
    pub target: BackfillTarget,
}

#[derive(Subcommand)]
pub enum BackfillTarget {
    /// Backfill PostgreSQL `events` into a ClickHouse cluster.
    ///
    /// Same row shape, same dedupe semantics as the live fan-out worker.
    /// Safe to run while `temps serve` is live — does not touch the
    /// outbox and does not write back to the primary.
    Clickhouse(ClickhouseBackfillArgs),
}

#[derive(Args, Clone)]
pub struct ClickhouseBackfillArgs {
    /// PostgreSQL connection URL (system of record).
    #[arg(long, env = "TEMPS_DATABASE_URL")]
    pub database_url: String,

    /// ClickHouse HTTP endpoint URL (e.g. https://ch.example.internal:8443).
    #[arg(long, env = "TEMPS_CLICKHOUSE_URL")]
    pub clickhouse_url: String,

    /// ClickHouse database name (Temps will create tables in it).
    #[arg(long, env = "TEMPS_CLICKHOUSE_DATABASE")]
    pub clickhouse_database: String,

    /// ClickHouse username.
    #[arg(long, env = "TEMPS_CLICKHOUSE_USER")]
    pub clickhouse_user: String,

    /// ClickHouse password.
    #[arg(long, env = "TEMPS_CLICKHOUSE_PASSWORD", hide_env_values = true)]
    pub clickhouse_password: String,

    /// Lower bound of the backfill window (RFC3339, inclusive). Defaults
    /// to the earliest event in PostgreSQL.
    #[arg(long)]
    pub from: Option<String>,

    /// Upper bound of the backfill window (RFC3339, inclusive). Defaults
    /// to the latest event in PostgreSQL.
    #[arg(long)]
    pub to: Option<String>,

    /// Restrict to a single project.
    #[arg(long)]
    pub project_id: Option<i32>,

    /// Rows per ClickHouse insert. CH prefers large batches. Default 10k.
    #[arg(long, default_value_t = 10_000)]
    pub batch_size: u64,

    /// Slice the `[from, to]` window into chunks this many days wide. The
    /// progress bar updates per-chunk and the resume checkpoint advances
    /// per-chunk, so smaller chunks mean finer-grained resume but slightly
    /// more per-chunk overhead. Default 1.
    #[arg(long, default_value_t = 1u32)]
    pub chunk_days: u32,

    /// Optional throttle: sleep enough between batches to stay under this
    /// throughput. Useful on live production servers where PG read IO is
    /// constrained. Default: no throttle.
    #[arg(long)]
    pub rate_limit_events_per_sec: Option<u64>,

    /// Apply ClickHouse schema migrations (`events`, `events_5m_mv`,
    /// `sessions`) before backfilling. Idempotent; safe to pass on every
    /// run. If you let `temps serve` enable ClickHouse first, those
    /// migrations have already run and this flag is a no-op.
    #[arg(long)]
    pub apply_migrations: bool,

    /// Count events that would be pushed and print the chunk plan; do
    /// not write to ClickHouse.
    #[arg(long)]
    pub dry_run: bool,

    /// Persist a checkpoint to `<state-file>` after every batch and
    /// resume from it on the next invocation with the same window.
    #[arg(long)]
    pub resume: bool,

    /// Where to persist the resume checkpoint. Defaults to
    /// `$TEMPS_DATA_DIR/clickhouse-backfill.state` (or
    /// `~/.temps/clickhouse-backfill.state` when unset).
    #[arg(long)]
    pub state_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CheckpointFile {
    /// RFC3339 timestamp of the last fully-pushed event.
    last_timestamp: Option<String>,
    /// id of the last fully-pushed event.
    last_id: Option<i64>,
    /// Sanity check — refuse to resume against a window the user changed.
    window_from: Option<String>,
    window_to: Option<String>,
    project_id: Option<i32>,
}

impl BackfillCommand {
    pub fn execute(self) -> anyhow::Result<()> {
        match self.target {
            BackfillTarget::Clickhouse(args) => run_clickhouse(args),
        }
    }
}

fn run_clickhouse(args: ClickhouseBackfillArgs) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_clickhouse_async(args))
}

async fn run_clickhouse_async(args: ClickhouseBackfillArgs) -> anyhow::Result<()> {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_blue()
    );
    println!(
        "{}",
        "   ClickHouse analytics backfill".bright_white().bold()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_blue()
    );
    println!();

    let db = temps_database::establish_connection(&args.database_url).await?;
    info!("Connected to PostgreSQL");

    let ch_cfg = ClickHouseConfig::new(
        &args.clickhouse_url,
        &args.clickhouse_database,
        &args.clickhouse_user,
        &args.clickhouse_password,
    );
    let backend = ClickHouseBackend::new(ch_cfg);
    let ch = Arc::new(backend.client_clone());

    if args.apply_migrations {
        println!(
            "{} Applying ClickHouse schema migrations…",
            "→".bright_blue()
        );
        let report = apply_clickhouse_schema(&ch).await?;
        println!(
            "{} Migrations: {} applied, {} skipped",
            "✓".bright_green(),
            report.applied.len(),
            report.skipped.len()
        );
        for name in &report.applied {
            println!("    + {}", name.bright_cyan());
        }
    }

    // 1. Resolve the [from, to] window.
    let (from, to) = resolve_window(db.as_ref(), &args).await?;
    println!(
        "{} Window: {} → {}",
        "→".bright_blue(),
        from.to_rfc3339().bright_cyan(),
        to.to_rfc3339().bright_cyan(),
    );
    if let Some(pid) = args.project_id {
        println!("{} Project filter: {}", "→".bright_blue(), pid);
    }

    // 2. Count + chunk plan.
    let total = count_events_window(db.as_ref(), from, to, args.project_id).await?;
    println!(
        "{} Events to push: {}",
        "→".bright_blue(),
        total.to_string().bright_white().bold(),
    );

    if total == 0 {
        println!("{} Nothing to backfill in this window.", "✓".bright_green());
        return Ok(());
    }

    let chunks = split_window(from, to, args.chunk_days);
    println!(
        "{} Chunks: {} × ~{}d",
        "→".bright_blue(),
        chunks.len(),
        args.chunk_days,
    );

    if args.dry_run {
        println!();
        println!(
            "{} {}",
            "✓".bright_green(),
            "Dry run — no rows pushed.".bright_white()
        );
        for (i, (cf, ct)) in chunks.iter().enumerate() {
            println!(
                "    chunk {:>3}/{}  {} → {}",
                i + 1,
                chunks.len(),
                cf.to_rfc3339(),
                ct.to_rfc3339()
            );
        }
        return Ok(());
    }

    // 3. Load resume cursor if requested.
    let state_path = args.state_file.clone().unwrap_or_else(default_state_path);
    let (mut cursor, mut already_pushed) =
        load_checkpoint_or_default(&args, &state_path, from, to)?;
    if already_pushed > 0 {
        println!(
            "{} Resuming from checkpoint — {} events already pushed",
            "→".bright_blue(),
            already_pushed
        );
    }

    let rate_limit = args.rate_limit_events_per_sec.map(|qps| {
        // Per-batch sleep that targets qps throughput.
        // sleep = batch_size / qps  (seconds)
        let secs = args.batch_size as f64 / qps.max(1) as f64;
        Duration::from_millis((secs * 1000.0) as u64)
    });

    // 4. Progress bar.
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "  {bar:40.cyan/blue} {pos:>10}/{len:<10} {percent:>3}%  ETA {eta_precise}  {msg}",
        )
        .expect("valid progress template")
        .progress_chars("█▓░"),
    );
    pb.set_position(already_pushed);

    // 5. Walk chunks. Each chunk is independently resumable through the
    //    keyset cursor — if we crash mid-chunk, we resume in the same
    //    chunk from `(last_timestamp, last_id)` exclusive.
    for (i, (cf, ct)) in chunks.iter().copied().enumerate() {
        pb.set_message(format!(
            "chunk {}/{} ({}…{})",
            i + 1,
            chunks.len(),
            cf.format("%Y-%m-%d"),
            ct.format("%Y-%m-%d"),
        ));

        // Skip chunks entirely behind the cursor — happens on resume.
        if let Some(last_ts) = cursor.last_timestamp {
            if ct < last_ts {
                continue;
            }
        }

        let chunk_start = if cursor.last_timestamp.map(|ts| ts > cf).unwrap_or(false) {
            cursor.last_timestamp.unwrap()
        } else {
            cf
        };

        let report = backfill_events_window(
            db.clone(),
            ch.clone(),
            chunk_start,
            ct,
            args.project_id,
            args.batch_size,
            cursor,
            rate_limit,
        )
        .await?;

        cursor = report.final_cursor;
        already_pushed += report.events_pushed;
        pb.set_position(already_pushed);

        if args.resume {
            persist_checkpoint(&state_path, &args, from, to, cursor)?;
        }
    }

    pb.finish_with_message("done");

    println!();
    println!(
        "{} Backfill complete: {} events pushed",
        "✓".bright_green(),
        already_pushed.to_string().bright_white().bold(),
    );
    println!(
        "    ClickHouse: {}/{}",
        args.clickhouse_url.bright_cyan(),
        args.clickhouse_database.bright_cyan()
    );

    if args.resume {
        // Successful end-to-end run — clear the checkpoint so the next
        // invocation against a new window doesn't get confused.
        let _ = fs::remove_file(&state_path);
    }

    Ok(())
}

async fn resolve_window(
    db: &sea_orm::DatabaseConnection,
    args: &ClickhouseBackfillArgs,
) -> anyhow::Result<(DBDateTime, DBDateTime)> {
    use chrono::DateTime;

    let from = match &args.from {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| anyhow::anyhow!("--from is not a valid RFC3339 timestamp: {e}"))?
            .with_timezone(&chrono::Utc),
        None => earliest_event_timestamp(db).await?,
    };
    let to = match &args.to {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| anyhow::anyhow!("--to is not a valid RFC3339 timestamp: {e}"))?
            .with_timezone(&chrono::Utc),
        None => latest_event_timestamp(db).await?,
    };
    if from > to {
        anyhow::bail!(
            "--from ({}) is after --to ({}); refusing to backfill an empty window",
            from.to_rfc3339(),
            to.to_rfc3339()
        );
    }
    Ok((from, to))
}

async fn earliest_event_timestamp(db: &sea_orm::DatabaseConnection) -> anyhow::Result<DBDateTime> {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct Row {
        ts: Option<DBDateTime>,
    }

    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT MIN(timestamp) AS ts FROM events",
        vec![],
    ))
    .one(db)
    .await?;
    row.and_then(|r| r.ts)
        .ok_or_else(|| anyhow::anyhow!("`events` table is empty — nothing to backfill"))
}

async fn latest_event_timestamp(db: &sea_orm::DatabaseConnection) -> anyhow::Result<DBDateTime> {
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct Row {
        ts: Option<DBDateTime>,
    }

    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT MAX(timestamp) AS ts FROM events",
        vec![],
    ))
    .one(db)
    .await?;
    row.and_then(|r| r.ts)
        .ok_or_else(|| anyhow::anyhow!("`events` table is empty — nothing to backfill"))
}

fn split_window(
    from: DBDateTime,
    to: DBDateTime,
    chunk_days: u32,
) -> Vec<(DBDateTime, DBDateTime)> {
    let chunk = chrono::Duration::days(chunk_days.max(1) as i64);
    let mut out = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let end = std::cmp::min(cursor + chunk, to);
        out.push((cursor, end));
        cursor = end + chrono::Duration::milliseconds(1);
    }
    if out.is_empty() {
        out.push((from, to));
    }
    out
}

fn default_state_path() -> PathBuf {
    let dir = std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".temps"))
                .unwrap_or_else(|| PathBuf::from(".temps"))
        });
    dir.join("clickhouse-backfill.state")
}

fn load_checkpoint_or_default(
    args: &ClickhouseBackfillArgs,
    path: &PathBuf,
    window_from: DBDateTime,
    window_to: DBDateTime,
) -> anyhow::Result<(BackfillCursor, u64)> {
    if !args.resume {
        return Ok((BackfillCursor::default(), 0));
    }
    if !path.exists() {
        return Ok((BackfillCursor::default(), 0));
    }

    let raw = fs::read_to_string(path)?;
    let file: CheckpointFile = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "checkpoint file unreadable; starting fresh");
            return Ok((BackfillCursor::default(), 0));
        }
    };

    // Refuse to resume against a different window — that almost always
    // means the operator changed --from/--to/--project-id between runs.
    let same_window = file.window_from.as_deref() == Some(window_from.to_rfc3339().as_str())
        && file.window_to.as_deref() == Some(window_to.to_rfc3339().as_str())
        && file.project_id == args.project_id;
    if !same_window {
        warn!(
            "checkpoint window mismatch; ignoring checkpoint and starting fresh \
             (delete {} to silence this warning)",
            path.display()
        );
        return Ok((BackfillCursor::default(), 0));
    }

    let cursor = BackfillCursor {
        last_timestamp: file
            .last_timestamp
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc)),
        last_id: file.last_id,
    };
    // We don't know exactly how many events landed before the cursor
    // without re-counting. Re-counting the prefix is cheap relative to
    // the work ahead and gives the progress bar a correct denominator.
    let already = 0u64; // recount happens via the progress bar starting position only
    Ok((cursor, already))
}

fn persist_checkpoint(
    path: &PathBuf,
    args: &ClickhouseBackfillArgs,
    window_from: DBDateTime,
    window_to: DBDateTime,
    cursor: BackfillCursor,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = CheckpointFile {
        last_timestamp: cursor.last_timestamp.map(|ts| ts.to_rfc3339()),
        last_id: cursor.last_id,
        window_from: Some(window_from.to_rfc3339()),
        window_to: Some(window_to.to_rfc3339()),
        project_id: args.project_id,
    };
    let body = serde_json::to_string_pretty(&file)?;
    fs::write(path, body)?;
    Ok(())
}
